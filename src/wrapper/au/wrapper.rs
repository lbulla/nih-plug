use atomic_float::AtomicF64;
use atomic_refcell::{AtomicRefCell, AtomicRefMut};
use crossbeam::atomic::AtomicCell;
use crossbeam::channel;
use crossbeam::channel::{Receiver, SendTimeoutError, Sender};
use crossbeam::queue::ArrayQueue;
use dispatch::Queue;
use objc2::rc::Retained;
use objc2_app_kit::NSView;
use objc2_foundation::NSThread;
use parking_lot::{Mutex, RwLock};
use std::any::Any;
use std::collections::hash_map::Keys;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::c_void;
use std::ptr::{fn_addr_eq, null_mut, slice_from_raw_parts};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use crate::event_loop::{BackgroundThread, EventLoop, MainThreadExecutor, TASK_QUEUE_CAPACITY};
use crate::midi::{MidiConfig, NoteEvent, PluginNoteEvent};
use crate::params::ParamMut;
use crate::prelude::{
    AsyncExecutor, AuPlugin, AudioIOLayout, AuxiliaryBuffers, BoolParam, BufferConfig, Editor,
    Param, ParamFlags, ParamPtr, Params, ParentWindowHandle, PluginState, ProcessMode,
    ProcessStatus, SmoothingStyle, TaskExecutor, Transport,
};
use crate::wrapper::au::au_types::{
    AuHostCallbackInfo, AuMidiOutputCallback, AuMidiOutputCallbackBlock,
    AuMidiOutputCallbackHandler, AuMidiOutputCallbackStruct, AuParamEvent, AuPreset, AudioUnit,
};
use crate::wrapper::au::context::{WrapperGuiContext, WrapperInitContext, WrapperProcessContext};
use crate::wrapper::au::editor::WrapperViewHolder;
use crate::wrapper::au::properties::{PropertyDispatcher, PropertyDispatcherImpl};
use crate::wrapper::au::scope::{
    InputElement, IoElement, IoElementImpl, IoScope, OutputElement, ShouldAllocate,
};
use crate::wrapper::au::util::ThreadWrapper;
use crate::wrapper::au::{au_sys, AuPropertyListenerProc, AuRenderCallback, NO_ERROR};
use crate::wrapper::state;
use crate::wrapper::util::buffer_management::{BufferManager, Buffers};
use crate::wrapper::util::hash_param_id;

// ---------- Constants / Types ---------- //

const DEFAULT_BUFFER_SIZE: u32 = 512;
const DEFAULT_SAMPLE_RATE: f32 = 48000.0;
const DEFAULT_LAST_RENDER_SAMPLE_TIME: au_sys::Float64 = -1.0;

const EVENT_CAPACITY: usize = 512;

pub(super) type EventRef<'a, P> = AtomicRefMut<'a, VecDeque<PluginNoteEvent<P>>>;

// ---------- Listener / Notifies ---------- //

struct PropertyListener {
    proc: AuPropertyListenerProc,
    data: ThreadWrapper<*mut c_void>,
}

struct RenderNotify {
    proc: AuRenderCallback,
    data: *mut c_void,
}

struct RenderNotifies(Vec<RenderNotify>);

impl RenderNotifies {
    fn call(
        &self,
        io_action_flags: &mut au_sys::AudioUnitRenderActionFlags,
        in_time_stamp: *const au_sys::AudioTimeStamp,
        in_output_bus_num: au_sys::UInt32,
        in_number_frames: au_sys::UInt32,
        io_data: *mut au_sys::AudioBufferList,
    ) {
        for notify in self.0.iter() {
            unsafe {
                (notify.proc)(
                    notify.data,
                    &raw mut *io_action_flags,
                    in_time_stamp,
                    in_output_bus_num,
                    in_number_frames,
                    io_data,
                );
            }
        }
    }
}

unsafe impl Send for RenderNotifies {}
unsafe impl Sync for RenderNotifies {}

// ---------- Params ---------- //

pub(super) struct WrapperParam {
    pub(super) id: String,
    pub(super) ptr: ParamPtr,
    pub(super) group_hash: u32,
}

// NOTE: `kAudioUnitProperty_BypassEffect` is a recommended property by `auval`.
//       So we implement it even without a dedicated bypass parameter.
enum BypassParam<'a> {
    Default(AtomicBool),
    Custom(&'a BoolParam),
}

const PARAM_EVENT_QUEUE_CAPACITY: usize = 2048;

pub(super) enum EditorParamEvent {
    BeginGesture {
        param_hash: u32,
    },
    SetValueFromEditor {
        param_hash: u32,
        param: ParamPtr,
        normalized_value: f32,
    },
    EndGesture {
        param_hash: u32,
    },
    NotifyEditor {
        param_hash: u32,
        normalized_value: f32,
    },
}

struct ScheduleParamImmediate {
    param_hash: u32,
    param: ParamPtr,
    value: f32,
}

// NOTE: Great type name.
type AuRamp = au_sys::AudioUnitParameterEvent__bindgen_ty_1__bindgen_ty_1;

struct ScheduleParamRamp {
    param_hash: u32,
    param: ParamPtr,
    duration: au_sys::UInt32,
    start_value: f32,
    end_value: f32,
    backup_smoothing_style: SmoothingStyle,
}

// TODO: `SmoothingStyle` with samples rather than ms.
// FIXME: Not very attractive to replace the smoother but I suppose it is still more efficient
//        than processing 1 sample buffers.
//        We could instead change the value only at the start and end of the ramp.
//        However, this is then not sample accurate anymore. Or we could add a separate smoother.
impl ScheduleParamRamp {
    fn new(param_hash: u32, param: ParamPtr, ramp: &AuRamp) -> Self {
        let duration = if ramp.startBufferOffset < 0 {
            -ramp.startBufferOffset as au_sys::UInt32
        } else {
            ramp.durationInFrames
        };

        Self {
            param_hash,
            param,
            duration,
            start_value: ramp.startValue,
            end_value: ramp.endValue,
            backup_smoothing_style: SmoothingStyle::None,
        }
    }

    fn init(&mut self, sample_rate: f32) -> f32 {
        match self.param {
            ParamPtr::EnumParam(param) => {
                let param = unsafe { &*param };

                let smoothed = &param.inner.smoothed;
                self.backup_smoothing_style = smoothed.style.as_ref().borrow().clone();

                *smoothed.style.as_ref().borrow_mut() =
                    SmoothingStyle::Linear(self.duration as f32 * 1000.0 / sample_rate);
                smoothed.reset(self.start_value as _);
                smoothed.set_target(sample_rate, self.end_value as _);
                param.preview_normalized(self.end_value as _)
            }
            ParamPtr::FloatParam(param) => {
                let param = unsafe { &*param };

                let smoothed = &param.smoothed;
                self.backup_smoothing_style = smoothed.style.as_ref().borrow().clone();

                *smoothed.style.as_ref().borrow_mut() =
                    SmoothingStyle::Linear(self.duration as f32 * 1000.0 / sample_rate);
                smoothed.reset(self.start_value);
                smoothed.set_target(sample_rate, self.end_value);
                self.end_value
            }
            ParamPtr::IntParam(param) => {
                let param = unsafe { &*param };

                let smoothed = &param.smoothed;
                self.backup_smoothing_style = smoothed.style.as_ref().borrow().clone();

                *smoothed.style.as_ref().borrow_mut() =
                    SmoothingStyle::Linear(self.duration as f32 * 1000.0 / sample_rate);
                smoothed.reset(self.start_value as _);
                smoothed.set_target(sample_rate, self.end_value as _);
                param.preview_normalized(self.end_value as _)
            }
            _ => unreachable!(),
        }
    }
}

impl Drop for ScheduleParamRamp {
    fn drop(&mut self) {
        match self.param {
            ParamPtr::EnumParam(param) => {
                let smoothed = unsafe { &(*param).inner.smoothed };
                *smoothed.style.as_ref().borrow_mut() = self.backup_smoothing_style.clone();
            }
            ParamPtr::FloatParam(param) => {
                let smoothed = unsafe { &(*param).smoothed };
                *smoothed.style.as_ref().borrow_mut() = self.backup_smoothing_style.clone();
            }
            ParamPtr::IntParam(param) => {
                let smoothed = unsafe { &(*param).smoothed };
                *smoothed.style.as_ref().borrow_mut() = self.backup_smoothing_style.clone();
            }
            _ => unreachable!(),
        }
    }
}

enum ScheduledParamEvent {
    Immediate(ScheduleParamImmediate),
    Ramp(ScheduleParamRamp),
}

// ---------- Task ---------- //

pub(super) enum Task<P: AuPlugin> {
    PluginTask(P::BackgroundTask),

    // NOTE: Notify the editor about parameter changes.
    ParameterValueChanged(u32, f32),
    ParameterValuesChanged,

    // NOTE: A `NSView` must be resized on the main thread.
    RequestResize,
}

// ---------- Wrapper ---------- //

pub(super) struct Wrapper<P: AuPlugin> {
    unit: AudioUnit,

    this: AtomicRefCell<Weak<Wrapper<P>>>,
    plugin: Mutex<P>,

    params: Arc<dyn Params>,
    param_hash_to_param: HashMap<u32, WrapperParam>,
    param_ptr_to_hash: HashMap<ParamPtr, u32>,
    param_id_to_ptr: HashMap<String, ParamPtr>,
    group_hash_to_group: HashMap<u32, String>,
    bypass_param: BypassParam<'static>,

    editor_param_event_queue: ArrayQueue<EditorParamEvent>,
    au_param_event: AtomicRefCell<AuParamEvent>,
    // NOTE: Key: Buffer offset.
    scheduled_params: AtomicRefCell<BTreeMap<au_sys::UInt32, Vec<ScheduledParamEvent>>>,

    property_listeners: AtomicRefCell<HashMap<au_sys::AudioUnitPropertyID, Vec<PropertyListener>>>,
    render_notifies: AtomicRefCell<RenderNotifies>,

    pub(super) task_executor: Mutex<TaskExecutor<P>>,
    tasks: ArrayQueue<Task<P>>,
    background_thread: AtomicRefCell<Option<BackgroundThread<Task<P>, Self>>>,

    editor: AtomicRefCell<Option<Mutex<Box<dyn Editor>>>>,
    wrapper_view_holder: AtomicRefCell<WrapperViewHolder>,

    audio_io_layout: AtomicRefCell<AudioIOLayout>,
    buffer_config: AtomicRefCell<BufferConfig>,
    buffer_manager: AtomicRefCell<BufferManager>,
    latency_samples: AtomicU32,

    // TODO: Other scopes.
    pub(super) input_scope: RwLock<IoScope<InputElement>>,
    pub(super) output_scope: RwLock<IoScope<OutputElement>>,

    input_events: AtomicRefCell<VecDeque<PluginNoteEvent<P>>>,
    output_events: AtomicRefCell<VecDeque<PluginNoteEvent<P>>>,
    midi_output_callback: AtomicRefCell<Option<AuMidiOutputCallback>>,

    tail_seconds: AtomicCell<au_sys::Float64>,
    last_render_error: AtomicCell<au_sys::OSStatus>,
    last_render_sample_time: AtomicF64,
    host_callback_info: AtomicRefCell<Option<AuHostCallbackInfo>>,
    initialized: AtomicBool,

    updated_state_sender: Sender<PluginState>,
    updated_state_receiver: Receiver<PluginState>,
    preset: AuPreset,
}

impl<P: AuPlugin> Wrapper<P> {
    pub(super) fn new(unit: au_sys::AudioUnit) -> Arc<Self> {
        let mut plugin = P::default();
        let task_executor = plugin.task_executor();

        let params = plugin.params();
        let params_and_hashes: Vec<_> = params
            .param_map()
            .into_iter()
            .map(|(id, ptr, group)| {
                let param_hash = hash_param_id(&id);
                let group_hash = hash_param_id(&group);
                (id, param_hash, ptr, group, group_hash)
            })
            .collect();
        let param_hash_to_param = params_and_hashes
            .iter()
            .map(|(id, param_hash, ptr, _, group_hash)| {
                (
                    *param_hash,
                    WrapperParam {
                        id: id.clone(),
                        ptr: *ptr,
                        group_hash: *group_hash,
                    },
                )
            })
            .collect();
        let param_ptr_to_hash = params_and_hashes
            .iter()
            .map(|(_, param_hash, ptr, _, _)| (*ptr, *param_hash))
            .collect();
        let param_id_to_ptr = params_and_hashes
            .iter()
            .map(|(id, _, ptr, _, _)| (id.clone(), *ptr))
            .collect();
        let group_hash_to_group = params_and_hashes
            .iter()
            .map(|(_, _, _, group, group_hash)| (*group_hash, group.clone()))
            .collect();
        let bypass_param = params_and_hashes
            .iter()
            .find_map(|(_, _, ptr, _, _)| unsafe {
                if ptr.flags().contains(ParamFlags::BYPASS) {
                    Some(match ptr {
                        ParamPtr::BoolParam(param) => BypassParam::Custom(&**param),
                        _ => unreachable!(),
                    })
                } else {
                    None
                }
            });
        let bypass_param = bypass_param.unwrap_or(BypassParam::Default(AtomicBool::new(false)));

        let au_param_event = AuParamEvent::new(au_sys::AudioUnitEvent {
            mEventType: 0,
            mArgument: au_sys::AudioUnitEvent__bindgen_ty_1 {
                mParameter: au_sys::AudioUnitParameter {
                    mAudioUnit: unit,
                    mParameterID: 0,
                    mScope: au_sys::kAudioUnitScope_Global,
                    mElement: 0,
                },
            },
        });

        let audio_io_layout;
        let mut input_scope = IoScope::new();
        let mut output_scope = IoScope::new();

        if P::AUDIO_IO_LAYOUTS.is_empty() {
            audio_io_layout = AudioIOLayout::default();
        } else {
            audio_io_layout = P::AUDIO_IO_LAYOUTS[0];

            if let Some(main_input_channels) = audio_io_layout.main_input_channels {
                input_scope.elements.push(InputElement::new(
                    audio_io_layout.main_input_name(),
                    DEFAULT_SAMPLE_RATE as _,
                    main_input_channels,
                ));
            }
            if let Some(main_output_channels) = audio_io_layout.main_output_channels {
                output_scope.elements.push(OutputElement::new(
                    audio_io_layout.main_output_name(),
                    DEFAULT_SAMPLE_RATE as _,
                    main_output_channels,
                ));
            }

            for i in 0..audio_io_layout.aux_input_ports.len() {
                input_scope.elements.push(InputElement::new(
                    audio_io_layout.aux_input_name(i).unwrap(),
                    DEFAULT_SAMPLE_RATE as _,
                    audio_io_layout.aux_input_ports[i],
                ));
            }
            for i in 0..audio_io_layout.aux_output_ports.len() {
                output_scope.elements.push(OutputElement::new(
                    audio_io_layout.aux_output_name(i).unwrap(),
                    DEFAULT_SAMPLE_RATE as _,
                    audio_io_layout.aux_output_ports[i],
                ));
            }

            for element in output_scope.elements.iter_mut().skip(1) {
                element.set_should_allocate(ShouldAllocate::Force);
            }
        }

        let (updated_state_sender, updated_state_receiver) = channel::bounded(0);

        let wrapper = Arc::new(Self {
            unit: AudioUnit::new(unit),

            this: AtomicRefCell::new(Weak::new()),
            plugin: Mutex::new(plugin),

            params,
            param_hash_to_param,
            param_ptr_to_hash,
            param_id_to_ptr,
            group_hash_to_group,
            bypass_param,

            editor_param_event_queue: ArrayQueue::new(PARAM_EVENT_QUEUE_CAPACITY),
            au_param_event: AtomicRefCell::new(au_param_event),
            scheduled_params: AtomicRefCell::new(BTreeMap::new()),

            property_listeners: AtomicRefCell::new(HashMap::new()),
            render_notifies: AtomicRefCell::new(RenderNotifies(Vec::new())),

            task_executor: Mutex::new(task_executor),
            tasks: ArrayQueue::new(TASK_QUEUE_CAPACITY),
            background_thread: AtomicRefCell::new(None),

            editor: AtomicRefCell::new(None),
            wrapper_view_holder: AtomicRefCell::new(WrapperViewHolder::default()),

            audio_io_layout: AtomicRefCell::new(audio_io_layout),
            buffer_config: AtomicRefCell::new(BufferConfig {
                sample_rate: DEFAULT_SAMPLE_RATE,
                min_buffer_size: None,
                max_buffer_size: DEFAULT_BUFFER_SIZE,
                process_mode: ProcessMode::Realtime,
            }),
            buffer_manager: AtomicRefCell::new(BufferManager::for_audio_io_layout(
                0,
                audio_io_layout,
            )),
            latency_samples: AtomicU32::new(0),

            input_scope: RwLock::new(input_scope),
            output_scope: RwLock::new(output_scope),
            midi_output_callback: AtomicRefCell::new(None),

            input_events: AtomicRefCell::new(VecDeque::with_capacity(EVENT_CAPACITY)),
            output_events: AtomicRefCell::new(VecDeque::with_capacity(EVENT_CAPACITY)),

            tail_seconds: AtomicCell::new(0.0),
            last_render_error: AtomicCell::new(NO_ERROR),
            last_render_sample_time: AtomicF64::new(DEFAULT_LAST_RENDER_SAMPLE_TIME),
            host_callback_info: AtomicRefCell::new(None),
            initialized: AtomicBool::new(false),

            updated_state_sender,
            updated_state_receiver,
            preset: AuPreset::default(),
        });

        *wrapper.this.borrow_mut() = Arc::downgrade(&wrapper);

        *wrapper.background_thread.borrow_mut() =
            Some(BackgroundThread::get_or_create(Arc::downgrade(&wrapper)));

        *wrapper.editor.borrow_mut() = wrapper
            .plugin
            .lock()
            .editor(AsyncExecutor {
                execute_background: Arc::new({
                    let wrapper = wrapper.clone();

                    move |task| {
                        let task_posted = wrapper.schedule_background(Task::PluginTask(task));
                        nih_debug_assert!(task_posted, "The task queue is full, dropping task...");
                    }
                }),
                execute_gui: Arc::new({
                    let wrapper = wrapper.clone();

                    move |task| {
                        let task_posted = wrapper.schedule_gui(Task::PluginTask(task));
                        nih_debug_assert!(task_posted, "The task queue is full, dropping task...");
                    }
                }),
            })
            .map(Mutex::new);

        wrapper
    }

    // ---------- Getter ---------- //

    pub(super) fn unit(&self) -> au_sys::AudioUnit {
        self.unit.get()
    }

    pub(super) fn as_arc(&self) -> Arc<Self> {
        self.this.borrow().upgrade().unwrap()
    }

    pub(super) fn initialized(&self) -> bool {
        self.initialized.load(Ordering::SeqCst)
    }

    // ---------- Editor ---------- //

    fn make_gui_context(&self) -> Arc<WrapperGuiContext<P>> {
        Arc::new(WrapperGuiContext {
            wrapper: self.as_arc(),
        })
    }

    pub(super) fn has_editor(&self) -> bool {
        self.editor.borrow().is_some()
    }

    #[must_use]
    pub(super) fn spawn_editor(&self, view: &Retained<NSView>) -> Box<dyn Any + Send> {
        let editor = self.editor.borrow();
        let editor = editor
            .as_ref()
            .expect("`spawn_editor` called without an editor")
            .lock();

        let parent_handle = ParentWindowHandle::AppKitNsView(Retained::as_ptr(view) as _);
        let editor_handle = editor.spawn(parent_handle, self.make_gui_context());
        self.wrapper_view_holder
            .borrow_mut()
            .init(view, editor.size());
        editor_handle
    }

    pub(super) fn request_resize(&self) -> bool {
        if self.wrapper_view_holder.borrow().has_view() {
            let task_posted = self.schedule_gui(Task::RequestResize);
            nih_debug_assert!(task_posted, "The task queue is full, dropping task...");
            true
        } else {
            false
        }
    }

    // ---------- Setup ---------- //

    pub(super) fn sample_rate(&self) -> f32 {
        self.buffer_config.borrow().sample_rate
    }

    pub(super) fn set_sample_rate(&self, sample_rate: f32) {
        let mut buffer_config = self.buffer_config.borrow_mut();
        buffer_config.sample_rate = sample_rate;
    }

    pub(super) fn buffer_size(&self) -> u32 {
        self.buffer_config.borrow().max_buffer_size
    }

    pub(super) fn set_buffer_size(&self, buffer_size: u32) {
        let mut buffer_config = self.buffer_config.borrow_mut();
        buffer_config.max_buffer_size = buffer_size;
    }

    pub(super) fn latency_seconds(&self) -> au_sys::Float64 {
        let sample_rate = self.sample_rate() as au_sys::Float64;
        self.latency_samples.load(Ordering::SeqCst) as au_sys::Float64 / sample_rate
    }

    pub(super) fn set_latency_samples(&self, samples: u32) {
        let old_samples = self.latency_samples.swap(samples, Ordering::SeqCst);
        if old_samples != samples {
            self.call_property_listeners(
                au_sys::kAudioUnitProperty_Latency,
                au_sys::kAudioUnitScope_Global,
                0,
            );
        }
    }

    fn make_init_context(&self) -> WrapperInitContext<'_, P> {
        WrapperInitContext { wrapper: self }
    }

    pub(super) fn init(&mut self) -> au_sys::OSStatus {
        let input_scope = self.input_scope.read();
        let output_scope = self.output_scope.read();

        let init_impl = |audio_io_layout: AudioIOLayout| -> au_sys::OSStatus {
            let mut plugin = self.plugin.lock();

            *self.audio_io_layout.borrow_mut() = audio_io_layout;
            let buffer_config = self.buffer_config.borrow();
            let mut init_context = self.make_init_context();

            let success = plugin.initialize(&audio_io_layout, &buffer_config, &mut init_context);
            if success {
                plugin.reset();

                for (_, wrapper_param) in self.param_hash_to_param.iter() {
                    unsafe {
                        wrapper_param
                            .ptr
                            .update_smoother(buffer_config.sample_rate, true)
                    };
                }

                *self.buffer_manager.borrow_mut() = BufferManager::for_audio_io_layout(
                    buffer_config.max_buffer_size as _,
                    audio_io_layout,
                );

                for input_element in input_scope.elements.iter() {
                    input_element.resize_buffer(buffer_config.max_buffer_size);
                }
                for output_element in output_scope.elements.iter() {
                    output_element.resize_buffer(buffer_config.max_buffer_size);
                }

                if let Some(main_sample_rate) = output_scope
                    .elements
                    .get(0)
                    .map(|output_element| output_element.stream_format().mSampleRate)
                {
                    for input_element in input_scope.elements.iter() {
                        if !input_element.init_converter(main_sample_rate, false) {
                            return au_sys::kAudioUnitErr_FailedInitialization;
                        }
                    }
                    for output_element in output_scope.elements.iter().skip(1) {
                        if !output_element.init_converter(main_sample_rate, true) {
                            return au_sys::kAudioUnitErr_FailedInitialization;
                        }
                    }
                }

                self.initialized.store(true, Ordering::SeqCst);
                return NO_ERROR;
            } else {
                return au_sys::kAudioUnitErr_FailedInitialization;
            }
        };

        if P::AU_CHANNEL_LAYOUTS.is_empty() {
            return init_impl(AudioIOLayout::default());
        } else {
            for (i, au_layout) in P::AU_CHANNEL_LAYOUTS.iter().enumerate() {
                let mut layout_is_valid = true;

                for (j, config) in au_layout.iter().enumerate() {
                    if let Some(input_element) = input_scope.element(j as _) {
                        if config.num_inputs != input_element.num_channels() {
                            layout_is_valid = false;
                            break;
                        }
                    } else if config.num_inputs != 0 {
                        layout_is_valid = false;
                        break;
                    }

                    if let Some(output_element) = output_scope.element(j as _) {
                        if config.num_outputs != output_element.num_channels() {
                            layout_is_valid = false;
                            break;
                        }
                    } else if config.num_outputs != 0 {
                        layout_is_valid = false;
                        break;
                    }
                }

                // TODO: Remove unused elements if there are any?
                if layout_is_valid {
                    return init_impl(P::AUDIO_IO_LAYOUTS[i]);
                };
            }
        }

        au_sys::kAudioUnitErr_FailedInitialization
    }

    pub(super) fn uninit(&mut self) -> au_sys::OSStatus {
        self.plugin.lock().deactivate();
        self.initialized.store(false, Ordering::SeqCst);
        NO_ERROR
    }

    // ---------- Scopes ---------- //

    pub(super) fn num_elements(&self, in_scope: au_sys::AudioUnitScope) -> Option<usize> {
        match in_scope {
            au_sys::kAudioUnitScope_Global => Some(1), // NOTE: AU default (fixed).
            au_sys::kAudioUnitScope_Input => Some(self.input_scope.read().elements.len()),
            au_sys::kAudioUnitScope_Output => Some(self.output_scope.read().elements.len()),
            _ => None,
        }
    }

    pub(super) fn map_element<F>(
        &self,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        mut f: F,
    ) -> au_sys::OSStatus
    where
        F: FnMut(&IoElement) -> au_sys::OSStatus,
    {
        match in_scope {
            au_sys::kAudioUnitScope_Input => self
                .input_scope
                .read()
                .map_element(in_element, |element| f(element.base())),
            au_sys::kAudioUnitScope_Output => self
                .output_scope
                .read()
                .map_element(in_element, |element| f(element.base())),
            _ => au_sys::kAudioUnitErr_InvalidScope,
        }
    }

    pub(super) fn map_element_mut<F>(
        &mut self,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        mut f: F,
    ) -> au_sys::OSStatus
    where
        F: FnMut(&mut IoElement) -> au_sys::OSStatus,
    {
        match in_scope {
            au_sys::kAudioUnitScope_Input => self
                .input_scope
                .write()
                .map_element_mut(in_element, |element| f(element.base_mut())),
            au_sys::kAudioUnitScope_Output => self
                .output_scope
                .write()
                .map_element_mut(in_element, |element| f(element.base_mut())),
            _ => au_sys::kAudioUnitErr_InvalidScope,
        }
    }

    // ---------- Properties ---------- //

    pub(super) fn get_property_info(
        &self,
        in_id: au_sys::AudioUnitPropertyID,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        out_data_size: *mut au_sys::UInt32,
        out_writable: *mut au_sys::Boolean,
    ) -> au_sys::OSStatus {
        PropertyDispatcher::info(
            in_id,
            self,
            in_scope,
            in_element,
            out_data_size,
            out_writable,
        )
    }

    pub(super) fn get_property(
        &self,
        in_id: au_sys::AudioUnitPropertyID,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        out_data: *mut c_void,
        io_data_size: *mut au_sys::UInt32,
    ) -> au_sys::OSStatus {
        PropertyDispatcher::get(in_id, self, in_scope, in_element, out_data, io_data_size)
    }

    pub(super) fn set_property(
        &mut self,
        in_id: au_sys::AudioUnitPropertyID,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        in_data: *const c_void,
        in_data_size: au_sys::UInt32,
    ) -> au_sys::OSStatus {
        PropertyDispatcher::set(in_id, self, in_scope, in_element, in_data, in_data_size)
    }

    pub(super) fn add_property_listener(
        &self,
        in_id: au_sys::AudioUnitPropertyID,
        in_proc: AuPropertyListenerProc,
        in_proc_data: *mut c_void,
    ) -> au_sys::OSStatus {
        self.property_listeners
            .borrow_mut()
            .entry(in_id)
            .or_default()
            .push(PropertyListener {
                proc: in_proc,
                data: ThreadWrapper::new(in_proc_data),
            });
        NO_ERROR
    }

    pub(super) fn remove_property_listener(
        &self,
        in_id: au_sys::AudioUnitPropertyID,
        in_proc: AuPropertyListenerProc,
        in_proc_data: *mut c_void,
    ) -> au_sys::OSStatus {
        if let Some(listeners) = self.property_listeners.borrow_mut().get_mut(&in_id) {
            listeners.retain(|listener| {
                !fn_addr_eq(listener.proc, in_proc) && listener.data.get() != in_proc_data
            });
        }
        NO_ERROR
    }

    pub(super) fn call_property_listeners(
        &self,
        in_id: au_sys::AudioUnitPropertyID,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
    ) {
        if let Some(listeners) = self.property_listeners.borrow().get(&in_id) {
            for listener in listeners {
                unsafe {
                    (listener.proc)(
                        listener.data.get(),
                        self.unit(),
                        in_id,
                        in_scope,
                        in_element,
                    );
                }
            }
        }
    }

    // ---------- Parameters ---------- //

    pub(super) fn param_hashes(&self) -> Keys<'_, u32, WrapperParam> {
        self.param_hash_to_param.keys()
    }

    pub(super) fn param_hash_to_param(&self, hash: &u32) -> Option<&WrapperParam> {
        self.param_hash_to_param.get(hash)
    }

    pub(super) fn param_ptr_to_hash(&self, ptr: &ParamPtr) -> Option<&u32> {
        self.param_ptr_to_hash.get(ptr)
    }

    pub(super) fn group_hash_to_group(&self, hash: &u32) -> Option<&String> {
        self.group_hash_to_group.get(hash)
    }

    pub(super) fn bypassed(&self) -> bool {
        match &self.bypass_param {
            BypassParam::Default(bypassed) => bypassed.load(Ordering::SeqCst),
            BypassParam::Custom(param) => param.value(),
        }
    }

    pub(super) fn set_bypassed(&self, bypass: bool) {
        match &self.bypass_param {
            BypassParam::Default(bypassed) => bypassed.store(bypass, Ordering::SeqCst),
            BypassParam::Custom(param) => {
                param.set_plain_value(bypass);
            }
        }
    }

    fn post_editor_param_event(&self, event: EditorParamEvent) {
        let event_posted = self.editor_param_event_queue.push(event).is_ok();
        nih_debug_assert!(
            event_posted,
            "Parameter event queue is full, parameter change will not be sent to the host"
        );
    }

    fn handle_editor_param_event(&self, event: EditorParamEvent, au_event: &mut AuParamEvent) {
        let success = match event {
            EditorParamEvent::BeginGesture { param_hash } => {
                au_event.send(
                    au_sys::kAudioUnitEvent_BeginParameterChangeGesture,
                    param_hash,
                ) == NO_ERROR
            }
            EditorParamEvent::SetValueFromEditor {
                param_hash,
                param,
                normalized_value,
            } => {
                let param_changed = self.set_param_impl(
                    param_hash,
                    param,
                    normalized_value,
                    true,
                    self.sample_rate(),
                    false,
                );
                if param_changed {
                    let success = au_event
                        .send(au_sys::kAudioUnitEvent_ParameterValueChange, param_hash)
                        == NO_ERROR;
                    unsafe {
                        if success && param.flags().contains(ParamFlags::BYPASS) {
                            self.call_property_listeners(
                                au_sys::kAudioUnitProperty_BypassEffect,
                                au_sys::kAudioUnitScope_Global,
                                0,
                            );
                        }
                    }
                    success
                } else {
                    true
                }
            }
            EditorParamEvent::EndGesture { param_hash } => {
                au_event.send(
                    au_sys::kAudioUnitEvent_EndParameterChangeGesture,
                    param_hash,
                ) == NO_ERROR
            }
            EditorParamEvent::NotifyEditor {
                param_hash,
                normalized_value,
            } => self.schedule_gui(Task::ParameterValueChanged(param_hash, normalized_value)),
        };
        nih_debug_assert!(success, "Failed to handle `EditorParamEvent`");
    }

    pub(super) fn post_editor_param_event_gui(&self, event: EditorParamEvent) {
        if self.is_rendering() {
            self.post_editor_param_event(event);
        } else {
            let mut au_event = self.au_param_event.borrow_mut();
            self.handle_editor_param_event(event, &mut au_event);
        }
    }

    pub(super) fn get_param_impl(&self, param: ParamPtr) -> au_sys::AudioUnitParameterValue {
        unsafe { param.unmodulated_normalized_value() * param.step_count().unwrap_or(1) as f32 }
    }

    pub(super) fn set_param_impl(
        &self,
        param_hash: u32,
        param: ParamPtr,
        value: f32,
        value_is_normalized: bool,
        sample_rate: f32,
        notify_editor: bool,
    ) -> bool {
        let normalized_value;
        unsafe {
            if value_is_normalized {
                if !param.set_normalized_value(value) {
                    return false;
                }
                param.update_smoother(sample_rate, false);
                normalized_value = value;
            } else {
                normalized_value = value / param.step_count().unwrap_or(1) as f32;
                if !param.set_normalized_value(normalized_value) {
                    return false;
                }
                param.update_smoother(sample_rate, false);
            }
        }

        if notify_editor {
            let task_posted =
                self.schedule_gui(Task::ParameterValueChanged(param_hash, normalized_value));
            nih_debug_assert!(task_posted, "The task queue is full, dropping task...");
        }
        true
    }

    pub(super) fn get_param(
        &self,
        in_id: au_sys::AudioUnitParameterID,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        out_value: *mut au_sys::AudioUnitParameterValue,
    ) -> au_sys::OSStatus {
        if let Some(wrapper_param) = self.param_hash_to_param(&in_id) {
            unsafe {
                *out_value = self.get_param_impl(wrapper_param.ptr);
            }
            NO_ERROR
        } else {
            au_sys::kAudioUnitErr_InvalidParameter
        }
    }

    pub(super) fn set_param(
        &self,
        in_id: au_sys::AudioUnitParameterID,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        in_value: au_sys::AudioUnitParameterValue,
        _in_buffer_offset_in_frames: au_sys::UInt32,
    ) -> au_sys::OSStatus {
        if let Some(wrapper_param) = self.param_hash_to_param(&in_id) {
            self.set_param_impl(
                in_id,
                wrapper_param.ptr,
                in_value,
                false,
                self.sample_rate(),
                true,
            );
            NO_ERROR
        } else {
            au_sys::kAudioUnitErr_InvalidParameter
        }
    }

    pub(super) unsafe fn schedule_params(
        &self,
        in_param_events: *const au_sys::AudioUnitParameterEvent,
        in_num_param_events: au_sys::UInt32,
    ) -> au_sys::OSStatus {
        let buffer_config = self.buffer_config.borrow();

        if P::SAMPLE_ACCURATE_AUTOMATION {
            let mut scheduled_params = self.scheduled_params.borrow_mut();

            for i in 0..in_num_param_events {
                let event = &*in_param_events.add(i as _);

                if let Some(wrapper_param) = self.param_hash_to_param(&event.parameter) {
                    if event.eventType == au_sys::kParameterEvent_Immediate {
                        let immediate = &event.eventValues.immediate;
                        if immediate.bufferOffset >= buffer_config.max_buffer_size {
                            return au_sys::kAudioUnitErr_TooManyFramesToProcess;
                        } else if immediate.bufferOffset == 0 {
                            self.set_param_impl(
                                event.parameter,
                                wrapper_param.ptr,
                                immediate.value,
                                false,
                                buffer_config.sample_rate,
                                true,
                            );
                        } else {
                            scheduled_params
                                .entry(immediate.bufferOffset)
                                .or_default()
                                .push(ScheduledParamEvent::Immediate(ScheduleParamImmediate {
                                    param_hash: event.parameter,
                                    param: wrapper_param.ptr,
                                    value: immediate.value,
                                }));
                        }
                    } else {
                        // FIXME: This is untested because I cannot find any host which sends these
                        //        kind of events. Logic Pro for instance, calls `set_param` and then
                        //        `render` with adjusted buffer sizes. Anyway, `auval` seems to be
                        //        happy with this implementation.
                        match wrapper_param.ptr {
                            ParamPtr::EnumParam(_)
                            | ParamPtr::FloatParam(_)
                            | ParamPtr::IntParam(_) => {
                                // NOTE: Negative `startBufferOffset` => duration to use.
                                //       `durationInFrames` stays the same for consecutive events
                                //       since this is the total duration of the ramp.
                                let ramp = &event.eventValues.ramp;
                                if ramp.startBufferOffset.abs() as u32
                                    >= buffer_config.max_buffer_size
                                {
                                    return au_sys::kAudioUnitErr_TooManyFramesToProcess;
                                }

                                scheduled_params
                                    .entry(ramp.startBufferOffset.max(0) as _)
                                    .or_default()
                                    .push(ScheduledParamEvent::Ramp(ScheduleParamRamp::new(
                                        event.parameter,
                                        wrapper_param.ptr,
                                        ramp,
                                    )));
                            }
                            _ => return au_sys::kAudioUnitErr_InvalidParameter,
                        }
                    }
                } else {
                    return au_sys::kAudioUnitErr_InvalidParameter;
                }
            }
        } else {
            for i in 0..in_num_param_events {
                let event = &*in_param_events.add(i as _);

                if let Some(wrapper_param) = self.param_hash_to_param(&event.parameter) {
                    let immediate = &event.eventValues.immediate;
                    self.set_param_impl(
                        event.parameter,
                        wrapper_param.ptr,
                        immediate.value,
                        false,
                        buffer_config.sample_rate,
                        true,
                    );
                } else {
                    return au_sys::kAudioUnitErr_InvalidParameter;
                }
            }
        }

        NO_ERROR
    }

    // ---------- MIDI ---------- //

    pub(super) fn set_midi_output_callback_block(&self, block: au_sys::AUMIDIEventListBlock) {
        *self.midi_output_callback.borrow_mut() = Some(AuMidiOutputCallback::Block(
            AuMidiOutputCallbackBlock::new(block),
        ));
    }

    pub(super) fn set_midi_output_callback_struct(
        &self,
        struct_: au_sys::AUMIDIOutputCallbackStruct,
    ) {
        let mut midi_output_callback = self.midi_output_callback.borrow_mut();
        if let Some(midi_output_callback) = midi_output_callback.as_ref() {
            match midi_output_callback {
                // NOTE: Prefer the block because the struct is deprecated.
                AuMidiOutputCallback::Block(_) => {
                    return;
                }
                _ => (),
            }
        }
        *midi_output_callback = Some(AuMidiOutputCallback::Struct(
            AuMidiOutputCallbackStruct::new(struct_),
        ));
    }

    unsafe fn handle_midi_output(&self, audio_time_stamp: *const au_sys::AudioTimeStamp) {
        let output_events = self.output_events.borrow_mut();
        if output_events.is_empty() {
            return;
        }

        if let Some(midi_output_callback) = self.midi_output_callback.borrow().as_ref() {
            match midi_output_callback {
                AuMidiOutputCallback::Block(block) => {
                    block.handle_events::<P>(audio_time_stamp, output_events);
                }
                AuMidiOutputCallback::Struct(struct_) => {
                    struct_.handle_events::<P>(audio_time_stamp, output_events);
                }
            }
        }
    }

    pub(super) fn midi_event_impl(
        &self,
        input_events: &mut EventRef<P>,
        data: &[u8],
        in_offset_sample_frame: au_sys::UInt32,
    ) -> au_sys::OSStatus {
        match NoteEvent::<P::SysExMessage>::from_midi(in_offset_sample_frame, data) {
            Ok(
                note_event @ (NoteEvent::NoteOn { .. }
                | NoteEvent::NoteOff { .. }
                | NoteEvent::PolyPressure { .. }),
            ) if P::MIDI_INPUT >= MidiConfig::Basic => {
                input_events.push_back(note_event);
            }
            Ok(note_event) if P::MIDI_INPUT >= MidiConfig::MidiCCs => {
                input_events.push_back(note_event);
            }
            Ok(_) => (),
            Err(n) => nih_debug_assert_failure!("Unhandled MIDI message type {}", n),
        };
        NO_ERROR
    }

    pub(super) fn midi_event(
        &self,
        in_status: au_sys::UInt32,
        in_data1: au_sys::UInt32,
        in_data2: au_sys::UInt32,
        in_offset_sample_frame: au_sys::UInt32,
    ) -> au_sys::OSStatus {
        let mut input_events = self.input_events.borrow_mut();
        self.midi_event_impl(
            &mut input_events,
            &[in_status as _, in_data1 as _, in_data2 as _],
            in_offset_sample_frame,
        )
    }

    pub(super) fn sys_ex(
        &self,
        in_data: *const au_sys::UInt8,
        in_length: au_sys::UInt32,
    ) -> au_sys::OSStatus {
        let mut input_events = self.input_events.borrow_mut();
        let data = unsafe { &*slice_from_raw_parts(in_data, in_length as _) };
        self.midi_event_impl(&mut input_events, data, 0)
    }

    // NOTE: See `AuMidiOutputCallbackBlock` for information regarding the list format.
    pub(super) fn midi_event_list(
        &self,
        in_offset_sample_frame: au_sys::UInt32,
        in_event_list: *const au_sys::MIDIEventList,
    ) -> au_sys::OSStatus {
        let mut input_events = self.input_events.borrow_mut();
        unsafe {
            let in_event_list = &*in_event_list;
            for i in 0..in_event_list.numPackets {
                let packet = &*in_event_list.packet.as_ptr().add(i as _);
                for j in 0..packet.wordCount {
                    let midi = au_sys::UInt32::to_be_bytes(packet.words[j as usize]);
                    self.midi_event_impl(
                        &mut input_events,
                        &[midi[1], midi[2], midi[3]],
                        in_offset_sample_frame,
                    );
                }
            }
        }
        NO_ERROR
    }

    // ---------- Render ---------- //

    pub(super) fn add_render_notify(
        &self,
        in_proc: AuRenderCallback,
        in_proc_data: *mut c_void,
    ) -> au_sys::OSStatus {
        self.render_notifies.borrow_mut().0.push(RenderNotify {
            proc: in_proc,
            data: in_proc_data,
        });
        NO_ERROR
    }

    pub(super) fn remove_render_notify(
        &self,
        in_proc: AuRenderCallback,
        in_proc_data: *mut c_void,
    ) -> au_sys::OSStatus {
        self.render_notifies
            .borrow_mut()
            .0
            .retain(|notify| !fn_addr_eq(notify.proc, in_proc) && notify.data != in_proc_data);
        NO_ERROR
    }

    // TODO: PropertyChanged?
    pub(super) fn tail_seconds(&self) -> au_sys::Float64 {
        self.tail_seconds.load()
    }

    // TODO: PropertyChanged?
    pub(super) fn last_render_error(&self) -> au_sys::OSStatus {
        let error = self.last_render_error.load();
        self.last_render_error.store(NO_ERROR);
        error
    }

    fn set_last_render_error(&self, os_status: au_sys::OSStatus) {
        if os_status != NO_ERROR && self.last_render_error.load() == NO_ERROR {
            self.last_render_error.store(os_status);
            self.call_property_listeners(
                au_sys::kAudioUnitProperty_LastRenderError,
                au_sys::kAudioUnitScope_Global,
                0,
            );
        }
    }

    pub(super) fn last_render_sample_time(&self) -> au_sys::Float64 {
        self.last_render_sample_time.load(Ordering::SeqCst)
    }

    // FIXME: Find a better way to determine whether this plugin is currently rendering or not.
    //        An initialized plugin might not be rendering anything.
    //        Example: Bypass a plugin in Logic Pro. The `kAudioUnitProperty_BypassEffect`
    //                 property seems to be unused in that host.
    fn is_rendering(&self) -> bool {
        self.last_render_sample_time() != DEFAULT_LAST_RENDER_SAMPLE_TIME
    }

    pub(super) fn set_host_callback_info(&self, info: au_sys::HostCallbackInfo) {
        *self.host_callback_info.borrow_mut() = Some(AuHostCallbackInfo::new(info));
    }

    fn make_process_context(&self, sample_rate: f32) -> WrapperProcessContext<'_, P> {
        let mut transport = Transport::new(sample_rate);
        if let Some(info) = self
            .host_callback_info
            .borrow()
            .as_ref()
            .map(|info| info.as_ref())
        {
            if let Some(beat_tempo_proc) = info.beatAndTempoProc {
                let mut current_beat = 0.0;
                let mut current_tempo = 0.0;
                unsafe {
                    if beat_tempo_proc(
                        info.hostUserData,
                        &raw mut current_beat,
                        &raw mut current_tempo,
                    ) == NO_ERROR
                    {
                        transport.tempo = Some(current_tempo);
                        transport.pos_beats = Some(current_beat);
                    }
                }
            }

            if let Some(time_proc) = info.musicalTimeLocationProc {
                let mut time_sig_num = 0.0;
                let mut time_sig_denom = 0;
                let mut current_measure_down_beat = 0.0;
                unsafe {
                    if time_proc(
                        info.hostUserData,
                        null_mut(),
                        &raw mut time_sig_num,
                        &raw mut time_sig_denom,
                        &raw mut current_measure_down_beat,
                    ) == NO_ERROR
                    {
                        transport.time_sig_numerator = Some(time_sig_num as _);
                        transport.time_sig_denominator = Some(time_sig_denom as _);
                        transport.bar_start_pos_beats = Some(current_measure_down_beat);
                    }
                }
            }

            if let Some(transport_proc) = info.transportStateProc2 {
                let mut is_playing = 0;
                let mut is_recording = 0;
                let mut current_sample_in_time_line = 0.0;
                let mut is_cycling = 0;
                let mut cycle_start_beat = 0.0;
                let mut cycle_end_beat = 0.0;
                unsafe {
                    if transport_proc(
                        info.hostUserData,
                        &raw mut is_playing,
                        &raw mut is_recording,
                        null_mut(),
                        &raw mut current_sample_in_time_line,
                        &raw mut is_cycling,
                        &raw mut cycle_start_beat,
                        &raw mut cycle_end_beat,
                    ) == NO_ERROR
                    {
                        transport.playing = is_playing > 0;
                        transport.recording = is_recording > 0;
                        transport.pos_samples = Some(current_sample_in_time_line as _);
                        if is_cycling > 0 {
                            transport.loop_range_beats = Some((cycle_start_beat, cycle_end_beat));
                        }
                    }
                }
            } else if let Some(transport_proc) = info.transportStateProc {
                let mut is_playing = 0;
                let mut current_sample_in_time_line = 0.0;
                let mut is_cycling = 0;
                let mut cycle_start_beat = 0.0;
                let mut cycle_end_beat = 0.0;
                unsafe {
                    if transport_proc(
                        info.hostUserData,
                        &raw mut is_playing,
                        null_mut(),
                        &raw mut current_sample_in_time_line,
                        &raw mut is_cycling,
                        &raw mut cycle_start_beat,
                        &raw mut cycle_end_beat,
                    ) == NO_ERROR
                    {
                        transport.playing = is_playing > 0;
                        transport.pos_samples = Some(current_sample_in_time_line as _);
                        if is_cycling > 0 {
                            transport.loop_range_beats = Some((cycle_start_beat, cycle_end_beat));
                        }
                    }
                }
            }
        }

        WrapperProcessContext {
            wrapper: self,
            transport,
            input_events_guard: self.input_events.borrow_mut(),
            output_events_guard: self.output_events.borrow_mut(),
        }
    }

    pub(super) fn reset(
        &self,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::OSStatus {
        self.plugin.lock().reset();
        self.last_render_sample_time
            .store(DEFAULT_LAST_RENDER_SAMPLE_TIME, Ordering::SeqCst);
        NO_ERROR
    }

    fn process_plugin<'a>(
        &self,
        buffers: Buffers<'a, 'a>,
        context: &mut WrapperProcessContext<P>,
        sample_rate: f32,
    ) {
        let mut aux = AuxiliaryBuffers {
            inputs: buffers.aux_inputs,
            outputs: buffers.aux_outputs,
        };

        let process_status = self
            .plugin
            .lock()
            .process(buffers.main_buffer, &mut aux, context);
        match process_status {
            ProcessStatus::Error(err) => {
                nih_debug_assert_failure!("Process error: {}", err);
                // TODO: What OSStatus?
                self.tail_seconds.store(0.0);
            }
            ProcessStatus::Normal => {
                self.tail_seconds.store(0.0);
            }
            ProcessStatus::Tail(tail) => {
                self.tail_seconds
                    .store(tail as au_sys::Float64 / sample_rate as au_sys::Float64);
            }
            ProcessStatus::KeepAlive => {
                self.tail_seconds.store(au_sys::Float64::MAX);
            }
        }
    }

    pub(super) unsafe fn render_impl(
        &self,
        io_action_flags: *mut au_sys::AudioUnitRenderActionFlags,
        in_time_stamp: *const au_sys::AudioTimeStamp,
        in_output_bus_num: au_sys::UInt32,
        in_number_frames: au_sys::UInt32,
        io_data: *mut au_sys::AudioBufferList,
    ) -> au_sys::OSStatus {
        if P::AUDIO_IO_LAYOUTS.is_empty() && P::MIDI_OUTPUT != MidiConfig::None {
            let sample_rate = self.sample_rate();
            let mut context = self.make_process_context(sample_rate);

            let mut buffer_manager = self.buffer_manager.borrow_mut();
            let buffers = buffer_manager.create_buffers(0, 0, |_| {});

            self.process_plugin(buffers, &mut context, sample_rate);
        } else {
            let output_scope = self.output_scope.read();

            if let Some(current_output_element) = output_scope.element(in_output_bus_num) {
                if (*io_data).mBuffers[0].mData.is_null()
                    || current_output_element.should_allocate() != ShouldAllocate::False
                {
                    current_output_element.prepare_buffer_list(in_number_frames);
                } else {
                    current_output_element.copy_buffer_list_from(io_data);
                }

                // TODO: Is this really stable in all cases?
                // NOTE: All output elements are rendered in one go.
                //       Therefore, we just copy the buffer when `mSampleTime` is the same.
                if self.last_render_sample_time() != (*in_time_stamp).mSampleTime {
                    let input_scope = self.input_scope.read();
                    let mut buffer_is_valid = true;

                    for (i, input_element) in input_scope.elements.iter().enumerate() {
                        if input_element.pull_input(
                            io_action_flags,
                            in_time_stamp,
                            i as _,
                            in_number_frames,
                        ) != NO_ERROR
                        {
                            buffer_is_valid = false;
                            break;
                        }
                    }

                    if buffer_is_valid {
                        for (i, output_element) in output_scope.elements.iter().enumerate() {
                            if i as au_sys::UInt32 != in_output_bus_num {
                                output_element.prepare_buffer_list(in_number_frames);
                            }
                        }

                        let sample_rate = self.sample_rate();
                        let mut context = self.make_process_context(sample_rate);
                        let mut buffer_manager = self.buffer_manager.borrow_mut();

                        let mut create_buffers_and_process =
                            |block_start: u32, block_length: u32| {
                                let buffers = buffer_manager.create_buffers(
                                    block_start as _,
                                    block_length as _,
                                    |buffer_source| {
                                        for (i, input_element) in
                                            input_scope.elements.iter().enumerate()
                                        {
                                            let pointers = if i == 0 {
                                                &mut buffer_source.main_input_channel_pointers
                                            } else {
                                                &mut buffer_source.aux_input_channel_pointers[i - 1]
                                            };
                                            *pointers = input_element.create_channel_pointers();
                                        }

                                        for (i, output_element) in
                                            output_scope.elements.iter().enumerate()
                                        {
                                            let pointers = if i == 0 {
                                                &mut buffer_source.main_output_channel_pointers
                                            } else {
                                                &mut buffer_source.aux_output_channel_pointers
                                                    [i - 1]
                                            };
                                            *pointers = output_element.create_channel_pointers();
                                        }
                                    },
                                );

                                self.process_plugin(buffers, &mut context, sample_rate);
                            };

                        if P::SAMPLE_ACCURATE_AUTOMATION {
                            let mut scheduled_params = self.scheduled_params.borrow_mut();

                            let mut block_start = 0u32;
                            for (block_end, param_events) in scheduled_params.iter_mut() {
                                if *block_end >= in_number_frames {
                                    break;
                                }

                                let block_length = block_end - block_start;
                                if block_length > 0 {
                                    create_buffers_and_process(block_start, block_length);
                                }

                                for param_event in param_events.iter_mut() {
                                    match param_event {
                                        ScheduledParamEvent::Immediate(immediate) => {
                                            let param_changed = self.set_param_impl(
                                                immediate.param_hash,
                                                immediate.param,
                                                immediate.value,
                                                false,
                                                sample_rate,
                                                false,
                                            );
                                            if param_changed {
                                                self.post_editor_param_event(
                                                    EditorParamEvent::NotifyEditor {
                                                        param_hash: immediate.param_hash,
                                                        normalized_value: immediate
                                                            .param
                                                            .unmodulated_normalized_value(),
                                                    },
                                                );
                                            }
                                        }
                                        ScheduledParamEvent::Ramp(ramp) => {
                                            let normalized_value = ramp.init(sample_rate);
                                            self.post_editor_param_event(
                                                EditorParamEvent::NotifyEditor {
                                                    param_hash: ramp.param_hash,
                                                    normalized_value,
                                                },
                                            );
                                        }
                                    }
                                }

                                block_start = *block_end;
                            }

                            if block_start < in_number_frames {
                                create_buffers_and_process(
                                    block_start,
                                    in_number_frames - block_start,
                                );
                            }

                            scheduled_params.clear();
                        } else {
                            nih_debug_assert!(
                                self.scheduled_params.borrow().is_empty(),
                                "`scheduled_params` must only be used when sample accurate \
                                 automation is enabled"
                            );
                            create_buffers_and_process(0, in_number_frames);
                        }
                    }

                    self.last_render_sample_time
                        .store((*in_time_stamp).mSampleTime, Ordering::SeqCst);
                }

                if (*io_data).mBuffers[0].mData.is_null() {
                    current_output_element.convert_buffer();
                    current_output_element.copy_buffer_list_to(io_data);
                } else {
                    current_output_element.convert_buffer();
                    current_output_element.copy_buffer_to(io_data);
                }
            } else {
                return au_sys::kAudioUnitErr_InvalidElement;
            }
        }

        self.handle_midi_output(in_time_stamp);
        NO_ERROR
    }

    pub(super) unsafe fn render(
        &self,
        mut io_action_flags: *mut au_sys::AudioUnitRenderActionFlags,
        in_time_stamp: *const au_sys::AudioTimeStamp,
        in_output_bus_num: au_sys::UInt32,
        in_number_frames: au_sys::UInt32,
        io_data: *mut au_sys::AudioBufferList,
    ) -> au_sys::OSStatus {
        // ---------- Prepare ---------- //

        match &self.bypass_param {
            BypassParam::Default(bypassed) => {
                if bypassed.load(Ordering::SeqCst) {
                    return NO_ERROR;
                }
            }
            _ => (),
        }

        if in_time_stamp.is_null() || io_data.is_null() {
            let os_status = au_sys::kAudio_ParamError;
            self.set_last_render_error(os_status);
            return os_status;
        }

        let mut temp_io_action_flags;
        if io_action_flags.is_null() {
            temp_io_action_flags = au_sys::AudioUnitRenderActionFlags::default();
            io_action_flags = &raw mut temp_io_action_flags;
        }
        if ((*io_action_flags) & au_sys::kAudioUnitRenderAction_DoNotCheckRenderArgs) == 0 {
            if in_number_frames > self.buffer_size() {
                let os_status = au_sys::kAudioUnitErr_TooManyFramesToProcess;
                self.set_last_render_error(os_status);
                return os_status;
            }
        }

        let render_notifies = self.render_notifies.borrow();
        let has_render_notifies = render_notifies.0.len() > 0;
        if has_render_notifies {
            let mut notify_io_action_flags =
                (*io_action_flags) | au_sys::kAudioUnitRenderAction_PreRender;

            render_notifies.call(
                &mut notify_io_action_flags,
                in_time_stamp,
                in_output_bus_num,
                in_number_frames,
                io_data,
            );
        }

        // ---------- Render ---------- //

        let os_status = self.render_impl(
            io_action_flags,
            in_time_stamp,
            in_output_bus_num,
            in_number_frames,
            io_data,
        );

        // ---------- Finish ---------- //

        if has_render_notifies {
            let mut notify_io_action_flags =
                (*io_action_flags) | au_sys::kAudioUnitRenderAction_PostRender;
            if os_status != NO_ERROR {
                notify_io_action_flags |= au_sys::kAudioUnitRenderAction_PostRenderError;
            }

            render_notifies.call(
                &mut notify_io_action_flags,
                in_time_stamp,
                in_output_bus_num,
                in_number_frames,
                io_data,
            );
        }

        self.set_last_render_error(os_status);

        let mut au_param_event = self.au_param_event.borrow_mut();
        while let Some(event) = self.editor_param_event_queue.pop() {
            self.handle_editor_param_event(event, &mut au_param_event);
        }

        let updated_state = self.updated_state_receiver.try_recv();
        if let Ok(mut state) = updated_state {
            self.set_state_object(&mut state, true);

            if let Err(err) = self.updated_state_sender.send(state) {
                nih_debug_assert_failure!(
                    "Failed to send state object back to GUI thread: {}",
                    err
                );
            };
        }

        os_status
    }

    // ---------- State ---------- //

    pub(super) unsafe fn get_desc(&self) -> (au_sys::AudioComponentDescription, au_sys::OSStatus) {
        let comp = au_sys::AudioComponentInstanceGetComponent(self.unit());
        let mut desc = au_sys::AudioComponentDescription::default();
        let os_status = au_sys::AudioComponentGetDescription(comp, &raw mut desc);
        (desc, os_status)
    }

    fn make_params_iter(&self) -> impl IntoIterator<Item = (&String, ParamPtr)> {
        self.param_hash_to_param
            .iter()
            .filter_map(|(_, wrapper_param)| Some((&wrapper_param.id, wrapper_param.ptr)))
    }

    pub(super) fn get_state_json(&self) -> Vec<u8> {
        unsafe { state::serialize_json::<P>(self.params.clone(), self.make_params_iter()).unwrap() }
    }

    pub(super) fn set_state_json(&self, state: &[u8]) -> bool {
        if let Some(mut state) = unsafe { state::deserialize_json(state) } {
            self.set_state_object(&mut state, false)
        } else {
            false
        }
    }

    pub(super) fn get_state_object(&self) -> PluginState {
        unsafe { state::serialize_object::<P>(self.params.clone(), self.make_params_iter()) }
    }

    fn set_state_object(&self, state: &mut PluginState, from_gui: bool) -> bool {
        let mut success = unsafe {
            state::deserialize_object::<P>(
                state,
                self.params.clone(),
                |param_id| self.param_id_to_ptr.get(param_id).copied(),
                Some(&self.buffer_config.borrow()),
            )
        };
        if !success {
            return false;
        }

        let mut plugin = self.plugin.lock();

        let audio_io_layout = self.audio_io_layout.borrow();
        let buffer_config = self.buffer_config.borrow();
        let mut init_context = self.make_init_context();

        success = plugin.initialize(&audio_io_layout, &buffer_config, &mut init_context);
        if success {
            plugin.reset();
        } else {
            return false;
        }

        let task_posted = self.schedule_gui(Task::ParameterValuesChanged);
        nih_debug_assert!(task_posted, "The task queue is full, dropping task...");

        self.request_resize();
        if from_gui {
            self.call_property_listeners(
                au_sys::kAudioUnitProperty_ClassInfo,
                au_sys::kAudioUnitScope_Global,
                0,
            );
        }

        true
    }

    pub(super) fn set_state_object_from_gui(&self, mut state: PluginState) {
        loop {
            if self.is_rendering() {
                match self
                    .updated_state_sender
                    .send_timeout(state, Duration::from_secs(1))
                {
                    Ok(_) => {
                        let state = self.updated_state_receiver.recv();
                        drop(state);
                        break;
                    }
                    Err(SendTimeoutError::Timeout(value)) => {
                        state = value;
                        continue;
                    }
                    Err(SendTimeoutError::Disconnected(_)) => {
                        nih_debug_assert_failure!("State update channel got disconnected");
                        return;
                    }
                }
            } else {
                self.set_state_object(&mut state, true);
                break;
            }
        }
    }

    pub(super) fn preset(&self) -> &au_sys::AUPreset {
        self.preset.as_ref()
    }

    pub(super) fn set_preset(&mut self, preset: Option<au_sys::AUPreset>) {
        self.preset.set(preset);
    }
}

// ---------- Events / Tasks ---------- //

impl<P: AuPlugin> EventLoop<Task<P>, Wrapper<P>> for Wrapper<P> {
    fn new_and_spawn(_executor: Weak<Self>) -> Self {
        panic!("What are you doing");
    }

    fn schedule_gui(&self, task: Task<P>) -> bool {
        if self.is_main_thread() {
            self.execute(task, true);
            true
        } else {
            let success = self.tasks.push(task).is_ok();
            if success {
                Queue::main().exec_async({
                    let wrapper = self.as_arc();
                    move || {
                        while let Some(task) = wrapper.tasks.pop() {
                            wrapper.execute(task, true);
                        }
                    }
                });
            }
            success
        }
    }

    fn schedule_background(&self, task: Task<P>) -> bool {
        self.background_thread
            .borrow()
            .as_ref()
            .unwrap()
            .schedule(task)
    }

    fn is_main_thread(&self) -> bool {
        NSThread::isMainThread_class()
    }
}

impl<P: AuPlugin> MainThreadExecutor<Task<P>> for Wrapper<P> {
    fn execute(&self, task: Task<P>, is_gui_thread: bool) {
        match task {
            Task::PluginTask(task) => (self.task_executor.lock())(task),
            Task::ParameterValueChanged(param_hash, normalized_value) => {
                if let Some(editor) = self.editor.borrow().as_ref() {
                    if self.wrapper_view_holder.borrow().has_view() {
                        let param_id = &self.param_hash_to_param[&param_hash].id;
                        editor
                            .lock()
                            .param_value_changed(param_id, normalized_value);
                    }
                }
            }
            Task::ParameterValuesChanged => {
                if let Some(editor) = self.editor.borrow().as_ref() {
                    if self.wrapper_view_holder.borrow().has_view() {
                        editor.lock().param_values_changed();
                    }
                }
            }
            Task::RequestResize => {
                nih_debug_assert!(
                    is_gui_thread,
                    "A `NSView` must be resized on the main thread"
                );
                self.wrapper_view_holder
                    .borrow()
                    .resize(self.editor.borrow().as_ref().unwrap().lock().size());
            }
        }
    }
}

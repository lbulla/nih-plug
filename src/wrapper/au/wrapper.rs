use atomic_refcell::AtomicRefCell;
use crossbeam::queue::ArrayQueue;
use dispatch::Queue;
use objc2::rc::Retained;
use objc2_app_kit::NSView;
use objc2_foundation::NSThread;
use parking_lot::{Mutex, RwLock};
use std::any::Any;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Weak};

use crate::event_loop::{BackgroundThread, EventLoop, MainThreadExecutor, TASK_QUEUE_CAPACITY};
use crate::prelude::{
    AsyncExecutor, AuPlugin, BufferConfig, Editor, ParentWindowHandle, ProcessMode, TaskExecutor,
};
use crate::wrapper::au::au_types::AuPreset;
use crate::wrapper::au::context::{WrapperGuiContext, WrapperInitContext};
use crate::wrapper::au::editor::WrapperViewHolder;
use crate::wrapper::au::properties::{PropertyDispatcher, PropertyDispatcherImpl};
use crate::wrapper::au::scope::{InputElement, IoElement, IoElementImpl, IoScope, OutputElement};
use crate::wrapper::au::util::ThreadWrapper;
use crate::wrapper::au::{au_sys, AuPropertyListenerProc, NO_ERROR};

// ---------- Constants ---------- //

const DEFAULT_BUFFER_SIZE: u32 = 512;
const DEFAULT_SAMPLE_RATE: f32 = 48000.0;

// ---------- Types ---------- //

struct PropertyListener {
    proc: AuPropertyListenerProc,
    data: ThreadWrapper<*mut c_void>,
}

enum Task<P: AuPlugin> {
    PluginTask(P::BackgroundTask),

    // NOTE: A `NSView` must be resized on the main thread.
    RequestResize,
}

// ---------- Wrapper ---------- //

pub(super) struct Wrapper<P: AuPlugin> {
    unit: ThreadWrapper<au_sys::AudioUnit>,

    this: AtomicRefCell<Weak<Wrapper<P>>>,
    plugin: Mutex<P>,
    property_listeners: AtomicRefCell<HashMap<au_sys::AudioUnitPropertyID, Vec<PropertyListener>>>,

    pub(super) task_executor: Mutex<TaskExecutor<P>>,
    tasks: ArrayQueue<Task<P>>,
    background_thread: AtomicRefCell<Option<BackgroundThread<Task<P>, Self>>>,

    editor: AtomicRefCell<Option<Mutex<Box<dyn Editor>>>>,
    wrapper_view_holder: AtomicRefCell<WrapperViewHolder>,

    buffer_config: AtomicRefCell<BufferConfig>,
    latency_samples: AtomicU32,

    // TODO: Other scopes.
    pub(super) input_scope: RwLock<IoScope<InputElement>>,
    pub(super) output_scope: RwLock<IoScope<OutputElement>>,

    initialized: AtomicBool,
    preset: AuPreset, // TODO
}

impl<P: AuPlugin> Wrapper<P> {
    pub(super) fn new(unit: au_sys::AudioUnit) -> Arc<Self> {
        let mut plugin = P::default();
        let task_executor = plugin.task_executor();
        let audio_io_layout = P::AUDIO_IO_LAYOUTS.first().copied().unwrap_or_default();

        let mut input_scope = IoScope::new();
        let mut output_scope = IoScope::new();

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

        let wrapper = Arc::new(Self {
            unit: ThreadWrapper::new(unit),

            this: AtomicRefCell::new(Weak::new()),
            plugin: Mutex::new(plugin),
            property_listeners: AtomicRefCell::new(HashMap::new()),

            task_executor: Mutex::new(task_executor),
            tasks: ArrayQueue::new(TASK_QUEUE_CAPACITY),
            background_thread: AtomicRefCell::new(None),

            editor: AtomicRefCell::new(None),
            wrapper_view_holder: AtomicRefCell::new(WrapperViewHolder::default()),

            buffer_config: AtomicRefCell::new(BufferConfig {
                sample_rate: DEFAULT_SAMPLE_RATE,
                min_buffer_size: None,
                max_buffer_size: DEFAULT_BUFFER_SIZE,
                process_mode: ProcessMode::Realtime,
            }),
            latency_samples: AtomicU32::new(0),

            input_scope: RwLock::new(input_scope),
            output_scope: RwLock::new(output_scope),

            initialized: AtomicBool::new(false),
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

    // ---------- Presets ---------- //

    // TODO
    pub(super) fn preset(&self) -> &au_sys::AUPreset {
        self.preset.as_ref()
    }

    pub(super) fn set_preset(&mut self, preset: au_sys::AUPreset) {
        self.preset.set(preset);
    }

    // ---------- Contexts ---------- //

    fn make_gui_context(&self) -> Arc<WrapperGuiContext<P>> {
        Arc::new(WrapperGuiContext {
            wrapper: self.as_arc(),
        })
    }

    fn make_init_context(&self) -> WrapperInitContext<P> {
        WrapperInitContext { wrapper: self }
    }

    // ---------- Editor ---------- //

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

    pub(super) fn init(&mut self) -> au_sys::OSStatus {
        let input_scope = self.input_scope.read();
        let output_scope = self.output_scope.read();

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
                let mut plugin = self.plugin.lock();

                let audio_io_layout = P::AUDIO_IO_LAYOUTS[i];
                let buffer_config = self.buffer_config.borrow();
                let mut init_context = self.make_init_context();

                let success =
                    plugin.initialize(&audio_io_layout, &buffer_config, &mut init_context);
                if success {
                    plugin.reset();

                    let params = plugin.params();
                    for (_id, ptr, _group) in params.param_map().iter() {
                        unsafe { ptr.update_smoother(buffer_config.sample_rate, true) };
                    }

                    self.initialized.store(true, Ordering::SeqCst);
                    return NO_ERROR;
                } else {
                    return au_sys::kAudioUnitErr_FailedInitialization;
                }
            };
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
            listeners
                .retain(|listener| listener.proc != in_proc && listener.data.get() != in_proc_data);
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

mod au_types;
mod context;
mod editor;
pub mod layout;
mod properties;
mod scope;
mod util;
mod wrapper;

// ---------- Imports ---------- //

use std::ffi::{c_uint, c_void};
use std::mem::transmute;
use std::ptr::null_mut;
use std::sync::Arc;

pub use coreaudio_sys as au_sys;
pub(self) use wrapper::Wrapper;

use crate::midi::MidiConfig;
use crate::prelude::AuPlugin;

// ---------- Constants ---------- //

pub(self) const NO_ERROR: au_sys::OSStatus = au_sys::noErr as _;

// ---------- Types ---------- //

pub(self) type AuPropertyListenerProc = unsafe extern "C" fn(
    in_ref_con: *mut c_void,
    in_unit: au_sys::AudioUnit,
    in_id: au_sys::AudioUnitPropertyID,
    in_scope: au_sys::AudioUnitScope,
    in_element: au_sys::AudioUnitElement,
);

pub(self) type AuRenderCallback = unsafe extern "C" fn(
    in_ref_con: *mut c_void,
    io_action_flags: *mut au_sys::AudioUnitRenderActionFlags,
    in_time_stamp: *const au_sys::AudioTimeStamp,
    in_output_bus_num: au_sys::UInt32,
    in_number_frames: au_sys::UInt32,
    io_data: *mut au_sys::AudioBufferList,
);

// ---------- PluginInstance ---------- //

#[repr(C)]
pub struct PluginInstance<P: AuPlugin> {
    _interface: au_sys::AudioComponentPlugInInterface,
    wrapper: *mut Wrapper<P>,
}

impl<P: AuPlugin> PluginInstance<P> {
    pub fn new() -> Self {
        Self {
            _interface: au_sys::AudioComponentPlugInInterface {
                Open: Some(Self::open),
                Close: Some(Self::close),
                Lookup: Some(Self::lookup),
                reserved: null_mut(),
            },
            wrapper: null_mut(),
        }
    }
}

impl<P: AuPlugin> PluginInstance<P> {
    unsafe extern "C" fn open(this: *mut c_void, unit: au_sys::AudioUnit) -> au_sys::OSStatus {
        let plugin_instance = this as *mut Self;
        (*plugin_instance).wrapper = Arc::into_raw(Wrapper::<P>::new(unit)) as _;
        NO_ERROR
    }

    unsafe extern "C" fn close(this: *mut c_void) -> au_sys::OSStatus {
        let plugin_instance = this as *mut Self;
        let _ = Arc::from_raw((*plugin_instance).wrapper); // NOTE: Drop the wrapper.
        (*plugin_instance).wrapper = null_mut();
        NO_ERROR
    }

    // NOTE: Sorted by `selector` value.
    unsafe extern "C" fn lookup(selector: au_sys::SInt16) -> au_sys::AudioComponentMethod {
        match selector as c_uint {
            au_sys::kAudioUnitInitializeSelect => Some(transmute(Self::init as *const c_void)),
            au_sys::kAudioUnitUninitializeSelect => Some(transmute(Self::uninit as *const c_void)),
            au_sys::kAudioUnitGetPropertyInfoSelect => {
                Some(transmute(Self::get_property_info as *const c_void))
            }
            au_sys::kAudioUnitGetPropertySelect => {
                Some(transmute(Self::get_property as *const c_void))
            }
            au_sys::kAudioUnitSetPropertySelect => {
                Some(transmute(Self::set_property as *const c_void))
            }
            au_sys::kAudioUnitGetParameterSelect => {
                Some(transmute(Self::get_param as *const c_void))
            }
            au_sys::kAudioUnitSetParameterSelect => {
                Some(transmute(Self::set_param as *const c_void))
            }
            au_sys::kAudioUnitResetSelect => Some(transmute(Self::reset as *const c_void)),
            au_sys::kAudioUnitAddPropertyListenerSelect => {
                Some(transmute(Self::add_property_listener as *const c_void))
            }
            au_sys::kAudioUnitRemovePropertyListenerSelect => {
                Some(transmute(Self::remove_property_listener as *const c_void))
            }
            au_sys::kAudioUnitRenderSelect => Some(transmute(Self::render as *const c_void)),
            au_sys::kAudioUnitAddRenderNotifySelect => {
                Some(transmute(Self::add_render_notify as *const c_void))
            }
            au_sys::kAudioUnitRemoveRenderNotifySelect => {
                Some(transmute(Self::remove_render_notify as *const c_void))
            }
            au_sys::kAudioUnitScheduleParametersSelect => {
                Some(transmute(Self::schedule_params as *const c_void))
            }
            au_sys::kAudioUnitRemovePropertyListenerWithUserDataSelect => Some(transmute(
                Self::remove_property_listener_data as *const c_void,
            )),
            au_sys::kMusicDeviceMIDIEventSelect => {
                if P::MIDI_INPUT != MidiConfig::None {
                    Some(transmute(Self::midi_event as *const c_void))
                } else {
                    None
                }
            }
            au_sys::kMusicDeviceSysExSelect => {
                if P::MIDI_INPUT != MidiConfig::None {
                    Some(transmute(Self::sys_ex as *const c_void))
                } else {
                    None
                }
            }
            au_sys::kMusicDeviceMIDIEventListSelect => {
                if P::MIDI_INPUT != MidiConfig::None {
                    Some(transmute(Self::midi_event_list as *const c_void))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    unsafe extern "C" fn init(this: *mut c_void) -> au_sys::OSStatus {
        let wrapper = Self::wrapper_from_this(this);
        wrapper.init()
    }

    unsafe extern "C" fn uninit(this: *mut c_void) -> au_sys::OSStatus {
        let wrapper = Self::wrapper_from_this(this);
        wrapper.uninit()
    }

    // NOTE: out_data_size == null || out_writable == null => assign no value (individually)
    unsafe extern "C" fn get_property_info(
        this: *mut c_void,
        in_id: au_sys::AudioUnitPropertyID,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        out_data_size: *mut au_sys::UInt32,
        out_writable: *mut au_sys::Boolean,
    ) -> au_sys::OSStatus {
        let wrapper = Self::wrapper_from_this(this);
        wrapper.get_property_info(in_id, in_scope, in_element, out_data_size, out_writable)
    }

    // NOTE: out_data == null => assign only the size
    unsafe extern "C" fn get_property(
        this: *mut c_void,
        in_id: au_sys::AudioUnitPropertyID,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        out_data: *mut c_void,
        io_data_size: *mut au_sys::UInt32,
    ) -> au_sys::OSStatus {
        let wrapper = Self::wrapper_from_this(this);
        wrapper.get_property(in_id, in_scope, in_element, out_data, io_data_size)
    }

    // NOTE: in_data == null && in_data_size == 0 => reset (for values without a default value)
    unsafe extern "C" fn set_property(
        this: *mut c_void,
        in_id: au_sys::AudioUnitPropertyID,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        in_data: *const c_void,
        in_data_size: au_sys::UInt32,
    ) -> au_sys::OSStatus {
        let wrapper = Self::wrapper_from_this(this);
        wrapper.set_property(in_id, in_scope, in_element, in_data, in_data_size)
    }

    unsafe extern "C" fn get_param(
        this: *mut c_void,
        in_id: au_sys::AudioUnitPropertyID,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        out_value: *mut au_sys::AudioUnitParameterValue,
    ) -> au_sys::OSStatus {
        let wrapper = Self::wrapper_from_this(this);
        wrapper.get_param(in_id, in_scope, in_element, out_value)
    }

    // NOTE: Potential realtime function. Hence, update the parameters immediately.
    unsafe extern "C" fn set_param(
        this: *mut c_void,
        in_id: au_sys::AudioUnitPropertyID,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        in_value: au_sys::AudioUnitParameterValue,
        in_buffer_offset_in_frames: au_sys::UInt32,
    ) -> au_sys::OSStatus {
        let wrapper = Self::wrapper_from_this(this);
        wrapper.set_param(
            in_id,
            in_scope,
            in_element,
            in_value,
            in_buffer_offset_in_frames,
        )
    }

    unsafe extern "C" fn reset(
        this: *mut c_void,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
    ) -> au_sys::OSStatus {
        let wrapper = Self::wrapper_from_this(this);
        wrapper.reset(in_scope, in_element)
    }

    unsafe extern "C" fn add_property_listener(
        this: *mut c_void,
        in_id: au_sys::AudioUnitPropertyID,
        in_proc: AuPropertyListenerProc,
        in_proc_data: *mut c_void,
    ) -> au_sys::OSStatus {
        let wrapper = Self::wrapper_from_this(this);
        wrapper.add_property_listener(in_id, in_proc, in_proc_data)
    }

    unsafe extern "C" fn remove_property_listener(
        this: *mut c_void,
        in_id: au_sys::AudioUnitPropertyID,
        in_proc: AuPropertyListenerProc,
    ) -> au_sys::OSStatus {
        let wrapper = Self::wrapper_from_this(this);
        wrapper.remove_property_listener(in_id, in_proc, null_mut())
    }

    unsafe extern "C" fn render(
        this: *mut c_void,
        io_action_flags: *mut au_sys::AudioUnitRenderActionFlags,
        in_time_stamp: *const au_sys::AudioTimeStamp,
        in_output_bus_num: au_sys::UInt32,
        in_number_frames: au_sys::UInt32,
        io_data: *mut au_sys::AudioBufferList,
    ) -> au_sys::OSStatus {
        let wrapper = Self::wrapper_from_this(this);
        wrapper.render(
            io_action_flags,
            in_time_stamp,
            in_output_bus_num,
            in_number_frames,
            io_data,
        )
    }

    unsafe extern "C" fn add_render_notify(
        this: *mut c_void,
        in_proc: AuRenderCallback,
        in_proc_data: *mut c_void,
    ) -> au_sys::OSStatus {
        let wrapper = Self::wrapper_from_this(this);
        wrapper.add_render_notify(in_proc, in_proc_data)
    }

    unsafe extern "C" fn remove_render_notify(
        this: *mut c_void,
        in_proc: AuRenderCallback,
        in_proc_data: *mut c_void,
    ) -> au_sys::OSStatus {
        let wrapper = Self::wrapper_from_this(this);
        wrapper.remove_render_notify(in_proc, in_proc_data)
    }

    // NOTE: Potential realtime function. Called directly before `render`.
    unsafe extern "C" fn schedule_params(
        this: *mut c_void,
        in_param_events: *const au_sys::AudioUnitParameterEvent,
        in_num_param_events: au_sys::UInt32,
    ) -> au_sys::OSStatus {
        let wrapper = Self::wrapper_from_this(this);
        wrapper.schedule_params(in_param_events, in_num_param_events)
    }

    unsafe extern "C" fn remove_property_listener_data(
        this: *mut c_void,
        in_id: au_sys::AudioUnitPropertyID,
        in_proc: AuPropertyListenerProc,
        in_proc_data: *mut c_void,
    ) -> au_sys::OSStatus {
        let wrapper = Self::wrapper_from_this(this);
        wrapper.remove_property_listener(in_id, in_proc, in_proc_data)
    }

    unsafe extern "C" fn midi_event(
        this: *mut c_void,
        in_status: au_sys::UInt32,
        in_data1: au_sys::UInt32,
        in_data2: au_sys::UInt32,
        in_offset_sample_frame: au_sys::UInt32,
    ) -> au_sys::OSStatus {
        let wrapper = Self::wrapper_from_this(this);
        wrapper.midi_event(in_status, in_data1, in_data2, in_offset_sample_frame)
    }

    unsafe extern "C" fn sys_ex(
        this: *mut c_void,
        in_data: *const au_sys::UInt8,
        in_length: au_sys::UInt32,
    ) -> au_sys::OSStatus {
        let wrapper = Self::wrapper_from_this(this);
        wrapper.sys_ex(in_data, in_length)
    }

    unsafe extern "C" fn midi_event_list(
        this: *mut c_void,
        in_offset_sample_frame: au_sys::UInt32,
        in_event_list: *const au_sys::MIDIEventList,
    ) -> au_sys::OSStatus {
        let wrapper = Self::wrapper_from_this(this);
        wrapper.midi_event_list(in_offset_sample_frame, in_event_list)
    }

    unsafe fn wrapper_from_this(this: *mut c_void) -> &'static mut Wrapper<P> {
        let plugin_instance = this as *mut PluginInstance<P>;
        &mut *(*plugin_instance).wrapper
    }
}

// ---------- Factory ---------- //

#[macro_export]
macro_rules! nih_export_au {
    ($plugin_ty:ty) => {
        use $crate::wrapper::au::{au_sys, PluginInstance};

        #[no_mangle]
        pub extern "C" fn factory(
            _in_desc: *const au_sys::AudioComponentDescription,
        ) -> *mut au_sys::AudioComponentPlugInInterface {
            let instance = Box::new(PluginInstance::<$plugin_ty>::new());
            Box::into_raw(instance) as _
        }
    };
}

mod util;
mod wrapper;

// ---------- Imports ---------- //

use std::ffi::{c_uint, c_void};
use std::mem::transmute;
use std::ptr::null_mut;
use std::sync::Arc;

pub use coreaudio_sys as au_sys;
pub(self) use wrapper::Wrapper;

use crate::prelude::AuPlugin;

// ---------- Constants ---------- //

pub(self) const NO_ERROR: au_sys::OSStatus = au_sys::noErr as _;

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

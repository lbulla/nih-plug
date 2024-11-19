use atomic_refcell::AtomicRefCell;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, Weak};

use crate::prelude::AuPlugin;
use crate::wrapper::au::properties::{PropertyDispatcher, PropertyDispatcherImpl};
use crate::wrapper::au::util::ThreadWrapper;
use crate::wrapper::au::{au_sys, AuPropertyListenerProc, NO_ERROR};

// ---------- Types ---------- //

struct PropertyListener {
    proc: AuPropertyListenerProc,
    data: ThreadWrapper<*mut c_void>,
}

// ---------- Wrapper ---------- //

pub(super) struct Wrapper<P: AuPlugin> {
    unit: ThreadWrapper<au_sys::AudioUnit>,

    this: AtomicRefCell<Weak<Wrapper<P>>>,
    plugin: Mutex<P>,
    property_listeners: AtomicRefCell<HashMap<au_sys::AudioUnitPropertyID, Vec<PropertyListener>>>,
}

impl<P: AuPlugin> Wrapper<P> {
    pub(super) fn new(unit: au_sys::AudioUnit) -> Arc<Self> {
        let plugin = P::default();

        let wrapper = Arc::new(Self {
            unit: ThreadWrapper::new(unit),

            this: AtomicRefCell::new(Weak::new()),
            plugin: Mutex::new(plugin),
            property_listeners: AtomicRefCell::new(HashMap::new()),
        });

        *wrapper.this.borrow_mut() = Arc::downgrade(&wrapper);

        wrapper
    }

    // ---------- Getter ---------- //

    pub(super) fn unit(&self) -> au_sys::AudioUnit {
        self.unit.get()
    }

    // ---------- Setup ---------- //

    pub(super) fn init(&self) -> au_sys::OSStatus {
        NO_ERROR
    }

    pub(super) fn uninit(&self) -> au_sys::OSStatus {
        NO_ERROR
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

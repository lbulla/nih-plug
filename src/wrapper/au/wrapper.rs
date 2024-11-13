use atomic_refcell::AtomicRefCell;
use parking_lot::Mutex;
use std::sync::{Arc, Weak};

use crate::prelude::AuPlugin;
use crate::wrapper::au::util::ThreadWrapper;
use crate::wrapper::au::{au_sys, NO_ERROR};

// ---------- Wrapper ---------- //

pub(super) struct Wrapper<P: AuPlugin> {
    unit: ThreadWrapper<au_sys::AudioUnit>,

    this: AtomicRefCell<Weak<Wrapper<P>>>,
    plugin: Mutex<P>,
}

impl<P: AuPlugin> Wrapper<P> {
    pub(super) fn new(unit: au_sys::AudioUnit) -> Arc<Self> {
        let plugin = P::default();

        let wrapper = Arc::new(Self {
            unit: ThreadWrapper::new(unit),

            this: AtomicRefCell::new(Weak::new()),
            plugin: Mutex::new(plugin),
        });

        *wrapper.this.borrow_mut() = Arc::downgrade(&wrapper);

        wrapper
    }

    // ---------- Setup ---------- //

    pub(super) fn init(&self) -> au_sys::OSStatus {
        NO_ERROR
    }

    pub(super) fn uninit(&self) -> au_sys::OSStatus {
        NO_ERROR
    }
}

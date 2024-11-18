use crate::wrapper::au::au_sys;
use crate::wrapper::au::util::{release_CFStringRef, utf8_to_CFStringRef};

// ---------- AuPreset ---------- //

// TODO
const DEFAULT_PRESET_NAME: &[u8] = b"Untitled\0";

pub(super) struct AuPreset(au_sys::AUPreset);

impl AuPreset {
    pub(super) fn default() -> Self {
        Self(Self::make_default())
    }

    pub(super) fn as_ref(&self) -> &au_sys::AUPreset {
        &self.0
    }

    pub(super) fn set(&mut self, preset: au_sys::AUPreset) {
        release_CFStringRef(self.0.presetName);
        self.0 = preset;
    }

    fn make_default() -> au_sys::AUPreset {
        au_sys::AUPreset {
            presetNumber: -1,
            presetName: utf8_to_CFStringRef(DEFAULT_PRESET_NAME),
        }
    }
}

unsafe impl Send for AuPreset {}
unsafe impl Sync for AuPreset {}

use std::num::NonZeroU32;
use std::ptr::null_mut;

use crate::wrapper::au::au_sys;

// ---------- ThreadWrapper ---------- //

// NOTE: Make types like pointers Send and Sync. Must obviously be used with care.
pub(super) struct ThreadWrapper<T: Clone>(T);

impl<T: Clone> ThreadWrapper<T> {
    pub(super) fn new(value: T) -> Self {
        Self(value)
    }

    pub(super) fn get(&self) -> T {
        self.0.clone()
    }

    pub(super) fn as_ref(&self) -> &T {
        &self.0
    }

    pub(super) fn as_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

unsafe impl<T: Clone> Send for ThreadWrapper<T> {}
unsafe impl<T: Clone> Sync for ThreadWrapper<T> {}

// ---------- Strings ---------- //

// TODO: Remove unused functions when they are not needed for sure.
pub(super) struct CFString(au_sys::CFStringRef);

impl CFString {
    pub(super) fn new(string: au_sys::CFStringRef) -> Self {
        Self(string)
    }

    pub(super) fn from_str(string: &str) -> Self {
        Self(str_to_CFStringRef(string))
    }

    pub(super) fn from_utf8(utf8: &[u8]) -> Self {
        Self(utf8_to_CFStringRef(utf8))
    }

    pub(super) fn get(&self) -> au_sys::CFStringRef {
        self.0
    }

    pub(super) fn set(&mut self, value: au_sys::CFStringRef) {
        self.release();
        self.0 = value;
    }

    pub(super) fn set_str(&mut self, string: &str) {
        self.set(str_to_CFStringRef(string));
    }

    pub(super) fn set_utf8(&mut self, utf8: &[u8]) {
        self.set(utf8_to_CFStringRef(utf8));
    }

    fn release(&mut self) {
        release_CFStringRef(self.0);
    }
}

impl Drop for CFString {
    fn drop(&mut self) {
        self.release();
    }
}

unsafe impl Send for CFString {}
unsafe impl Sync for CFString {}

#[must_use]
#[allow(non_snake_case)]
pub(super) fn str_to_CFStringRef(string: &str) -> au_sys::CFStringRef {
    unsafe {
        au_sys::CFStringCreateWithBytes(
            au_sys::kCFAllocatorDefault,
            string.as_ptr(),
            string.len() as _,
            au_sys::kCFStringEncodingUTF8,
            false as _,
        )
    }
}

#[must_use]
#[allow(non_snake_case)]
pub(super) fn utf8_to_CFStringRef(utf8: &[u8]) -> au_sys::CFStringRef {
    unsafe { au_sys::__CFStringMakeConstantString(utf8.as_ptr() as _) }
}

#[allow(non_snake_case)]
pub(super) fn release_CFStringRef(string_ref: au_sys::CFStringRef) {
    unsafe {
        au_sys::CFRelease(string_ref as _);
    }
}

// ---------- ChannelPointerVec ---------- //

pub(super) type ChannelPointerVec = ThreadWrapper<Vec<*mut f32>>;

impl ChannelPointerVec {
    pub(super) fn from_num_buffers(num_buffers: NonZeroU32) -> Self {
        Self::new(vec![null_mut(); num_buffers.get() as _])
    }
}

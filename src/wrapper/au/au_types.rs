use std::alloc::{alloc, dealloc, realloc, Layout};
use std::num::NonZeroU32;
use std::ptr::{copy_nonoverlapping, null_mut};
use std::sync::LazyLock;

use crate::wrapper::au::au_sys;
use crate::wrapper::au::util::{
    release_CFStringRef, retain_CFStringRef, utf8_to_const_CFStringRef, ThreadWrapper,
};

// ---------- AudioUnit ---------- //

pub(super) type AudioUnit = ThreadWrapper<au_sys::AudioUnit>;

// ---------- AuPreset ---------- //

static DEFAULT_PRESET_NAME: LazyLock<ThreadWrapper<au_sys::CFStringRef>> =
    LazyLock::new(|| ThreadWrapper::new(utf8_to_const_CFStringRef(b"Untitled\0")));

pub(super) struct AuPreset(au_sys::AUPreset);

impl AuPreset {
    pub(super) fn default() -> Self {
        Self(Self::make_default())
    }

    pub(super) fn as_ref(&self) -> &au_sys::AUPreset {
        &self.0
    }

    pub(super) fn set(&mut self, preset: Option<au_sys::AUPreset>) {
        let preset = preset.unwrap_or(Self::make_default());

        release_CFStringRef(self.0.presetName);
        self.0 = preset;
        retain_CFStringRef(self.0.presetName);
    }

    fn make_default() -> au_sys::AUPreset {
        au_sys::AUPreset {
            presetNumber: -1,
            presetName: DEFAULT_PRESET_NAME.get(),
        }
    }
}

impl Drop for AuPreset {
    fn drop(&mut self) {
        release_CFStringRef(self.0.presetName);
    }
}

unsafe impl Send for AuPreset {}
unsafe impl Sync for AuPreset {}

// ---------- AuRenderCallbackStruct ---------- //

pub(super) type AuRenderCallbackStruct = ThreadWrapper<au_sys::AURenderCallbackStruct>;

impl AuRenderCallbackStruct {
    // NOTE: This might call `get_property` (e.g. in `auval`).
    //       So make sure that reading the properties / scopes is possible.
    pub(super) fn call(
        &self,
        io_action_flags: *mut au_sys::AudioUnitRenderActionFlags,
        in_time_stamp: *const au_sys::AudioTimeStamp,
        in_bus_num: au_sys::UInt32,
        in_number_frames: au_sys::UInt32,
        io_data: *mut au_sys::AudioBufferList,
    ) -> au_sys::OSStatus {
        let proc = self.as_ref().inputProc.expect("`proc` must be `Some`");
        unsafe {
            (proc)(
                self.as_ref().inputProcRefCon,
                io_action_flags,
                in_time_stamp,
                in_bus_num,
                in_number_frames,
                io_data,
            )
        }
    }
}

// ---------- AuConnection ---------- //

pub(super) struct AuConnection {
    pub(super) src_unit: AudioUnit,
    pub(super) src_output_num: au_sys::UInt32,
}

impl AuConnection {
    pub(super) fn call(
        &self,
        io_action_flags: *mut au_sys::AudioUnitRenderActionFlags,
        in_time_stamp: *const au_sys::AudioTimeStamp,
        in_number_frames: au_sys::UInt32,
        io_data: *mut au_sys::AudioBufferList,
    ) -> au_sys::OSStatus {
        unsafe {
            au_sys::AudioUnitRender(
                self.src_unit.get(),
                io_action_flags,
                in_time_stamp,
                self.src_output_num,
                in_number_frames,
                io_data,
            )
        }
    }
}

impl From<&au_sys::AudioUnitConnection> for AuConnection {
    fn from(obj: &au_sys::AudioUnitConnection) -> Self {
        Self {
            src_unit: AudioUnit::new(obj.sourceAudioUnit),
            src_output_num: obj.sourceOutputNumber,
        }
    }
}

// ---------- AuBufferList ---------- //

// TODO: Interleaved.
// NOTE: The size of `au_sys::AudioBufferList` includes only one buffer.
//       We must therefore allocate the required size.
pub(super) struct AuBufferList {
    layout: Layout,
    list: *mut au_sys::AudioBufferList,
}

impl AuBufferList {
    pub(super) fn new(num_channels: NonZeroU32) -> Self {
        unsafe {
            let layout = Self::create_layout(num_channels);
            let list = alloc(layout) as *mut au_sys::AudioBufferList;
            (*list).mNumberBuffers = num_channels.get();

            let this = Self { layout, list };
            for buffer in this.iter_mut() {
                buffer.mNumberChannels = 1;
            }

            this
        }
    }

    pub(super) fn set_num_channels(&mut self, num_channels: NonZeroU32) {
        unsafe {
            let layout = Self::create_layout(num_channels);
            self.list = realloc(self.list as _, self.layout, layout.size()) as _;
            (*self.list).mNumberBuffers = num_channels.get();
            self.layout = layout;

            for buffer in self.iter_mut() {
                buffer.mNumberChannels = 1;
            }
        }
    }

    pub(super) fn list(&self) -> *mut au_sys::AudioBufferList {
        self.list
    }

    pub(super) fn iter(&self) -> AuBufferListIter {
        AuBufferListIter {
            buffer_list: self,
            index: 0,
        }
    }

    pub(super) fn iter_mut(&self) -> AuBufferListIterMut {
        AuBufferListIterMut {
            buffer_list: self,
            index: 0,
        }
    }

    pub(super) unsafe fn copy_from(&self, src: *const au_sys::AudioBufferList) {
        copy_nonoverlapping::<u8>(src as _, self.list as _, self.layout.size());
    }

    pub(super) unsafe fn copy_to(&self, dest: *mut au_sys::AudioBufferList) {
        copy_nonoverlapping::<u8>(self.list as _, dest as _, self.layout.size());
    }

    pub(super) unsafe fn copy_buffer_to(&self, dest: *mut au_sys::AudioBufferList) {
        for i in 0..(*dest).mNumberBuffers as usize {
            let src_buffer = &*self.buffer(i);
            let dest_buffer = &mut *(*dest).mBuffers.as_mut_ptr().add(i);

            dest_buffer.mDataByteSize = src_buffer.mDataByteSize;
            copy_nonoverlapping::<u8>(
                src_buffer.mData as _,
                dest_buffer.mData as _,
                src_buffer.mDataByteSize as _,
            );
        }
    }

    fn size(num_channels: NonZeroU32) -> usize {
        size_of::<au_sys::AudioBufferList>()
            + (num_channels.get() as usize - 1) * size_of::<au_sys::AudioBuffer>()
    }

    unsafe fn create_layout(num_channels: NonZeroU32) -> Layout {
        Layout::from_size_align_unchecked(
            Self::size(num_channels),
            align_of::<au_sys::AudioBufferList>(),
        )
    }

    unsafe fn buffer(&self, index: usize) -> *const au_sys::AudioBuffer {
        (*self.list).mBuffers.as_ptr().add(index)
    }

    unsafe fn buffer_mut(&self, index: usize) -> *mut au_sys::AudioBuffer {
        (*self.list).mBuffers.as_mut_ptr().add(index)
    }
}

impl Drop for AuBufferList {
    fn drop(&mut self) {
        unsafe {
            dealloc(self.list as _, self.layout);
        }
    }
}

unsafe impl Sync for AuBufferList {}
unsafe impl Send for AuBufferList {}

pub(super) struct AuBufferListIter<'a> {
    buffer_list: &'a AuBufferList,
    index: usize,
}

impl<'a> Iterator for AuBufferListIter<'a> {
    type Item = &'a au_sys::AudioBuffer;

    fn next(&mut self) -> Option<Self::Item> {
        unsafe {
            if self.index < (*self.buffer_list.list).mNumberBuffers as usize {
                let result = Some(&*self.buffer_list.buffer(self.index));
                self.index += 1;
                result
            } else {
                None
            }
        }
    }
}

pub(super) struct AuBufferListIterMut<'a> {
    buffer_list: &'a AuBufferList,
    index: usize,
}

impl<'a> Iterator for AuBufferListIterMut<'a> {
    type Item = &'a mut au_sys::AudioBuffer;

    fn next(&mut self) -> Option<Self::Item> {
        unsafe {
            if self.index < (*self.buffer_list.list).mNumberBuffers as usize {
                let result = Some(&mut *self.buffer_list.buffer_mut(self.index));
                self.index += 1;
                result
            } else {
                None
            }
        }
    }
}

// ---------- AuParamEvent ---------- //

pub(super) struct AuParamEvent(au_sys::AudioUnitEvent);

impl AuParamEvent {
    pub(super) fn new(event: au_sys::AudioUnitEvent) -> Self {
        Self(event)
    }

    pub(super) fn send(
        &mut self,
        event_type: au_sys::AudioUnitEventType,
        param_id: au_sys::AudioUnitParameterID,
    ) -> au_sys::OSStatus {
        self.0.mEventType = event_type;
        unsafe {
            self.0.mArgument.mParameter.mParameterID = param_id;
            au_sys::AUEventListenerNotify(null_mut(), null_mut(), &raw const self.0)
        }
    }
}

unsafe impl Send for AuParamEvent {}
unsafe impl Sync for AuParamEvent {}

// ---------- AuHostCallbackInfo ---------- //

pub(super) type AuHostCallbackInfo = ThreadWrapper<au_sys::HostCallbackInfo>;

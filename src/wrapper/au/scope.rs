use atomic_refcell::AtomicRefCell;
use std::num::NonZeroU32;
use std::ptr::{null_mut, NonNull};

use crate::wrapper::au::au_sys;
use crate::wrapper::au::au_types::{AuBufferList, AuConnection, AuRenderCallbackStruct};
use crate::wrapper::au::layout::{layout_tag_from_channels, layout_tag_to_channels};
use crate::wrapper::au::util::{CFString, ChannelPointerVec};
use crate::wrapper::util::buffer_management::ChannelPointers;

// ---------- IoElement ---------- //

struct BufferHandler {
    // TODO: Save buffer size?
    buffer: Option<Vec<f32>>,
    buffer_list: AuBufferList,
    buffer_ptrs: ChannelPointerVec,
}

impl BufferHandler {
    pub fn new(num_channels: NonZeroU32) -> Self {
        Self {
            buffer: None,
            buffer_list: AuBufferList::new(num_channels),
            buffer_ptrs: ChannelPointerVec::from_num_buffers(num_channels),
        }
    }

    fn resize_buffer(&mut self, num_channels: u32, buffer_size: u32) {
        let buffer = self.buffer.get_or_insert_default();
        buffer.resize((num_channels * buffer_size) as _, 0.0);

        self.buffer_list
            .set_num_channels(NonZeroU32::new(num_channels).unwrap());
        self.buffer_ptrs
            .as_mut()
            .resize(num_channels as _, null_mut());
    }

    fn prepare_list(&mut self, num_samples: u32) {
        if let Some(buffer) = self.buffer.as_mut() {
            let num_samples = num_samples as usize;
            let mut offset = 0usize;
            for au_buffer in self.buffer_list.iter_mut() {
                au_buffer.mDataByteSize = (num_samples * size_of::<f32>()) as _;
                au_buffer.mData = unsafe { buffer.as_mut_ptr().add(offset) } as _;
                offset += num_samples;
            }
        } else {
            self.prepare_list_null(num_samples);
        }
    }

    fn prepare_list_null(&mut self, num_samples: u32) {
        let num_samples = num_samples as usize;

        for au_buffer in self.buffer_list.iter_mut() {
            au_buffer.mDataByteSize = (num_samples * size_of::<f32>()) as _;
            au_buffer.mData = null_mut();
        }
    }

    fn create_channel_pointers(&mut self) -> Option<ChannelPointers> {
        let buffer_ptrs = self.buffer_ptrs.as_mut();
        for (i, au_buffer) in self.buffer_list.iter().enumerate() {
            buffer_ptrs[i] = au_buffer.mData as _;
        }

        Some(ChannelPointers {
            ptrs: NonNull::new(buffer_ptrs.as_mut_ptr()).unwrap(),
            num_channels: buffer_ptrs.len(),
        })
    }
}

#[derive(Clone, PartialEq)]
pub(super) enum ShouldAllocate {
    False,
    True,
    // NOTE: Force buffer creation for output elements other than the first one
    //       because they are rendered in one go while the render function is called
    //       for each individual output element.
    Force,
}

// NOTE: The properties and `BufferHandler` must be accessible at the same time.
//       See `AuRenderCallback`.
pub(super) struct IoElement {
    name: CFString,
    stream_format: au_sys::AudioStreamBasicDescription,
    layout_tag: au_sys::AudioChannelLayoutTag,
    should_allocate: ShouldAllocate,
    buffer_handler: AtomicRefCell<BufferHandler>,
}

impl IoElement {
    fn new(name: String, sample_rate: au_sys::Float64, num_channels: NonZeroU32) -> Self {
        // TODO: Support more stream formats.
        let sample_size = size_of::<f32>() as au_sys::UInt32;
        let stream_format = au_sys::AudioStreamBasicDescription {
            mSampleRate: sample_rate,
            mFormatID: au_sys::kAudioFormatLinearPCM,
            mFormatFlags: au_sys::kAudioFormatFlagsNativeFloatPacked
                | au_sys::kAudioFormatFlagIsNonInterleaved,
            mBytesPerPacket: sample_size,
            mFramesPerPacket: 1,
            mBytesPerFrame: sample_size,
            mChannelsPerFrame: num_channels.get(),
            mBitsPerChannel: 8 * sample_size,
            mReserved: 0,
        };

        Self {
            name: CFString::from_str(name.as_str()),
            stream_format,
            layout_tag: layout_tag_from_channels(stream_format.mChannelsPerFrame),
            should_allocate: ShouldAllocate::True, // NOTE: AU default.
            buffer_handler: AtomicRefCell::new(BufferHandler::new(num_channels)),
        }
    }

    // ---------- Properties ---------- //

    pub(super) fn name(&self) -> au_sys::CFStringRef {
        self.name.get()
    }

    pub(super) fn sample_rate(&self) -> au_sys::Float64 {
        self.stream_format.mSampleRate
    }

    pub(super) fn set_sample_rate(&mut self, sample_rate: au_sys::Float64) {
        self.stream_format.mSampleRate = sample_rate;
    }

    pub(super) fn num_channels(&self) -> au_sys::UInt32 {
        self.stream_format.mChannelsPerFrame
    }

    pub(super) fn stream_format(&self) -> &au_sys::AudioStreamBasicDescription {
        &self.stream_format
    }

    pub(super) fn set_stream_format(
        &mut self,
        stream_format: &au_sys::AudioStreamBasicDescription,
        buffer_size: u32,
    ) {
        self.stream_format = *stream_format;
        self.layout_tag = layout_tag_from_channels(self.stream_format.mChannelsPerFrame);
        self.resize_buffer(buffer_size);
    }

    pub(super) fn layout(&self, layout: &mut au_sys::AudioChannelLayout) {
        layout.mChannelLayoutTag = self.layout_tag;
        layout.mChannelBitmap = au_sys::AudioChannelBitmap::default();
        layout.mNumberChannelDescriptions = 0;
        // TODO: Support more than just `mChannelLayoutTag`.
        // layout.mChannelDescriptions = [au_sys::AudioChannelDescription::default()];
    }

    pub(super) fn set_layout(&mut self, layout: &au_sys::AudioChannelLayout, buffer_size: u32) {
        self.layout_tag = layout.mChannelLayoutTag;
        self.stream_format.mChannelsPerFrame = layout_tag_to_channels(self.layout_tag);
        self.resize_buffer(buffer_size);
    }

    pub(super) fn should_allocate(&self) -> ShouldAllocate {
        self.should_allocate.clone()
    }

    pub(super) fn set_should_allocate(&mut self, should_allocate: ShouldAllocate) {
        self.should_allocate = should_allocate;
    }

    // ---------- Buffer ---------- //

    fn resize_buffer(&self, buffer_size: u32) {
        if self.should_allocate == ShouldAllocate::False {
            return;
        }

        let mut buffer_handler = self.buffer_handler.borrow_mut();
        buffer_handler.resize_buffer(self.stream_format.mChannelsPerFrame, buffer_size);
    }

    fn copy_buffer_to(&self, dest: *mut au_sys::AudioBufferList) {
        unsafe {
            self.buffer_handler
                .borrow()
                .buffer_list
                .copy_buffer_to(dest);
        }
    }

    fn prepare_buffer_list(&self, num_samples: u32) {
        self.buffer_handler.borrow_mut().prepare_list(num_samples);
    }

    fn prepare_buffer_list_null(&self, num_samples: u32) {
        self.buffer_handler
            .borrow_mut()
            .prepare_list_null(num_samples);
    }

    fn create_channel_pointers(&self) -> Option<ChannelPointers> {
        self.buffer_handler.borrow_mut().create_channel_pointers()
    }

    fn copy_buffer_list_from(&self, src: *const au_sys::AudioBufferList) {
        unsafe {
            self.buffer_handler.borrow().buffer_list.copy_from(src);
        }
    }

    fn copy_buffer_list_to(&self, dest: *mut au_sys::AudioBufferList) {
        unsafe {
            self.buffer_handler.borrow().buffer_list.copy_to(dest);
        }
    }
}

// ---------- IoElementImpl ---------- //

// TODO: Remove unused functions when they are not needed for sure.
// NOTE: Some boilerplate for not having to type `base()` / `base_mut()` all the time.
pub(super) trait IoElementImpl {
    // ---------- Base ---------- //

    fn base(&self) -> &IoElement;
    fn base_mut(&mut self) -> &mut IoElement;

    // ---------- Properties ---------- //

    fn name(&self) -> au_sys::CFStringRef {
        self.base().name()
    }

    fn sample_rate(&self) -> au_sys::Float64 {
        self.base().sample_rate()
    }

    fn set_sample_rate(&mut self, sample_rate: au_sys::Float64) {
        self.base_mut().set_sample_rate(sample_rate)
    }

    fn num_channels(&self) -> au_sys::UInt32 {
        self.base().num_channels()
    }

    fn stream_format(&self) -> &au_sys::AudioStreamBasicDescription {
        self.base().stream_format()
    }

    fn set_stream_format(
        &mut self,
        stream_format: &au_sys::AudioStreamBasicDescription,
        buffer_size: u32,
    ) {
        self.base_mut()
            .set_stream_format(stream_format, buffer_size)
    }

    fn layout(&self, layout: &mut au_sys::AudioChannelLayout) {
        self.base().layout(layout);
    }

    fn set_layout(&mut self, layout: &au_sys::AudioChannelLayout, buffer_size: u32) {
        self.base_mut().set_layout(layout, buffer_size);
    }

    fn should_allocate(&self) -> ShouldAllocate {
        self.base().should_allocate()
    }

    fn set_should_allocate(&mut self, should_allocate: ShouldAllocate) {
        self.base_mut().set_should_allocate(should_allocate);
    }

    // ---------- Buffer ---------- //

    fn resize_buffer(&self, buffer_size: u32) {
        self.base().resize_buffer(buffer_size);
    }

    fn copy_buffer_to(&self, dest_buffer_list: *mut au_sys::AudioBufferList) {
        self.base().copy_buffer_to(dest_buffer_list);
    }

    fn prepare_buffer_list(&self, num_samples: u32) {
        self.base().prepare_buffer_list(num_samples);
    }

    fn prepare_buffer_list_null(&self, num_samples: u32) {
        self.base().prepare_buffer_list_null(num_samples);
    }

    fn create_channel_pointers(&self) -> Option<ChannelPointers> {
        self.base().create_channel_pointers()
    }

    fn copy_buffer_list_from(&self, src: *const au_sys::AudioBufferList) {
        self.base().copy_buffer_list_from(src);
    }

    fn copy_buffer_list_to(&self, dest: *mut au_sys::AudioBufferList) {
        self.base().copy_buffer_list_to(dest);
    }
}

// ---------- InputElement ---------- //

#[derive(PartialEq)]
enum InputType {
    None,
    Callback,
    Connection,
}

pub(super) struct InputElement {
    base: IoElement,
    input_type: InputType,
    render_callback_struct: Option<AuRenderCallbackStruct>,
    connection: Option<AuConnection>,
}

impl InputElement {
    pub(super) fn new(
        name: String,
        sample_rate: au_sys::Float64,
        num_channels: NonZeroU32,
    ) -> Self {
        Self {
            base: IoElement::new(name, sample_rate, num_channels),
            input_type: InputType::None,
            render_callback_struct: None,
            connection: None,
        }
    }

    pub(super) fn set_render_callback_struct(
        &mut self,
        render_callback_struct: au_sys::AURenderCallbackStruct,
    ) {
        if render_callback_struct.inputProc.is_some() {
            self.input_type = InputType::Callback;
        } else {
            self.input_type = InputType::None;
        }

        self.render_callback_struct = Some(AuRenderCallbackStruct::new(render_callback_struct));
        self.connection = None;
    }

    pub(super) fn set_connection(&mut self, connection: &au_sys::AudioUnitConnection) {
        self.connection = Some(connection.into());
        self.render_callback_struct = None;

        if !connection.sourceAudioUnit.is_null() {
            self.input_type = InputType::Connection;
        } else {
            self.input_type = InputType::None;
        }
    }

    pub(super) fn pull_input(
        &self,
        io_action_flags: *mut au_sys::AudioUnitRenderActionFlags,
        in_time_stamp: *const au_sys::AudioTimeStamp,
        in_bus_num: au_sys::UInt32,
        in_number_frames: au_sys::UInt32,
    ) -> au_sys::OSStatus {
        if self.input_type == InputType::None {
            return au_sys::kAudioUnitErr_NoConnection;
        }

        if self.input_type == InputType::Callback {
            self.base.prepare_buffer_list(in_number_frames);
            let render_callback = self
                .render_callback_struct
                .as_ref()
                .expect("`render_callback` must be `Some`");
            render_callback.call(
                io_action_flags,
                in_time_stamp,
                in_bus_num,
                in_number_frames,
                self.base.buffer_handler.borrow().buffer_list.list(),
            )
        } else {
            self.base.prepare_buffer_list_null(in_number_frames);
            let connection = self
                .connection
                .as_ref()
                .expect("`connection` must be `Some`");
            connection.call(
                io_action_flags,
                in_time_stamp,
                in_number_frames,
                self.base.buffer_handler.borrow().buffer_list.list(),
            )
        }
    }
}

impl IoElementImpl for InputElement {
    fn base(&self) -> &IoElement {
        &self.base
    }
    fn base_mut(&mut self) -> &mut IoElement {
        &mut self.base
    }
}

// ---------- OutputElement ---------- //

pub(super) struct OutputElement {
    base: IoElement,
}

impl OutputElement {
    pub(super) fn new(
        name: String,
        sample_rate: au_sys::Float64,
        num_channels: NonZeroU32,
    ) -> Self {
        Self {
            base: IoElement::new(name, sample_rate, num_channels),
        }
    }
}

impl IoElementImpl for OutputElement {
    fn base(&self) -> &IoElement {
        &self.base
    }
    fn base_mut(&mut self) -> &mut IoElement {
        &mut self.base
    }
}

// ---------- IoScope ---------- //

pub(super) struct IoScope<E: IoElementImpl> {
    pub(super) elements: Vec<E>,
}

impl<E: IoElementImpl> IoScope<E> {
    pub(super) fn new() -> Self {
        Self {
            elements: Vec::new(),
        }
    }

    // ---------- Element ---------- //

    pub(super) fn element(&self, in_element: au_sys::AudioUnitElement) -> Option<&E> {
        self.elements.get(in_element as usize)
    }

    pub(super) fn element_mut(&mut self, in_element: au_sys::AudioUnitElement) -> Option<&mut E> {
        self.elements.get_mut(in_element as usize)
    }

    pub(super) fn map_element<F>(
        &self,
        in_element: au_sys::AudioUnitElement,
        mut f: F,
    ) -> au_sys::OSStatus
    where
        F: FnMut(&E) -> au_sys::OSStatus,
    {
        self.element(in_element)
            .map_or(au_sys::kAudioUnitErr_InvalidElement, |element| f(element))
    }

    pub(super) fn map_element_mut<F>(
        &mut self,
        in_element: au_sys::AudioUnitElement,
        mut f: F,
    ) -> au_sys::OSStatus
    where
        F: FnMut(&mut E) -> au_sys::OSStatus,
    {
        self.element_mut(in_element)
            .map_or(au_sys::kAudioUnitErr_InvalidElement, |element| f(element))
    }
}

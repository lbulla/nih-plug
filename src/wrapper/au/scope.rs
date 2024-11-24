use atomic_refcell::AtomicRefCell;
use std::ffi::c_void;
use std::num::NonZeroU32;
use std::ptr::{null_mut, NonNull};

use crate::wrapper::au::au_types::{AuBufferList, AuConnection, AuRenderCallbackStruct};
use crate::wrapper::au::layout::{layout_tag_from_channels, layout_tag_to_channels};
use crate::wrapper::au::util::{CFString, ChannelPointerVec, ThreadWrapper};
use crate::wrapper::au::{au_sys, NO_ERROR};
use crate::wrapper::util::buffer_management::ChannelPointers;

// ---------- IoElement ---------- //

struct BufferConverter {
    main_sample_rate: au_sys::Float64,
    ref_: ThreadWrapper<au_sys::AudioConverterRef>,
}

impl Drop for BufferConverter {
    fn drop(&mut self) {
        unsafe {
            au_sys::AudioConverterDispose(self.ref_.get());
        }
    }
}

unsafe extern "C" fn convert_buffer(
    _in_audio_converter: au_sys::AudioConverterRef,
    _io_number_data_packets: *mut au_sys::UInt32,
    io_data: *mut au_sys::AudioBufferList,
    _out_data_packet_description: *mut *mut au_sys::AudioStreamPacketDescription,
    in_user_data: *mut c_void,
) -> au_sys::OSStatus {
    let io_element = &*(in_user_data as *const IoElement);
    io_element.copy_buffer_list_to(io_data);
    NO_ERROR
}

struct BufferHandler {
    // TODO: Save buffer size?
    buffer: Option<Vec<f32>>,
    buffer_list: AuBufferList,
    buffer_ptrs: ChannelPointerVec,

    converter: Option<BufferConverter>,
}

impl BufferHandler {
    pub fn new(num_channels: NonZeroU32) -> Self {
        Self {
            buffer: None,
            buffer_list: AuBufferList::new(num_channels),
            buffer_ptrs: ChannelPointerVec::from_num_buffers(num_channels),

            converter: None,
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

    // FIXME: Not sure how to test this properly yet due to its unusual nature.
    //        `auval` passes though.
    // NOTE: `main_sample_rate` => Sample rate of the first output element.
    fn init_converter(
        &mut self,
        stream_format: &au_sys::AudioStreamBasicDescription,
        main_sample_rate: au_sys::Float64,
        is_output: bool,
    ) -> bool {
        // TODO: We check only the sample rate at the moment which is the only thing
        //       that can differ. In future, we might have to check more (e.g. sample size)
        //       and implement a more complex buffer handling.
        //       Plus, we could also implement options like the quality of the resampling.
        if main_sample_rate == stream_format.mSampleRate {
            self.converter = None;
            true
        } else {
            if let Some(converter) = self.converter.as_ref() {
                if main_sample_rate == converter.main_sample_rate {
                    return true;
                }
            }

            let mut main_stream_format = stream_format.clone();
            main_stream_format.mSampleRate = main_sample_rate;

            let success;
            let mut converter_ref = null_mut();
            if is_output {
                success = unsafe {
                    au_sys::AudioConverterNew(
                        &raw const main_stream_format,
                        &raw const *stream_format,
                        &raw mut converter_ref,
                    )
                } == NO_ERROR;
            } else {
                success = unsafe {
                    au_sys::AudioConverterNew(
                        &raw const *stream_format,
                        &raw const main_stream_format,
                        &raw mut converter_ref,
                    )
                } == NO_ERROR;
            }

            if success {
                self.converter = Some(BufferConverter {
                    main_sample_rate,
                    ref_: ThreadWrapper::new(converter_ref),
                });
                true
            } else {
                self.converter = None;
                false
            }
        }
    }

    fn convert_buffer(&self, io_element: &IoElement) -> au_sys::OSStatus {
        if let Some(converter) = self.converter.as_ref() {
            let mut output_packet_size = 1;
            let mut packet_description = au_sys::AudioStreamPacketDescription::default();
            unsafe {
                au_sys::AudioConverterFillComplexBuffer(
                    converter.ref_.get(),
                    Some(convert_buffer),
                    &raw const *io_element as _,
                    &raw mut output_packet_size,
                    self.buffer_list.list(),
                    &raw mut packet_description,
                )
            }
        } else {
            NO_ERROR
        }
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

    fn init_converter(&self, main_sample_rate: au_sys::Float64, is_output: bool) -> bool {
        let mut buffer_handler = self.buffer_handler.borrow_mut();
        buffer_handler.init_converter(&self.stream_format, main_sample_rate, is_output)
    }

    fn convert_buffer(&self) -> au_sys::OSStatus {
        self.buffer_handler.borrow().convert_buffer(self)
    }
}

// ---------- IoElementImpl ---------- //

// NOTE: Some boilerplate for not having to type `base()` / `base_mut()` all the time.
pub(super) trait IoElementImpl {
    // ---------- Base ---------- //

    fn base(&self) -> &IoElement;
    fn base_mut(&mut self) -> &mut IoElement;

    // ---------- Properties ---------- //

    fn num_channels(&self) -> au_sys::UInt32 {
        self.base().num_channels()
    }

    fn stream_format(&self) -> &au_sys::AudioStreamBasicDescription {
        self.base().stream_format()
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

    fn create_channel_pointers(&self) -> Option<ChannelPointers> {
        self.base().create_channel_pointers()
    }

    fn copy_buffer_list_from(&self, src: *const au_sys::AudioBufferList) {
        self.base().copy_buffer_list_from(src);
    }

    fn copy_buffer_list_to(&self, dest: *mut au_sys::AudioBufferList) {
        self.base().copy_buffer_list_to(dest);
    }

    fn init_converter(&self, main_sample_rate: au_sys::Float64, is_output: bool) -> bool {
        self.base().init_converter(main_sample_rate, is_output)
    }

    fn convert_buffer(&self) -> au_sys::OSStatus {
        self.base().convert_buffer()
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

        let os_status;
        if self.input_type == InputType::Callback {
            self.base.prepare_buffer_list(in_number_frames);
            let render_callback = self
                .render_callback_struct
                .as_ref()
                .expect("`render_callback` must be `Some`");
            os_status = render_callback.call(
                io_action_flags,
                in_time_stamp,
                in_bus_num,
                in_number_frames,
                self.base.buffer_handler.borrow().buffer_list.list(),
            );
        } else {
            self.base.prepare_buffer_list_null(in_number_frames);
            let connection = self
                .connection
                .as_ref()
                .expect("`connection` must be `Some`");
            os_status = connection.call(
                io_action_flags,
                in_time_stamp,
                in_number_frames,
                self.base.buffer_handler.borrow().buffer_list.list(),
            );
        }
        if os_status == NO_ERROR {
            self.base.convert_buffer()
        } else {
            os_status
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

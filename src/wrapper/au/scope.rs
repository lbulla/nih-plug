use std::num::NonZeroU32;

use crate::wrapper::au::au_sys;
use crate::wrapper::au::layout::{layout_tag_from_channels, layout_tag_to_channels};
use crate::wrapper::au::util::CFString;

// ---------- IoElement ---------- //

pub(super) struct IoElement {
    name: CFString,
    stream_format: au_sys::AudioStreamBasicDescription,
    layout_tag: au_sys::AudioChannelLayoutTag,
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
    ) {
        self.stream_format = *stream_format;
        self.layout_tag = layout_tag_from_channels(self.stream_format.mChannelsPerFrame);
    }

    pub(super) fn layout(&self, layout: &mut au_sys::AudioChannelLayout) {
        layout.mChannelLayoutTag = self.layout_tag;
        layout.mChannelBitmap = au_sys::AudioChannelBitmap::default();
        layout.mNumberChannelDescriptions = 0;
        // TODO: Support more than just `mChannelLayoutTag`.
        // layout.mChannelDescriptions = [au_sys::AudioChannelDescription::default()];
    }

    pub(super) fn set_layout(&mut self, layout: &au_sys::AudioChannelLayout) {
        self.layout_tag = layout.mChannelLayoutTag;
        self.stream_format.mChannelsPerFrame = layout_tag_to_channels(self.layout_tag);
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

    fn set_stream_format(&mut self, stream_format: &au_sys::AudioStreamBasicDescription) {
        self.base_mut().set_stream_format(stream_format)
    }

    fn layout(&self, layout: &mut au_sys::AudioChannelLayout) {
        self.base().layout(layout);
    }

    fn set_layout(&mut self, layout: &au_sys::AudioChannelLayout) {
        self.base_mut().set_layout(layout);
    }
}

// ---------- InputElement ---------- //

pub(super) struct InputElement {
    base: IoElement,
}

impl InputElement {
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

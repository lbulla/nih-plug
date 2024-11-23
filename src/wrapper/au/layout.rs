use const_for::const_for;
use std::mem::MaybeUninit;

use crate::audio_setup::AudioIOLayout;
use crate::prelude::AuPlugin;
use crate::wrapper::au::au_sys;

// ---------- AudioChannelLayoutTag ---------- //

pub(super) const fn layout_tag_from_channels(
    num_channels: au_sys::UInt32,
) -> au_sys::AudioChannelLayoutTag {
    match num_channels {
        1 => au_sys::kAudioChannelLayoutTag_Mono,
        2 => au_sys::kAudioChannelLayoutTag_Stereo,
        _ => au_sys::kAudioChannelLayoutTag_DiscreteInOrder | num_channels,
    }
}

pub(super) const fn layout_tag_to_channels(
    layout_tag: au_sys::AudioChannelLayoutTag,
) -> au_sys::UInt32 {
    match layout_tag {
        au_sys::kAudioChannelLayoutTag_Mono => 1,
        au_sys::kAudioChannelLayoutTag_Stereo => 2,
        _ => layout_tag & !au_sys::kAudioChannelLayoutTag_DiscreteInOrder,
    }
}

// ---------- AuChannelLayouts ---------- //

pub const MAX_AU_CHANNEL_LAYOUTS: usize = 16; // FIXME: Should be `P::AUDIO_IO_LAYOUTS.len()`
pub const MAX_AU_CHANNEL_CONFIGS: usize = 16;

pub type AuChannelLayout = ConstVec<AuChannelConfig, MAX_AU_CHANNEL_CONFIGS>;
pub type AuChannelLayouts = ConstVec<AuChannelLayout, MAX_AU_CHANNEL_LAYOUTS>;

pub struct AuChannelConfig {
    pub num_inputs: au_sys::UInt32,
    pub input_layout_tag: au_sys::AudioChannelLayoutTag,

    pub num_outputs: au_sys::UInt32,
    pub output_layout_tag: au_sys::AudioChannelLayoutTag,
}

impl AuChannelConfig {
    const fn new(num_inputs: au_sys::UInt32, num_outputs: au_sys::UInt32) -> Self {
        Self {
            num_inputs,
            input_layout_tag: layout_tag_from_channels(num_inputs),

            num_outputs,
            output_layout_tag: layout_tag_from_channels(num_outputs),
        }
    }
}

const fn push_au_config(
    au_layout: &mut AuChannelLayout,
    num_inputs: au_sys::UInt32,
    num_outputs: au_sys::UInt32,
) {
    if num_inputs > 0 || num_outputs > 0 {
        let config = AuChannelConfig::new(num_inputs, num_outputs);
        au_layout.push(config);
    }
}

// TODO: Check for "gaps" (elements without any channels between elements with some).
const fn add_au_channel_layout(au_layouts: &mut AuChannelLayouts, audio_io_layout: &AudioIOLayout) {
    let mut num_inputs;
    if let Some(main_input_channels) = audio_io_layout.main_input_channels {
        num_inputs = main_input_channels.get();
    } else {
        num_inputs = 0;
    }

    let mut num_outputs;
    if let Some(main_output_channels) = audio_io_layout.main_output_channels {
        num_outputs = main_output_channels.get();
    } else {
        num_outputs = 0;
    }

    let mut au_layout = ConstVec::new();
    push_au_config(&mut au_layout, num_inputs, num_outputs);

    let max_num_aux_ports;
    if audio_io_layout.aux_input_ports.len() > audio_io_layout.aux_output_ports.len() {
        max_num_aux_ports = audio_io_layout.aux_input_ports.len();
    } else {
        max_num_aux_ports = audio_io_layout.aux_output_ports.len();
    }

    const_for!(j in 0..max_num_aux_ports => {
        if j < audio_io_layout.aux_input_ports.len() {
            num_inputs = audio_io_layout.aux_input_ports[j].get();
        } else {
            num_inputs = 0;
        }

        if j < audio_io_layout.aux_output_ports.len() {
            num_outputs = audio_io_layout.aux_output_ports[j].get();
        } else {
            num_outputs = 0;
        }

        push_au_config(&mut au_layout, num_inputs, num_outputs);
    });

    au_layouts.push(au_layout);
}

pub const fn au_channel_layouts<P: AuPlugin>() -> AuChannelLayouts {
    let mut au_layouts = AuChannelLayouts::new();

    const_for!(i in 0..P::AUDIO_IO_LAYOUTS.len() => {
        add_au_channel_layout(&mut au_layouts, &P::AUDIO_IO_LAYOUTS[i]);
    });

    au_layouts
}

// ---------- ConstVec ---------- //

pub struct ConstVec<T, const N: usize> {
    data: MaybeUninit<[T; N]>,
    len: usize,
}

impl<T, const N: usize> ConstVec<T, N> {
    pub const fn new() -> Self {
        Self {
            data: MaybeUninit::zeroed(),
            len: 0,
        }
    }

    pub const fn get(&self, idx: usize) -> Option<&T> {
        if idx < self.len {
            unsafe { Some(&*self.as_ptr().add(idx)) }
        } else {
            None
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub const fn as_ptr(&self) -> *const T {
        self.data.as_ptr() as _
    }

    pub const fn as_mut_ptr(&mut self) -> *mut T {
        self.data.as_mut_ptr() as _
    }

    pub const fn push(&mut self, value: T) {
        if self.len() == N {
            panic!("Size `N` exceeded");
        }

        let data = self.as_mut_ptr();
        unsafe {
            data.add(self.len).write(value);
        }
        self.len += 1;
    }

    pub fn iter(&self) -> ConstVecIter<T, N> {
        ConstVecIter { vec: self, idx: 0 }
    }
}

pub struct ConstVecIter<'a, T, const N: usize> {
    vec: &'a ConstVec<T, N>,
    idx: usize,
}

impl<'a, T, const N: usize> Iterator for ConstVecIter<'a, T, N> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.idx < self.vec.len() {
            let result = unsafe { Some(&*self.vec.as_ptr().add(self.idx)) };
            self.idx += 1;
            result
        } else {
            None
        }
    }
}

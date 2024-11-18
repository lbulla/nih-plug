use std::collections::HashSet;

use super::Plugin;
use crate::wrapper::au::au_sys;
use crate::wrapper::au::layout::{au_channel_layouts, AuChannelLayouts};

// ---------- AuPlugin ---------- //

pub trait AuPlugin: Plugin {
    const AU_CHANNEL_LAYOUTS: &'static AuChannelLayouts = &au_channel_layouts::<Self>();

    // TODO: We could make these functions probably const, too.

    fn channel_infos() -> Vec<au_sys::AUChannelInfo> {
        let mut channel_infos = Vec::new();

        for au_layout in Self::AU_CHANNEL_LAYOUTS.iter() {
            for config in au_layout.iter() {
                let channel_info = au_sys::AUChannelInfo {
                    inChannels: config.num_inputs as _,
                    outChannels: config.num_outputs as _,
                };
                channel_infos.push(channel_info);
            }
        }

        channel_infos
    }

    fn layout_tags(
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
    ) -> HashSet<au_sys::AudioChannelLayoutTag> {
        let mut layout_tags = HashSet::new();

        for au_layout in Self::AU_CHANNEL_LAYOUTS.iter() {
            if let Some(config) = au_layout.get(in_element as usize) {
                layout_tags.insert(if in_scope == au_sys::kAudioUnitScope_Input {
                    config.input_layout_tag
                } else {
                    config.output_layout_tag
                });
            }
        }

        layout_tags
    }
}

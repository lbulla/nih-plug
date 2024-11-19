use std::ffi::c_void;
use std::ptr::{copy_nonoverlapping, null};
use std::sync::Arc;

use nih_plug_derive::PropertyDispatcherImpl;

use crate::prelude::AuPlugin;
use crate::wrapper::au::editor::WrapperViewCreator;
use crate::wrapper::au::scope::ShouldAllocate;
use crate::wrapper::au::util::utf8_to_CFStringRef;
use crate::wrapper::au::{au_sys, Wrapper, NO_ERROR};

// ---------- Constants ---------- //

const ANY_SCOPE: &'static [au_sys::AudioUnitScope] = &[
    au_sys::kAudioUnitScope_Global,
    au_sys::kAudioUnitScope_Input,
    au_sys::kAudioUnitScope_Output,
    au_sys::kAudioUnitScope_Group,
    au_sys::kAudioUnitScope_Part,
    au_sys::kAudioUnitScope_Note,
    au_sys::kAudioUnitScope_Layer,
    au_sys::kAudioUnitScope_LayerItem,
];
const GLOBAL_SCOPE: &'static [au_sys::AudioUnitScope] = &[au_sys::kAudioUnitScope_Global];
const IO_SCOPE: &'static [au_sys::AudioUnitScope] = &[
    au_sys::kAudioUnitScope_Input,
    au_sys::kAudioUnitScope_Output,
];

// NOTE: 0 -> 63999 is reserved (see `AudioUnitProperties.h`).
//       But we use a less obvious value than 64000.
const WRAPPER_PROPERTY_ID: au_sys::AudioUnitPropertyID = 0x787725F1;

// ---------- PropertyDispatcher ---------- //

// NOTE: Just used for automatically mapping `au_sys::AudioUnitPropertyID` to properties.
//       Sorted by `au_sys::AudioUnitPropertyID`.
#[derive(PropertyDispatcherImpl)]
#[allow(clippy::enum_variant_names, dead_code)]
pub(super) enum PropertyDispatcher {
    ClassInfoProperty,
    MakeConnectionProperty,
    SampleRateProperty,
    StreamFormatProperty,
    ElementCountProperty,
    LatencyProperty,
    SupportedNumChannelsProperty,
    MaximumFramesPerSliceProperty,
    AudioChannelLayoutProperty,
    TailTimeProperty,
    BypassEffectProperty,
    LastRenderErrorProperty,
    SetRenderCallbackProperty,
    ElementNameProperty,
    CocoaUIProperty,
    SupportedChannelLayoutTagsProperty,
    PresentPresetProperty,
    ShouldAllocateBufferProperty,
    LastRenderSampleTimeProperty,

    // NOTE: Internal property for the editor.
    WrapperProperty,
}

pub(super) trait PropertyDispatcherImpl<P: AuPlugin> {
    fn info(
        id: au_sys::AudioUnitPropertyID,
        wrapper: &Wrapper<P>,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        out_data_size: *mut au_sys::UInt32,
        out_writable: *mut au_sys::Boolean,
    ) -> au_sys::OSStatus;

    fn get(
        id: au_sys::AudioUnitPropertyID,
        wrapper: &Wrapper<P>,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        out_data: *mut c_void,
        io_data_size: *mut au_sys::UInt32,
    ) -> au_sys::OSStatus;

    fn set(
        id: au_sys::AudioUnitPropertyID,
        wrapper: &mut Wrapper<P>,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        in_data: *const c_void,
        in_data_size: au_sys::UInt32,
    ) -> au_sys::OSStatus;
}

// ---------- Property ---------- //

// NOTE: Defaults to read + write.
//       Read / write only properties override `set_data` / `data` for simplicity.
pub(super) trait Property<P: AuPlugin>: seal::PropertyImpl<P> {
    fn info(
        wrapper: &Wrapper<P>,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        out_data_size: *mut au_sys::UInt32,
        out_writable: *mut au_sys::Boolean,
    ) -> au_sys::OSStatus {
        if !Self::SCOPES.contains(&in_scope) {
            return au_sys::kAudioUnitErr_InvalidScope;
        }

        let size = Self::size(wrapper, in_scope, in_element);
        if size == 0 {
            return au_sys::kAudioUnitErr_PropertyNotInUse;
        }

        unsafe {
            if !out_data_size.is_null() {
                *out_data_size = size;
            }
            if !out_writable.is_null() {
                *out_writable = Self::WRITABLE;
            }
        }

        NO_ERROR
    }

    fn get(
        wrapper: &Wrapper<P>,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        out_data: *mut c_void,
        io_data_size: *mut au_sys::UInt32,
    ) -> au_sys::OSStatus {
        if !Self::SCOPES.contains(&in_scope) {
            return au_sys::kAudioUnitErr_InvalidScope;
        }

        let size = Self::size(wrapper, in_scope, in_element);
        if size == 0 {
            return au_sys::kAudioUnitErr_PropertyNotInUse;
        }

        unsafe {
            *io_data_size = size;
        }
        if out_data.is_null() {
            NO_ERROR
        } else {
            Self::get_impl(wrapper, in_scope, in_element, seal::cast_out_data(out_data))
        }
    }

    fn set(
        wrapper: &mut Wrapper<P>,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        in_data: *const c_void,
        in_data_size: au_sys::UInt32,
    ) -> au_sys::OSStatus {
        if !Self::SCOPES.contains(&in_scope) {
            return au_sys::kAudioUnitErr_InvalidScope;
        }

        let os_status;
        if in_data.is_null() && in_data_size == 0 {
            os_status = Self::reset_impl(wrapper, in_scope, in_element);
        } else if in_data_size != Self::size(wrapper, in_scope, in_element) {
            return au_sys::kAudioUnitErr_InvalidPropertyValue;
        } else {
            os_status = Self::set_impl(wrapper, in_scope, in_element, seal::cast_in_data(in_data));
        }

        if os_status == NO_ERROR {
            wrapper.call_property_listeners(Self::ID, in_scope, in_element);
        }
        os_status
    }
}

mod seal {
    use super::*;

    pub trait PropertyImpl<P: AuPlugin>: 'static {
        const ID: au_sys::AudioUnitPropertyID;
        const SCOPES: &'static [au_sys::AudioUnitScope];
        const WRITABLE: au_sys::Boolean;
        type Type;

        // NOTE: Return 0 to indicate that the property is not in use.
        fn size(
            _wrapper: &Wrapper<P>,
            _in_scope: au_sys::AudioUnitScope,
            _in_element: au_sys::AudioUnitElement,
        ) -> au_sys::UInt32 {
            size_of::<Self::Type>() as _
        }

        fn get_impl(
            _wrapper: &Wrapper<P>,
            _in_scope: au_sys::AudioUnitScope,
            _in_element: au_sys::AudioUnitElement,
            _out_data: &mut Self::Type,
        ) -> au_sys::OSStatus {
            au_sys::kAudioUnitErr_InvalidProperty
        }

        fn set_impl(
            _wrapper: &mut Wrapper<P>,
            _in_scope: au_sys::AudioUnitScope,
            _in_element: au_sys::AudioUnitElement,
            _in_data: &Self::Type,
        ) -> au_sys::OSStatus {
            au_sys::kAudioUnitErr_PropertyNotWritable
        }

        fn reset_impl(
            _wrapper: &mut Wrapper<P>,
            _in_scope: au_sys::AudioUnitScope,
            _in_element: au_sys::AudioUnitElement,
        ) -> au_sys::OSStatus {
            // TODO: Implementations.
            au_sys::kAudioUnitErr_PropertyNotWritable
        }
    }

    pub fn cast_out_data<T>(out_data: *mut c_void) -> &'static mut T {
        unsafe { &mut *out_data.cast::<T>() }
    }

    pub fn cast_in_data<T>(in_data: *const c_void) -> &'static T {
        unsafe { &*in_data.cast::<T>() }
    }
}

// ---------- Property implementations ---------- //

macro_rules! declare_property {
    // NOTE: Read only.
    (
        pub(super) struct $name:ident;

        const ID: au_sys::AudioUnitPropertyID = $id:expr;
        const SCOPES: &'static [au_sys::AudioUnitScope] = $scopes:expr;
        type Type = $type:ty;

        fn size(
            $wrapper_size:ident: &Wrapper<P>,
            $in_scope_size:ident: au_sys::AudioUnitScope,
            $in_element_size:ident: au_sys::AudioUnitElement,
        ) -> au_sys::UInt32
            $size:block

        fn get_impl(
            $wrapper_get:ident: &Wrapper<P>,
            $in_scope_get:ident: au_sys::AudioUnitScope,
            $in_element_get:ident: au_sys::AudioUnitElement,
            $out_data_get:ident: &mut Type,
        ) -> au_sys::OSStatus
            $get:block
    ) => {
        pub(super) struct $name;

        impl<P: AuPlugin> seal::PropertyImpl<P> for $name {
            const ID: au_sys::AudioUnitPropertyID = $id;
            const SCOPES: &'static [au_sys::AudioUnitScope] = $scopes;
            const WRITABLE: au_sys::Boolean = 0;
            type Type = $type;

            fn size(
                $wrapper_size: &Wrapper<P>,
                $in_scope_size: au_sys::AudioUnitScope,
                $in_element_size: au_sys::AudioUnitElement,
            ) -> au_sys::UInt32
                $size

            fn get_impl(
                $wrapper_get: &Wrapper<P>,
                $in_scope_get: au_sys::AudioUnitScope,
                $in_element_get: au_sys::AudioUnitElement,
                $out_data_get: &mut Self::Type,
            ) -> au_sys::OSStatus
                $get
        }

        impl<P: AuPlugin> Property<P> for $name {
            fn set(
                _wrapper: &mut Wrapper<P>,
                _in_scope: au_sys::AudioUnitScope,
                _in_element: au_sys::AudioUnitElement,
                _out_data: *const c_void,
                _in_size: au_sys::UInt32,
            ) -> au_sys::OSStatus {
                au_sys::kAudioUnitErr_PropertyNotWritable
            }
        }
    };
    // NOTE: Write only.
    (
        pub(super) struct $name:ident;

        const ID: au_sys::AudioUnitPropertyID = $id:expr;
        const SCOPES: &'static [au_sys::AudioUnitScope] = $scopes:expr;
        type Type = $type:ty;

        fn size(
            $wrapper_size:ident: &Wrapper<P>,
            $in_scope_size:ident: au_sys::AudioUnitScope,
            $in_element_size:ident: au_sys::AudioUnitElement,
        ) -> au_sys::UInt32
            $size:block

        fn set_impl(
            $wrapper_set:ident: &mut Wrapper<P>,
            $in_scope_set:ident: au_sys::AudioUnitScope,
            $in_element_set:ident: au_sys::AudioUnitElement,
            $in_data:ident: &Type,
        ) -> au_sys::OSStatus
            $set:block

        fn reset_impl(
            $wrapper_reset:ident: &mut Wrapper<P>,
            $in_scope_reset:ident: au_sys::AudioUnitScope,
            $in_element_reset:ident: au_sys::AudioUnitElement,
        ) -> au_sys::OSStatus
            $reset:block
    ) => {
        pub(super) struct $name;

        impl<P: AuPlugin> seal::PropertyImpl<P> for $name {
            const ID: au_sys::AudioUnitPropertyID = $id;
            const SCOPES: &'static [au_sys::AudioUnitScope] = $scopes;
            const WRITABLE: au_sys::Boolean = 1;
            type Type = $type;

            fn size(
                $wrapper_size: &Wrapper<P>,
                $in_scope_size: au_sys::AudioUnitScope,
                $in_element_size: au_sys::AudioUnitElement,
            ) -> au_sys::UInt32
                $size

            fn set_impl(
                $wrapper_set: &mut Wrapper<P>,
                $in_scope_set: au_sys::AudioUnitScope,
                $in_element_set: au_sys::AudioUnitElement,
                $in_data: &Self::Type,
            ) -> au_sys::OSStatus
                $set

            fn reset_impl(
                $wrapper_reset: &mut Wrapper<P>,
                $in_scope_reset: au_sys::AudioUnitScope,
                $in_element_reset: au_sys::AudioUnitElement,
            ) -> au_sys::OSStatus
                $reset
        }

        impl<P: AuPlugin> Property<P> for $name {
            fn get(
                _wrapper: &Wrapper<P>,
                _in_scope: au_sys::AudioUnitScope,
                _in_element: au_sys::AudioUnitElement,
                _out_data: *mut c_void,
                _io_data_size: *mut au_sys::UInt32,
            ) -> au_sys::OSStatus {
                au_sys::kAudioUnitErr_InvalidProperty
            }
        }
    };
    // NOTE: Read + write.
    (
        pub(super) struct $name:ident;

        const ID: au_sys::AudioUnitPropertyID = $id:expr;
        const SCOPES: &'static [au_sys::AudioUnitScope] = $scopes:expr;
        type Type = $type:ty;

        fn size(
            $wrapper_size:ident: &Wrapper<P>,
            $in_scope_size:ident: au_sys::AudioUnitScope,
            $in_element_size:ident: au_sys::AudioUnitElement,
        ) -> au_sys::UInt32
            $size:block

        fn get_impl(
            $wrapper_get:ident: &Wrapper<P>,
            $in_scope_get:ident: au_sys::AudioUnitScope,
            $in_element_get:ident: au_sys::AudioUnitElement,
            $out_data:ident: &mut Type,
        ) -> au_sys::OSStatus
            $get:block

        fn set_impl(
            $wrapper_set:ident: &mut Wrapper<P>,
            $in_scope_set:ident: au_sys::AudioUnitScope,
            $in_element_set:ident: au_sys::AudioUnitElement,
            $in_data:ident: &Type,
        ) -> au_sys::OSStatus
            $set:block

        fn reset_impl(
            $wrapper_reset:ident: &mut Wrapper<P>,
            $in_scope_reset:ident: au_sys::AudioUnitScope,
            $in_element_reset:ident: au_sys::AudioUnitElement,
        ) -> au_sys::OSStatus
            $reset:block
    ) => {
        pub(super) struct $name;

        impl<P: AuPlugin> seal::PropertyImpl<P> for $name {
            const ID: au_sys::AudioUnitPropertyID = $id;
            const SCOPES: &'static [au_sys::AudioUnitScope] = $scopes;
            const WRITABLE: au_sys::Boolean = 1;
            type Type = $type;

            fn size(
                $wrapper_size: &Wrapper<P>,
                $in_scope_size: au_sys::AudioUnitScope,
                $in_element_size: au_sys::AudioUnitElement,
            ) -> au_sys::UInt32
                $size

            fn get_impl(
                $wrapper_get: &Wrapper<P>,
                $in_scope_get: au_sys::AudioUnitScope,
                $in_element_get: au_sys::AudioUnitElement,
                $out_data: &mut Self::Type,
            ) -> au_sys::OSStatus
                $get

            fn set_impl(
                $wrapper_set: &mut Wrapper<P>,
                $in_scope_set: au_sys::AudioUnitScope,
                $in_element_set: au_sys::AudioUnitElement,
                $in_data: &Self::Type,
            ) -> au_sys::OSStatus
                $set

            fn reset_impl(
                $wrapper_reset: &mut Wrapper<P>,
                $in_scope_reset: au_sys::AudioUnitScope,
                $in_element_reset: au_sys::AudioUnitElement,
            ) -> au_sys::OSStatus
                $reset
        }

        impl<P: AuPlugin> Property<P> for $name {}
    };
}

macro_rules! default_size {
    () => {
        size_of::<Self::Type>() as _
    };
}

declare_property!(
    pub(super) struct ClassInfoProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_ClassInfo;
    const SCOPES: &'static [au_sys::AudioUnitScope] = GLOBAL_SCOPE;
    type Type = au_sys::CFDictionaryRef;

    fn size(
        _wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::UInt32 {
        default_size!()
    }

    fn get_impl(
        wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        out_data: &mut Type,
    ) -> au_sys::OSStatus {
        // TODO
        unsafe {
            let comp = au_sys::AudioComponentInstanceGetComponent(wrapper.unit());
            let mut desc = au_sys::AudioComponentDescription::default();
            let os_status = au_sys::AudioComponentGetDescription(comp, &raw mut desc);
            if os_status != NO_ERROR {
                return os_status;
            }

            let dict = au_sys::CFDictionaryCreateMutable(
                au_sys::kCFAllocatorDefault,
                5,
                &raw const au_sys::kCFTypeDictionaryKeyCallBacks,
                &raw const au_sys::kCFTypeDictionaryValueCallBacks,
            );

            let add_num = |key: &[u8], num: au_sys::SInt32| {
                let key = utf8_to_CFStringRef(key);
                let value = au_sys::CFNumberCreate(
                    au_sys::kCFAllocatorDefault,
                    au_sys::kCFNumberSInt32Type as _,
                    &raw const num as _,
                );
                au_sys::CFDictionarySetValue(dict, key as _, value as _);
            };

            add_num(au_sys::kAUPresetVersionKey, 0);
            add_num(au_sys::kAUPresetTypeKey, desc.componentType as _);
            add_num(au_sys::kAUPresetSubtypeKey, desc.componentSubType as _);
            add_num(
                au_sys::kAUPresetManufacturerKey,
                desc.componentManufacturer as _,
            );

            let name_key = utf8_to_CFStringRef(au_sys::kAUPresetNameKey);
            let name_value = wrapper.preset().presetName;
            au_sys::CFDictionarySetValue(dict, name_key as _, name_value as _);

            *out_data = dict;
        }
        NO_ERROR
    }

    fn set_impl(
        _wrapper: &mut Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        _in_data: &Type,
    ) -> au_sys::OSStatus {
        // TODO
        au_sys::kAudioUnitErr_PropertyNotWritable
    }

    fn reset_impl(
        _wrapper: &mut Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::OSStatus {
        au_sys::kAudioUnitErr_PropertyNotWritable
    }
);

declare_property!(
    pub(super) struct MakeConnectionProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_MakeConnection;
    const SCOPES: &'static [au_sys::AudioUnitScope] = &[au_sys::kAudioUnitScope_Input];
    type Type = au_sys::AudioUnitConnection;

    fn size(
        _wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::UInt32 {
        default_size!()
    }

    fn set_impl(
        wrapper: &mut Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        in_data: &Type,
    ) -> au_sys::OSStatus {
        wrapper
            .input_scope
            .write()
            .map_element_mut(in_data.destInputNumber, |element| {
                element.set_connection(in_data);
                NO_ERROR
            })
    }

    fn reset_impl(
        _wrapper: &mut Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::OSStatus {
        au_sys::kAudioUnitErr_PropertyNotWritable
    }
);

// TODO: Independent sample rates for each element
//       though that might be uncommon for most hosts.
//       (AU API specifications)
//       Converter: https://developer.apple.com/documentation/audiotoolbox/1502936-audioconverternew?language=objc

declare_property!(
    pub(super) struct SampleRateProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_SampleRate;
    const SCOPES: &'static [au_sys::AudioUnitScope] = IO_SCOPE;
    type Type = au_sys::Float64;

    fn size(
        _wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::UInt32 {
        default_size!()
    }

    fn get_impl(
        wrapper: &Wrapper<P>,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        out_data: &mut Type,
    ) -> au_sys::OSStatus {
        wrapper.map_element(in_scope, in_element, |element| {
            *out_data = element.sample_rate();
            NO_ERROR
        })
    }

    fn set_impl(
        wrapper: &mut Wrapper<P>,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        in_data: &Type,
    ) -> au_sys::OSStatus {
        if wrapper.initialized() {
            return au_sys::kAudioUnitErr_Initialized;
        }

        let os_status = wrapper.map_element_mut(in_scope, in_element, |element| {
            element.set_sample_rate(*in_data);
            NO_ERROR
        });

        if os_status == NO_ERROR {
            if in_scope == au_sys::kAudioUnitScope_Output && in_element == 0 {
                wrapper.set_sample_rate(*in_data as _);
            }
        }
        os_status
    }

    fn reset_impl(
        _wrapper: &mut Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::OSStatus {
        au_sys::kAudioUnitErr_PropertyNotWritable
    }
);

declare_property!(
    pub(super) struct StreamFormatProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_StreamFormat;
    const SCOPES: &'static [au_sys::AudioUnitScope] = IO_SCOPE;
    type Type = au_sys::AudioStreamBasicDescription;

    fn size(
        _wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::UInt32 {
        default_size!()
    }

    fn get_impl(
        wrapper: &Wrapper<P>,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        out_data: &mut Type,
    ) -> au_sys::OSStatus {
        wrapper.map_element(in_scope, in_element, |element| {
            *out_data = *element.stream_format();
            NO_ERROR
        })
    }

    fn set_impl(
        wrapper: &mut Wrapper<P>,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        in_data: &Type,
    ) -> au_sys::OSStatus {
        if wrapper.initialized() {
            return au_sys::kAudioUnitErr_Initialized;
        }

        let buffer_size = wrapper.buffer_size();
        let os_status = wrapper.map_element_mut(in_scope, in_element, |element| {
            let current_format = element.stream_format();

            // NOTE: Check simple stuff first before checking the channels.
            if in_data.mFormatID != current_format.mFormatID
                || in_data.mFormatFlags != current_format.mFormatFlags
                || in_data.mBytesPerPacket != current_format.mBytesPerPacket
                || in_data.mFramesPerPacket != current_format.mFramesPerPacket
                || in_data.mBytesPerPacket != current_format.mBytesPerPacket
                || in_data.mBitsPerChannel != current_format.mBitsPerChannel
            {
                return au_sys::kAudioUnitErr_FormatNotSupported;
            }
            if in_data.mSampleRate == current_format.mSampleRate
                && in_data.mChannelsPerFrame == current_format.mChannelsPerFrame
            {
                return NO_ERROR;
            }

            // TODO: We could make this probably const, too.
            let mut supported_num_channels = Vec::new();
            for au_layout in P::AU_CHANNEL_LAYOUTS.iter() {
                if let Some(config) = au_layout.get(in_element as usize) {
                    supported_num_channels.push(if in_scope == au_sys::kAudioUnitScope_Input {
                        config.num_inputs
                    } else {
                        config.num_outputs
                    });
                }
            }
            if !supported_num_channels.contains(&in_data.mChannelsPerFrame) {
                return au_sys::kAudioUnitErr_FormatNotSupported;
            }

            element.set_stream_format(in_data, buffer_size);
            NO_ERROR
        });

        if os_status == NO_ERROR {
            if in_scope == au_sys::kAudioUnitScope_Output && in_element == 0 {
                wrapper.set_sample_rate(in_data.mSampleRate as _);
            }
        }
        os_status
    }

    fn reset_impl(
        _wrapper: &mut Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::OSStatus {
        au_sys::kAudioUnitErr_PropertyNotWritable
    }
);

declare_property!(
    pub(super) struct ElementCountProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_ElementCount;
    const SCOPES: &'static [au_sys::AudioUnitScope] = ANY_SCOPE;
    type Type = au_sys::UInt32;

    fn size(
        _wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::UInt32 {
        default_size!()
    }

    fn get_impl(
        wrapper: &Wrapper<P>,
        in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        out_data: &mut Type,
    ) -> au_sys::OSStatus {
        if let Some(num_elements) = wrapper.num_elements(in_scope) {
            *out_data = num_elements as _;
            NO_ERROR
        } else {
            au_sys::kAudioUnitErr_InvalidScope
        }
    }
);

declare_property!(
    pub(super) struct LatencyProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_Latency;
    const SCOPES: &'static [au_sys::AudioUnitScope] = GLOBAL_SCOPE;
    type Type = au_sys::Float64; // NOTE: Seconds.

    fn size(
        _wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::UInt32 {
        default_size!()
    }

    fn get_impl(
        wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        out_data: &mut Type,
    ) -> au_sys::OSStatus {
        *out_data = wrapper.latency_seconds();
        NO_ERROR
    }
);

declare_property!(
    pub(super) struct SupportedNumChannelsProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_SupportedNumChannels;
    const SCOPES: &'static [au_sys::AudioUnitScope] = GLOBAL_SCOPE;
    type Type = au_sys::AUChannelInfo;

    fn size(
        _wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::UInt32 {
        (P::channel_infos().len() * size_of::<Self::Type>()) as _
    }

    fn get_impl(
        _wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        out_data: &mut Type,
    ) -> au_sys::OSStatus {
        let channel_infos = P::channel_infos();
        unsafe {
            copy_nonoverlapping::<u8>(
                channel_infos.as_ptr() as _,
                &raw mut *out_data as _,
                channel_infos.len() * size_of::<Self::Type>(),
            );
        }
        NO_ERROR
    }
);

declare_property!(
    pub(super) struct MaximumFramesPerSliceProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_MaximumFramesPerSlice;
    const SCOPES: &'static [au_sys::AudioUnitScope] = GLOBAL_SCOPE;
    type Type = au_sys::UInt32;

    fn size(
        _wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::UInt32 {
        default_size!()
    }

    fn get_impl(
        wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        out_data: &mut Type,
    ) -> au_sys::OSStatus {
        *out_data = wrapper.buffer_size() as _;
        NO_ERROR
    }

    fn set_impl(
        wrapper: &mut Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        in_data: &Type,
    ) -> au_sys::OSStatus {
        if wrapper.initialized() {
            au_sys::kAudioUnitErr_Initialized
        } else {
            wrapper.set_buffer_size(*in_data);
            NO_ERROR
        }
    }

    fn reset_impl(
        _wrapper: &mut Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::OSStatus {
        au_sys::kAudioUnitErr_PropertyNotWritable
    }
);

// TODO: Support more than just `mChannelLayoutTag`.
declare_property!(
    pub(super) struct AudioChannelLayoutProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_AudioChannelLayout;
    const SCOPES: &'static [au_sys::AudioUnitScope] = IO_SCOPE;
    type Type = au_sys::AudioChannelLayout;

    fn size(
        _wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::UInt32 {
        (size_of::<Self::Type>() - size_of::<au_sys::AudioChannelDescription>()) as _
    }

    fn get_impl(
        wrapper: &Wrapper<P>,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        out_data: &mut Type,
    ) -> au_sys::OSStatus {
        wrapper.map_element(in_scope, in_element, |element| {
            element.layout(out_data);
            NO_ERROR
        })
    }

    fn set_impl(
        wrapper: &mut Wrapper<P>,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        in_data: &Type,
    ) -> au_sys::OSStatus {
        let buffer_size = wrapper.buffer_size();
        wrapper.map_element_mut(in_scope, in_element, |element| {
            let layout_tags = P::layout_tags(in_scope, in_element);
            if layout_tags.contains(&in_data.mChannelLayoutTag) {
                element.set_layout(in_data, buffer_size);
                NO_ERROR
            } else {
                au_sys::kAudioUnitErr_FormatNotSupported
            }
        })
    }

    fn reset_impl(
        _wrapper: &mut Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::OSStatus {
        au_sys::kAudioUnitErr_PropertyNotWritable
    }
);

declare_property!(
    pub(super) struct TailTimeProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_TailTime;
    const SCOPES: &'static [au_sys::AudioUnitScope] = GLOBAL_SCOPE;
    type Type = au_sys::Float64; // NOTE: Seconds.

    fn size(
        _wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::UInt32 {
        default_size!()
    }

    fn get_impl(
        wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        out_data: &mut Type,
    ) -> au_sys::OSStatus {
        *out_data = wrapper.tail_seconds();
        NO_ERROR
    }
);

declare_property!(
    pub(super) struct BypassEffectProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_BypassEffect;
    const SCOPES: &'static [au_sys::AudioUnitScope] = GLOBAL_SCOPE;
    type Type = au_sys::UInt32;

    fn size(
        _wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::UInt32 {
        default_size!()
    }

    fn get_impl(
        _wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        out_data: &mut Type,
    ) -> au_sys::OSStatus {
        // TODO
        *out_data = false as _;
        NO_ERROR
    }

    fn set_impl(
        _wrapper: &mut Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        _in_data: &Type,
    ) -> au_sys::OSStatus {
        // TODO
        au_sys::kAudioUnitErr_PropertyNotWritable
    }

    fn reset_impl(
        _wrapper: &mut Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::OSStatus {
        au_sys::kAudioUnitErr_PropertyNotWritable
    }
);

declare_property!(
    pub(super) struct LastRenderErrorProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_LastRenderError;
    const SCOPES: &'static [au_sys::AudioUnitScope] = GLOBAL_SCOPE;
    type Type = au_sys::OSStatus;

    fn size(
        _wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::UInt32 {
        default_size!()
    }

    fn get_impl(
        wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        out_data: &mut Type,
    ) -> au_sys::OSStatus {
        *out_data = wrapper.last_render_error();
        NO_ERROR
    }
);

declare_property!(
    pub(super) struct SetRenderCallbackProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_SetRenderCallback;
    const SCOPES: &'static [au_sys::AudioUnitScope] = &[au_sys::kAudioUnitScope_Input];
    type Type = au_sys::AURenderCallbackStruct;

    fn size(
        _wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::UInt32 {
        default_size!()
    }

    fn set_impl(
        wrapper: &mut Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        in_data: &Type,
    ) -> au_sys::OSStatus {
        wrapper
            .input_scope
            .write()
            .map_element_mut(in_element, |element| {
                element.set_render_callback_struct(*in_data);
                NO_ERROR
            })
    }

    fn reset_impl(
        _wrapper: &mut Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::OSStatus {
        au_sys::kAudioUnitErr_PropertyNotWritable
    }
);

declare_property!(
    pub(super) struct ElementNameProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_ElementName;
    const SCOPES: &'static [au_sys::AudioUnitScope] = ANY_SCOPE;
    type Type = au_sys::CFStringRef;

    fn size(
        _wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::UInt32 {
        default_size!()
    }

    fn get_impl(
        wrapper: &Wrapper<P>,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        out_data: &mut Type,
    ) -> au_sys::OSStatus {
        wrapper.map_element(in_scope, in_element, |element| {
            let name = element.name();
            unsafe { au_sys::CFRetain(name as _) };
            *out_data = name;
            NO_ERROR
        })
    }
);

declare_property!(
    pub(super) struct CocoaUIProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_CocoaUI;
    const SCOPES: &'static [au_sys::AudioUnitScope] = GLOBAL_SCOPE;
    type Type = au_sys::AudioUnitCocoaViewInfo;

    fn size(
        wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::UInt32 {
        if wrapper.has_editor() {
            default_size!()
        } else {
            0
        }
    }

    fn get_impl(
        wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        out_data: &mut Type,
    ) -> au_sys::OSStatus {
        nih_debug_assert!(
            wrapper.has_editor(),
            "This should have been prevented by the size of 0"
        );
        out_data.mCocoaAUViewBundleLocation = WrapperViewCreator::<P>::bundle_location();
        out_data.mCocoaAUViewClass[0] = WrapperViewCreator::<P>::class_name();
        NO_ERROR
    }
);

declare_property!(
    pub(super) struct SupportedChannelLayoutTagsProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_SupportedChannelLayoutTags;
    const SCOPES: &'static [au_sys::AudioUnitScope] = IO_SCOPE;
    type Type = au_sys::AudioChannelLayoutTag;

    fn size(
        _wrapper: &Wrapper<P>,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
    ) -> au_sys::UInt32 {
        (P::layout_tags(in_scope, in_element).len() * size_of::<Self::Type>()) as _
    }

    fn get_impl(
        _wrapper: &Wrapper<P>,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        out_data: &mut Type,
    ) -> au_sys::OSStatus {
        let layout_tags = P::layout_tags(in_scope, in_element);
        let out_data = &raw mut *out_data;
        for (i, layout_tag) in layout_tags.iter().enumerate() {
            unsafe {
                *out_data.add(i) = *layout_tag;
            }
        }
        NO_ERROR
    }
);

declare_property!(
    pub(super) struct PresentPresetProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_PresentPreset;
    const SCOPES: &'static [au_sys::AudioUnitScope] = GLOBAL_SCOPE;
    type Type = au_sys::AUPreset;

    fn size(
        _wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::UInt32 {
        default_size!()
    }

    fn get_impl(
        wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        out_data: &mut Type,
    ) -> au_sys::OSStatus {
        // TODO
        *out_data = *wrapper.preset();
        unsafe {
            au_sys::CFRetain(out_data.presetName as _);
        }
        NO_ERROR
    }

    fn set_impl(
        wrapper: &mut Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        in_data: &Type,
    ) -> au_sys::OSStatus {
        // TODO
        wrapper.set_preset(*in_data);
        NO_ERROR
    }

    fn reset_impl(
        _wrapper: &mut Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::OSStatus {
        au_sys::kAudioUnitErr_PropertyNotWritable
    }
);

declare_property!(
    pub(super) struct ShouldAllocateBufferProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_ShouldAllocateBuffer;
    const SCOPES: &'static [au_sys::AudioUnitScope] = IO_SCOPE;
    type Type = au_sys::UInt32;

    fn size(
        _wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::UInt32 {
        default_size!()
    }

    fn get_impl(
        wrapper: &Wrapper<P>,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        out_data: &mut Type,
    ) -> au_sys::OSStatus {
        wrapper.map_element(in_scope, in_element, |element| {
            match element.should_allocate() {
                ShouldAllocate::False => *out_data = 0,
                _ => *out_data = 1,
            }
            NO_ERROR
        })
    }

    fn set_impl(
        wrapper: &mut Wrapper<P>,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        in_data: &Type,
    ) -> au_sys::OSStatus {
        if wrapper.initialized() {
            return au_sys::kAudioUnitErr_Initialized;
        }

        wrapper.map_element_mut(in_scope, in_element, |element| {
            match element.should_allocate() {
                ShouldAllocate::Force => au_sys::kAudioUnitErr_PropertyNotWritable,
                _ => {
                    element.set_should_allocate(if *in_data > 0 {
                        ShouldAllocate::True
                    } else {
                        ShouldAllocate::False
                    });
                    NO_ERROR
                }
            }
        })
    }

    fn reset_impl(
        _wrapper: &mut Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::OSStatus {
        au_sys::kAudioUnitErr_PropertyNotWritable
    }
);

declare_property!(
    pub(super) struct LastRenderSampleTimeProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_LastRenderSampleTime;
    const SCOPES: &'static [au_sys::AudioUnitScope] = GLOBAL_SCOPE;
    type Type = au_sys::Float64;

    fn size(
        _wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::UInt32 {
        default_size!()
    }

    fn get_impl(
        wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        out_data: &mut Type,
    ) -> au_sys::OSStatus {
        *out_data = wrapper.last_render_sample_time();
        NO_ERROR
    }
);

declare_property!(
    pub(super) struct WrapperProperty;

    const ID: au_sys::AudioUnitPropertyID = WRAPPER_PROPERTY_ID;
    const SCOPES: &'static [au_sys::AudioUnitScope] = GLOBAL_SCOPE;
    type Type = *const Wrapper<P>;

    fn size(
        _wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::UInt32 {
        default_size!()
    }

    fn get_impl(
        wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        out_data: &mut Type,
    ) -> au_sys::OSStatus {
        *out_data = Arc::into_raw(wrapper.as_arc());
        NO_ERROR
    }
);

pub(super) fn wrapper_for_audio_unit<P: AuPlugin>(
    audio_unit: au_sys::AudioUnit,
) -> Option<Arc<Wrapper<P>>> {
    let mut wrapper = null::<Wrapper<P>>();
    let mut property_size = size_of_val(&wrapper) as au_sys::UInt32;

    let result = unsafe {
        au_sys::AudioUnitGetProperty(
            audio_unit,
            WRAPPER_PROPERTY_ID,
            au_sys::kAudioUnitScope_Global,
            0,
            &raw mut wrapper as _,
            &raw mut property_size,
        )
    };

    if result == NO_ERROR {
        unsafe { Some(Arc::from_raw(wrapper)) }
    } else {
        None
    }
}

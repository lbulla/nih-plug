use std::ffi::c_void;
use std::ptr::{copy_nonoverlapping, null, slice_from_raw_parts};
use std::sync::{Arc, LazyLock};

use nih_plug_derive::PropertyDispatcherImpl;

use crate::midi::MidiConfig;
use crate::prelude::{AuPlugin, Param, ParamFlags, ParamPtr};
use crate::wrapper::au::editor::WrapperViewCreator;
use crate::wrapper::au::scope::ShouldAllocate;
use crate::wrapper::au::util::{
    retain_CFStringRef, str_to_CFStringRef, utf8_to_const_CFStringRef, value_strings_for_param,
    CFStringRef_to_string,
};
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

// ---------- Preset ---------- //

const AU_PRESET_VERSION: au_sys::SInt32 = 0;

struct ClassInfoKeys {
    version: *const c_void,
    type_: *const c_void,
    subtype: *const c_void,
    manufacturer: *const c_void,
    data: *const c_void,
    name: *const c_void,
}

unsafe impl Send for ClassInfoKeys {}
unsafe impl Sync for ClassInfoKeys {}

static KEYS: LazyLock<ClassInfoKeys> = LazyLock::new(|| ClassInfoKeys {
    version: utf8_to_const_CFStringRef(au_sys::kAUPresetVersionKey) as _,
    type_: utf8_to_const_CFStringRef(au_sys::kAUPresetTypeKey) as _,
    subtype: utf8_to_const_CFStringRef(au_sys::kAUPresetSubtypeKey) as _,
    manufacturer: utf8_to_const_CFStringRef(au_sys::kAUPresetManufacturerKey) as _,
    data: utf8_to_const_CFStringRef(au_sys::kAUPresetDataKey) as _,
    name: utf8_to_const_CFStringRef(au_sys::kAUPresetNameKey) as _,
});

// ---------- PropertyDispatcher ---------- //

// NOTE: Just used for automatically mapping `au_sys::AudioUnitPropertyID` to properties.
//       Sorted by `au_sys::AudioUnitPropertyID`.
#[derive(PropertyDispatcherImpl)]
#[allow(clippy::enum_variant_names, dead_code)]
pub(super) enum PropertyDispatcher {
    ClassInfoProperty,
    MakeConnectionProperty,
    SampleRateProperty,
    ParameterListProperty,
    ParameterInfoProperty,
    StreamFormatProperty,
    ElementCountProperty,
    LatencyProperty,
    SupportedNumChannelsProperty,
    MaximumFramesPerSliceProperty,
    ParameterValueStrings,
    AudioChannelLayoutProperty,
    TailTimeProperty,
    BypassEffectProperty,
    LastRenderErrorProperty,
    SetRenderCallbackProperty,
    HostCallbacksProperty,
    ElementNameProperty,
    CocoaUIProperty,
    SupportedChannelLayoutTagsProperty,
    ParameterStringFromValueProperty,
    ParameterClumpNameProperty,
    PresentPresetProperty,
    ParameterValueFromStringProperty,
    MidiOutputCallbackInfoProperty,
    MidiOutputCallbackProperty,
    MidiOutputEventListCallbackProperty,
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
        unsafe {
            let (desc, os_status) = wrapper.get_desc();
            if os_status != NO_ERROR {
                return os_status;
            }

            let dict = au_sys::CFDictionaryCreateMutable(
                au_sys::kCFAllocatorDefault,
                6,
                &raw const au_sys::kCFTypeDictionaryKeyCallBacks,
                &raw const au_sys::kCFTypeDictionaryValueCallBacks,
            );

            let set_num = |key: *const c_void, num: au_sys::SInt32| {
                let value = au_sys::CFNumberCreate(
                    au_sys::kCFAllocatorDefault,
                    au_sys::kCFNumberSInt32Type as _,
                    &raw const num as _,
                );
                au_sys::CFDictionarySetValue(dict, key, value as _);
            };

            set_num(KEYS.version, AU_PRESET_VERSION);
            set_num(KEYS.type_, desc.componentType as _);
            set_num(KEYS.subtype, desc.componentSubType as _);
            set_num(KEYS.manufacturer, desc.componentManufacturer as _);

            let data = wrapper.get_state_json();
            let data_value =
                au_sys::CFDataCreate(au_sys::kCFAllocatorDefault, data.as_ptr(), data.len() as _);
            au_sys::CFDictionarySetValue(dict, KEYS.data, data_value as _);

            let name_value = wrapper.preset().presetName;
            au_sys::CFDictionarySetValue(dict, KEYS.name, name_value as _);

            *out_data = dict;
            os_status
        }
    }

    fn set_impl(
        wrapper: &mut Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        in_data: &Type,
    ) -> au_sys::OSStatus {
        unsafe {
            let (desc, mut os_status) = wrapper.get_desc();
            if os_status != NO_ERROR {
                return os_status;
            }

            let dict = *in_data;

            let check_num = |key: *const c_void, target: au_sys::SInt32| {
                let value = au_sys::CFDictionaryGetValue(dict, key);
                if value.is_null() {
                    return au_sys::kAudioUnitErr_InvalidPropertyValue;
                }

                let mut number = -1 as au_sys::SInt32;
                au_sys::CFNumberGetValue(
                    value as _,
                    au_sys::kCFNumberSInt32Type as _,
                    &raw mut number as _,
                );

                if number != target {
                    au_sys::kAudioUnitErr_InvalidPropertyValue
                } else {
                    NO_ERROR
                }
            };

            os_status = check_num(KEYS.version, AU_PRESET_VERSION);
            if os_status != NO_ERROR {
                return os_status;
            }

            os_status = check_num(KEYS.type_, desc.componentType as _);
            if os_status != NO_ERROR {
                return os_status;
            }

            os_status = check_num(KEYS.subtype, desc.componentSubType as _);
            if os_status != NO_ERROR {
                return os_status;
            }

            os_status = check_num(KEYS.manufacturer, desc.componentManufacturer as _);
            if os_status != NO_ERROR {
                return os_status;
            }

            let data_value = au_sys::CFDictionaryGetValue(dict, KEYS.data);
            if data_value.is_null() {
                return au_sys::kAudioUnitErr_InvalidPropertyValue;
            }
            let data = au_sys::CFDataGetBytePtr(data_value as _);
            if data.is_null() {
                return au_sys::kAudioUnitErr_InvalidPropertyValue;
            }
            let data_len = au_sys::CFDataGetLength(data_value as _);
            let data_slice = slice_from_raw_parts(data, data_len as usize);
            if !wrapper.set_state_json(&*data_slice) {
                return au_sys::kAudioUnitErr_InvalidPropertyValue;
            }

            let name_value = au_sys::CFDictionaryGetValue(dict, KEYS.name) as au_sys::CFStringRef;
            if name_value.is_null() {
                // NOTE: Use the default preset.
                wrapper.set_preset(None);
            } else {
                wrapper.set_preset(Some(au_sys::AUPreset {
                    presetNumber: -1,
                    presetName: name_value,
                }));
            }
            wrapper.call_property_listeners(
                au_sys::kAudioUnitProperty_PresentPreset,
                au_sys::kAudioUnitScope_Global,
                0,
            );

            os_status
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
    pub(super) struct ParameterListProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_ParameterList;
    const SCOPES: &'static [au_sys::AudioUnitScope] = GLOBAL_SCOPE;
    type Type = au_sys::AudioUnitParameterID;

    fn size(
        wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::UInt32 {
        (size_of::<Self::Type>() * wrapper.param_hashes().len()) as _
    }

    fn get_impl(
        wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        out_data: &mut Type,
    ) -> au_sys::OSStatus {
        let out_data = &raw mut *out_data;
        for (i, hash) in wrapper.param_hashes().enumerate() {
            unsafe { *out_data.add(i) = *hash };
        }
        NO_ERROR
    }
);

// FIXME: `auval` warns about small rounding errors for float parameters
//        (Parameter did not retain default value when set).
//        I suppose that is due to the normalized representation and the 32 bit float type.
//        Example: `crossover` plugin => Parameter `Crossover 3`.
declare_property!(
    pub(super) struct ParameterInfoProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_ParameterInfo;
    const SCOPES: &'static [au_sys::AudioUnitScope] = GLOBAL_SCOPE;
    type Type = au_sys::AudioUnitParameterInfo;

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
        in_element: au_sys::AudioUnitElement,
        out_data: &mut Type,
    ) -> au_sys::OSStatus {
        if let Some(wrapper_param) = wrapper.param_hash_to_param(&in_element) {
            out_data.name.fill(0); // NOTE: Unused (legacy).

            out_data.unitName = null();
            out_data.clumpID = wrapper_param.group_hash;
            out_data.minValue = 0.0;
            out_data.flags = au_sys::kAudioUnitParameterFlag_HasCFNameString
                | au_sys::kAudioUnitParameterFlag_CFNameRelease
                | au_sys::kAudioUnitParameterFlag_IsHighResolution
                | au_sys::kAudioUnitParameterFlag_IsReadable;
            if out_data.clumpID != au_sys::kAudioUnitClumpID_System {
                out_data.flags |= au_sys::kAudioUnitParameterFlag_HasClump;
            }

            // NOTE: Skip some matches this way.
            let flags;
            match wrapper_param.ptr {
                ParamPtr::BoolParam(param) => {
                    let param = unsafe { &*param };
                    flags = param.flags();

                    out_data.cfNameString = str_to_CFStringRef(param.name());
                    out_data.unit = au_sys::kAudioUnitParameterUnit_Boolean;
                    out_data.minValue = 0.0;
                    out_data.maxValue = 1.0;
                    out_data.defaultValue = param.default_normalized_value();
                    out_data.flags |= au_sys::kAudioUnitParameterFlag_ValuesHaveStrings;
                }
                ParamPtr::EnumParam(param) => {
                    let param = unsafe { &*param };
                    flags = param.flags();

                    out_data.cfNameString = str_to_CFStringRef(param.name());
                    out_data.unit = au_sys::kAudioUnitParameterUnit_Indexed;
                    out_data.maxValue = param.step_count().unwrap_or(1) as _;
                    out_data.defaultValue = param.default_normalized_value() * out_data.maxValue;
                    out_data.flags |= au_sys::kAudioUnitParameterFlag_ValuesHaveStrings;

                    if P::SAMPLE_ACCURATE_AUTOMATION {
                        out_data.flags |= au_sys::kAudioUnitParameterFlag_CanRamp;
                    }
                }
                ParamPtr::FloatParam(param) => {
                    let param = unsafe { &*param };
                    flags = param.flags();

                    out_data.cfNameString = str_to_CFStringRef(param.name());
                    out_data.unit = au_sys::kAudioUnitParameterUnit_Generic;
                    out_data.maxValue = param.step_count().unwrap_or(1) as _;
                    out_data.defaultValue = param.default_normalized_value() * out_data.maxValue;

                    if P::SAMPLE_ACCURATE_AUTOMATION {
                        out_data.flags |= au_sys::kAudioUnitParameterFlag_CanRamp;
                    }
                }
                ParamPtr::IntParam(param) => {
                    let param = unsafe { &*param };
                    flags = param.flags();

                    out_data.cfNameString = str_to_CFStringRef(param.name());
                    out_data.unit = au_sys::kAudioUnitParameterUnit_Indexed;
                    out_data.maxValue = param.step_count().unwrap_or(1) as _;
                    out_data.defaultValue = param.default_normalized_value() * out_data.maxValue;
                    out_data.flags |= au_sys::kAudioUnitParameterFlag_ValuesHaveStrings;

                    if P::SAMPLE_ACCURATE_AUTOMATION {
                        out_data.flags |= au_sys::kAudioUnitParameterFlag_CanRamp;
                    }
                }
            }

            let non_automatable = flags.contains(ParamFlags::NON_AUTOMATABLE);
            let hidden = flags.contains(ParamFlags::HIDDEN);
            if non_automatable || hidden {
                out_data.flags |= au_sys::kAudioUnitParameterFlag_NonRealTime
            }
            if !hidden {
                out_data.flags |= au_sys::kAudioUnitParameterFlag_IsWritable;
            }

            NO_ERROR
        } else {
            au_sys::kAudioUnitErr_InvalidParameter
        }
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

declare_property!(
    pub(super) struct ParameterValueStrings;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_ParameterValueStrings;
    const SCOPES: &'static [au_sys::AudioUnitScope] = GLOBAL_SCOPE;
    type Type = au_sys::CFArrayRef;

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
        in_element: au_sys::AudioUnitElement,
        out_data: &mut Type,
    ) -> au_sys::OSStatus {
        if let Some(wrapper_param) = wrapper.param_hash_to_param(&in_element) {
            match wrapper_param.ptr {
                ParamPtr::BoolParam(param) => {
                    let param = unsafe { &*param };
                    *out_data = value_strings_for_param(param);
                    NO_ERROR
                }
                ParamPtr::EnumParam(param) => {
                    let param = unsafe { &*param };
                    *out_data = value_strings_for_param(param);
                    NO_ERROR
                }
                ParamPtr::FloatParam(_) => au_sys::kAudioUnitErr_PropertyNotInUse,
                ParamPtr::IntParam(param) => {
                    let param = unsafe { &*param };
                    *out_data = value_strings_for_param(param);
                    NO_ERROR
                }
            }
        } else {
            au_sys::kAudioUnitErr_InvalidParameter
        }
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
        wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        out_data: &mut Type,
    ) -> au_sys::OSStatus {
        *out_data = wrapper.bypassed() as _;
        NO_ERROR
    }

    fn set_impl(
        wrapper: &mut Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        in_data: &Type,
    ) -> au_sys::OSStatus {
        wrapper.set_bypassed(*in_data > 0);
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
    pub(super) struct HostCallbacksProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_HostCallbacks;
    const SCOPES: &'static [au_sys::AudioUnitScope] = GLOBAL_SCOPE;
    type Type = au_sys::HostCallbackInfo;

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
        wrapper.set_host_callback_info(*in_data);
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
    pub(super) struct ParameterStringFromValueProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_ParameterStringFromValue;
    const SCOPES: &'static [au_sys::AudioUnitScope] = GLOBAL_SCOPE;
    type Type = au_sys::AudioUnitParameterStringFromValue;

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
        if let Some(wrapper_param) = wrapper.param_hash_to_param(&out_data.inParamID) {
            unsafe {
                out_data.outString = str_to_CFStringRef(
                    wrapper_param
                        .ptr
                        .normalized_value_to_string(
                            *out_data.inValue / wrapper_param.ptr.step_count().unwrap_or(1) as f32,
                            true,
                        )
                        .as_str(),
                );
            }
            NO_ERROR
        } else {
            au_sys::kAudioUnitErr_InvalidParameter
        }
    }
);

declare_property!(
    pub(super) struct ParameterClumpNameProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_ParameterClumpName;
    const SCOPES: &'static [au_sys::AudioUnitScope] = GLOBAL_SCOPE;
    type Type = au_sys::AudioUnitParameterIDName;

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
        if let Some(group) = wrapper.group_hash_to_group(&out_data.inID) {
            let mut group = group.clone();
            group.truncate(out_data.inDesiredLength as _);
            out_data.outName = str_to_CFStringRef(group.as_str());
            NO_ERROR
        } else {
            au_sys::kAudioUnitErr_InvalidPropertyValue
        }
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
        *out_data = *wrapper.preset();
        retain_CFStringRef(out_data.presetName);
        NO_ERROR
    }

    fn set_impl(
        wrapper: &mut Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        in_data: &Type,
    ) -> au_sys::OSStatus {
        wrapper.set_preset(Some(*in_data));
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
    pub(super) struct ParameterValueFromStringProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_ParameterValueFromString;
    const SCOPES: &'static [au_sys::AudioUnitScope] = GLOBAL_SCOPE;
    type Type = au_sys::AudioUnitParameterValueFromString;

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
        if let Some(wrapper_param) = wrapper.param_hash_to_param(&out_data.inParamID) {
            unsafe {
                if let Some(value) = wrapper_param
                    .ptr
                    .string_to_normalized_value(CFStringRef_to_string(out_data.inString).as_str())
                {
                    out_data.outValue = value * wrapper_param.ptr.step_count().unwrap_or(1) as f32;
                } else {
                    return au_sys::kAudioUnitErr_InvalidParameterValue;
                }
            }
            NO_ERROR
        } else {
            au_sys::kAudioUnitErr_InvalidParameter
        }
    }
);

declare_property!(
    pub(super) struct MidiOutputCallbackInfoProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_MIDIOutputCallbackInfo;
    const SCOPES: &'static [au_sys::AudioUnitScope] = GLOBAL_SCOPE;
    type Type = au_sys::CFArrayRef;

    fn size(
        _wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::UInt32 {
        if P::MIDI_OUTPUT == MidiConfig::None {
            0
        } else {
            default_size!()
        }
    }

    fn get_impl(
        _wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        out_data: &mut Type,
    ) -> au_sys::OSStatus {
        nih_debug_assert!(
            P::MIDI_OUTPUT != MidiConfig::None,
            "This should have been prevented by the size of 0"
        );

        let string = utf8_to_const_CFStringRef(b"MIDI Output\0");
        unsafe {
            let array = au_sys::CFArrayCreate(
                au_sys::kCFAllocatorDefault,
                &raw const string as _,
                1,
                &raw const au_sys::kCFTypeArrayCallBacks,
            );
            *out_data = array;
        }

        NO_ERROR
    }
);

declare_property!(
    pub(super) struct MidiOutputCallbackProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_MIDIOutputCallback;
    const SCOPES: &'static [au_sys::AudioUnitScope] = GLOBAL_SCOPE;
    type Type = au_sys::AUMIDIOutputCallbackStruct;

    fn size(
        _wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::UInt32 {
        if P::MIDI_OUTPUT == MidiConfig::None {
            0
        } else {
            default_size!()
        }
    }

    fn set_impl(
        wrapper: &mut Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        in_data: &Type,
    ) -> au_sys::OSStatus {
        nih_debug_assert!(
            P::MIDI_OUTPUT != MidiConfig::None,
            "This should have been prevented by the size of 0"
        );
        wrapper.set_midi_output_callback_struct(*in_data);
        NO_ERROR
    }

    fn reset_impl(
        _wrapper: &mut Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::OSStatus {
        nih_debug_assert!(
            P::MIDI_OUTPUT != MidiConfig::None,
            "This should have been prevented by the size of 0"
        );
        au_sys::kAudioUnitErr_PropertyNotWritable
    }
);

declare_property!(
    pub(super) struct MidiOutputEventListCallbackProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_MIDIOutputEventListCallback;
    const SCOPES: &'static [au_sys::AudioUnitScope] = GLOBAL_SCOPE;
    type Type = au_sys::AUMIDIEventListBlock;

    fn size(
        _wrapper: &Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::UInt32 {
        if P::MIDI_OUTPUT == MidiConfig::None {
            0
        } else {
            default_size!()
        }
    }

    fn set_impl(
        wrapper: &mut Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
        in_data: &Type,
    ) -> au_sys::OSStatus {
        nih_debug_assert!(
            P::MIDI_OUTPUT != MidiConfig::None,
            "This should have been prevented by the size of 0"
        );
        wrapper.set_midi_output_callback_block(*in_data);
        NO_ERROR
    }

    fn reset_impl(
        _wrapper: &mut Wrapper<P>,
        _in_scope: au_sys::AudioUnitScope,
        _in_element: au_sys::AudioUnitElement,
    ) -> au_sys::OSStatus {
        nih_debug_assert!(
            P::MIDI_OUTPUT != MidiConfig::None,
            "This should have been prevented by the size of 0"
        );
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

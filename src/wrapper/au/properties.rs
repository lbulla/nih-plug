use std::ffi::c_void;

use nih_plug_derive::PropertyDispatcherImpl;

use crate::prelude::AuPlugin;
use crate::wrapper::au::{au_sys, Wrapper, NO_ERROR};

// ---------- Constants ---------- //

const GLOBAL_SCOPE: &'static [au_sys::AudioUnitScope] = &[au_sys::kAudioUnitScope_Global];

// ---------- PropertyDispatcher ---------- //

// NOTE: Just used for automatically mapping `au_sys::AudioUnitPropertyID` to properties.
//       Sorted by `au_sys::AudioUnitPropertyID`.
#[derive(PropertyDispatcherImpl)]
#[allow(clippy::enum_variant_names, dead_code)]
pub(super) enum PropertyDispatcher {
    CocoaUIProperty,
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
    pub(super) struct CocoaUIProperty;

    const ID: au_sys::AudioUnitPropertyID = au_sys::kAudioUnitProperty_CocoaUI;
    const SCOPES: &'static [au_sys::AudioUnitScope] = GLOBAL_SCOPE;
    type Type = au_sys::AudioUnitCocoaViewInfo;

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
        *out_data = au_sys::AudioUnitCocoaViewInfo::default();
        NO_ERROR
    }
);

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{
    AnyClass, AnyObject, AnyProtocol, ClassBuilder, MessageReceiver, NSObject, Sel,
};
use objc2::{msg_send_id, sel, ClassType, Encode, Encoding, Message, RefEncode};
use objc2_app_kit::{NSEvent, NSView};
use objc2_foundation::{NSBundle, NSPoint, NSRect, NSSize, NSString};
use std::any::Any;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ptr::{null, null_mut};
use std::sync::{Arc, OnceLock};

use crate::prelude::AuPlugin;
use crate::wrapper::au::au_sys;
use crate::wrapper::au::properties::wrapper_for_audio_unit;
use crate::wrapper::au::Wrapper;

// ---------- ClassBuilder ---------- //

// NOTE: All classes must be built dynamically
//       because unique class names are required for each plugin.
//       (`objc_allocateClassPair` fails for duplicate names)
fn create_class_builder<P: AuPlugin>(class_name_root: &str, superclass: &AnyClass) -> ClassBuilder {
    let class_name = class_name_root.to_owned() + &P::NAME.replace(" ", "_");
    ClassBuilder::new(class_name.as_str(), superclass)
        .expect("A class with the name `{class_name}` likely already exists")
}

// ---------- WrapperViewCreator ---------- //

pub(super) struct WrapperViewCreator<P: AuPlugin> {
    p: PhantomData<P>,
}

impl<P: AuPlugin> WrapperViewCreator<P> {
    // NOTE: Might be the host's bundle and not the plugin's.
    pub(super) fn bundle_location() -> au_sys::CFURLRef {
        let bundle_url = unsafe { NSBundle::bundleForClass(Self::class()).bundleURL() };
        Self::manually_drop_return(bundle_url)
    }

    pub(super) fn class_name() -> au_sys::CFStringRef {
        let class_name = NSString::from_str(Self::class().name());
        Self::manually_drop_return(class_name)
    }

    extern "C" fn ui_view_for_audio_unit(
        _this: &NSObject,
        _sel: Sel,
        audio_unit: AudioUnit,
        size: NSSize,
    ) -> *mut NSView {
        if let Some(wrapper) = wrapper_for_audio_unit::<P>(audio_unit.0) {
            let frame = NSRect::new(NSPoint::ZERO, size);
            unsafe { Retained::autorelease_return(WrapperView::new(wrapper, frame)) }
        } else {
            null_mut()
        }
    }

    extern "C" fn interface_version(_: &NSObject, _: Sel) -> u32 {
        0
    }

    extern "C" fn description(_this: &NSObject, _sel: Sel) -> *const NSString {
        let description = NSString::from_str(P::NAME);
        Retained::autorelease_return(description)
    }

    // NOTE: Used for objects which are destroyed by the host.
    fn manually_drop_return<O: Message, R>(obj: Retained<O>) -> *const R {
        Retained::as_ptr(&*ManuallyDrop::new(obj)) as _
    }

    fn class() -> &'static AnyClass {
        static CLASS: OnceLock<&'static AnyClass> = OnceLock::new();

        CLASS.get_or_init(|| {
            let mut class_builder =
                create_class_builder::<P>("nih_plug_au_view_creator_", NSObject::class());
            class_builder.add_protocol(AnyProtocol::get("AUCocoaUIBase").unwrap());

            unsafe {
                class_builder.add_method(
                    sel!(uiViewForAudioUnit:withSize:),
                    WrapperViewCreator::<P>::ui_view_for_audio_unit
                        as extern "C" fn(_, _, _, _) -> _,
                );
                class_builder.add_method(
                    sel!(interfaceVersion),
                    WrapperViewCreator::<P>::interface_version as extern "C" fn(_, _) -> _,
                );
                class_builder.add_method(
                    sel!(description),
                    WrapperViewCreator::<P>::description as extern "C" fn(_, _) -> _,
                );
            }

            class_builder.register()
        })
    }
}

// ---------- WrapperView ---------- //

struct WrapperView<P: AuPlugin> {
    p: PhantomData<P>,
}

impl<P: AuPlugin> WrapperView<P> {
    unsafe fn new(wrapper: Arc<Wrapper<P>>, frame: NSRect) -> Retained<NSView> {
        let this: Allocated<NSView> = msg_send_id![Self::class(), alloc];
        let this: Retained<NSView> = msg_send_id![this, initWithFrame: frame];

        let editor_handle = wrapper.spawn_editor(&this);
        let editor_handle = Box::new(EditorHandle(editor_handle));
        EditorHandleIvar::set(this.as_ref(), Box::into_raw(editor_handle));

        this
    }

    unsafe extern "C" fn dealloc(this: &NSView, _sel: Sel) {
        let editor_handle = EditorHandleIvar::get(this.as_ref());
        let _ = Box::from_raw(*editor_handle); // NOTE: Drop the editor handle.
        *editor_handle = null_mut();

        let () = this.send_super_message(NSView::class(), sel!(dealloc), ());
    }

    // NOTE: This is required for the editor to get the keyboard focus (first responder).
    extern "C" fn hit_test(this: &NSView, _sel: Sel, point: NSPoint) -> *const NSView {
        let subviews = unsafe { this.subviews() };
        if let Some(subview) = subviews.first_retained() {
            nih_debug_assert_eq!(
                subviews.len(),
                1,
                "There must be only one subview for `WrapperView`"
            );

            // FIXME: `NSPointInRect` is not implemented yet in `objc2`.
            let frame = this.frame();
            if point.x >= frame.origin.x
                && point.y >= frame.origin.y
                && point.x <= frame.origin.x + frame.size.width
                && point.y <= frame.origin.y + frame.size.height
            {
                return Retained::as_ptr(&subview);
            }
        }
        null()
    }

    // NOTE: Do nothing. Otherwise, the key events would be sent to the superview
    //       and we do not want that.
    extern "C" fn key_down(_this: &NSView, _sel: Sel, _event: &NSEvent) {}
    extern "C" fn key_up(_this: &NSView, _sel: Sel, _event: &NSEvent) {}

    fn class() -> &'static AnyClass {
        static CLASS: OnceLock<&'static AnyClass> = OnceLock::new();

        CLASS.get_or_init(|| {
            let mut class_builder = create_class_builder::<P>("nih_plug_au_view_", NSView::class());

            EditorHandleIvar::add_to_class(&mut class_builder);
            unsafe {
                class_builder.add_method(
                    sel!(dealloc),
                    WrapperView::<P>::dealloc as unsafe extern "C" fn(_, _),
                );
                class_builder.add_method(
                    sel!(hitTest:),
                    WrapperView::<P>::hit_test as extern "C" fn(_, _, _) -> _,
                );
                class_builder.add_method(
                    sel!(keyDown:),
                    WrapperView::<P>::key_down as extern "C" fn(_, _, _),
                );
                class_builder.add_method(
                    sel!(keyUp:),
                    WrapperView::<P>::key_up as extern "C" fn(_, _, _),
                );
            }

            class_builder.register()
        })
    }
}

// ---------- WrapperViewHolder ---------- //

#[derive(Default)]
pub(super) struct WrapperViewHolder {
    view: Weak<NSView>,
}

impl WrapperViewHolder {
    pub(super) fn init(&mut self, view: &Retained<NSView>, size: (u32, u32)) {
        nih_debug_assert!(
            !self.has_view(),
            "`WrapperViewHolder` has got a view already"
        );

        Self::resize_impl(view, size);
        self.view = Weak::from_retained(view);
    }

    pub(super) fn has_view(&self) -> bool {
        self.view.load().is_some()
    }

    pub(super) fn resize(&self, size: (u32, u32)) {
        if let Some(view) = self.view.load() {
            Self::resize_impl(&view, size)
        }
    }

    fn resize_impl(view: &Retained<NSView>, size: (u32, u32)) {
        unsafe {
            view.setFrameSize(NSSize::new(size.0 as _, size.1 as _));
        }
    }
}

unsafe impl Send for WrapperViewHolder {}
unsafe impl Sync for WrapperViewHolder {}

// ---------- EditorHandle ---------- //

#[allow(dead_code)]
struct EditorHandle(Box<dyn Any + Send>);

unsafe impl Encode for EditorHandle {
    const ENCODING: Encoding = Encoding::Struct("EditorHandle", &[]);
}

unsafe impl RefEncode for EditorHandle {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
}

struct EditorHandleIvar;

impl IvarWrapper for EditorHandleIvar {
    const NAME: &'static str = "editor_handle";
    type Type = *mut EditorHandle;
}

// ---------- Ivar ---------- //

trait IvarWrapper {
    const NAME: &'static str;
    type Type: Encode;

    fn add_to_class(class_builder: &mut ClassBuilder) {
        class_builder.add_ivar::<Self::Type>(Self::NAME);
    }

    fn get(object: &AnyObject) -> *mut Self::Type {
        let ivar = object.class().instance_variable(Self::NAME).unwrap();
        unsafe { ivar.load_ptr::<Self::Type>(object) }
    }

    fn set(object: &AnyObject, value: Self::Type) {
        unsafe { Self::get(object).write(value) };
    }
}

// ---------- AudioUnit ---------- //

#[repr(C)]
struct AudioUnit(au_sys::AudioUnit);

unsafe impl Encode for AudioUnit {
    const ENCODING: Encoding = Encoding::Pointer(&Encoding::Void);
}

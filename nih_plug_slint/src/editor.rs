use nih_plug::prelude::{Editor, GuiContext, ParentWindowHandle};
use plugin_canvas::dimensions::LogicalSize;
use plugin_canvas::window::WindowAttributes;
use plugin_canvas_slint::editor::EditorHandle as RawEditorHandle;
use plugin_canvas_slint::editor::SlintEditor as RawSlintEditor;
use plugin_canvas_slint::window_adapter::PluginCanvasWindowAdapter;
use raw_window_handle::{
    AppKitWindowHandle, RawWindowHandle, WebWindowHandle, Win32WindowHandle, XcbWindowHandle,
};
use std::any::Any;
use std::num::{NonZeroIsize, NonZeroU32};
use std::ptr::NonNull;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::{ParamEvent, SlintEditor, SlintState};

pub(crate) struct SlintEditorWrapper<E: SlintEditor + 'static> {
    pub(crate) slint_state: Arc<SlintState>,
    pub(crate) editor: Arc<E>,
}

impl<E: SlintEditor + 'static> Editor for SlintEditorWrapper<E> {
    fn spawn(
        &self,
        parent: ParentWindowHandle,
        context: Arc<dyn GuiContext>,
    ) -> Box<dyn Any + Send> {
        let parent = match parent {
            ParentWindowHandle::X11Window(window) => {
                RawWindowHandle::Xcb(XcbWindowHandle::new(NonZeroU32::new(window).unwrap()))
            }
            ParentWindowHandle::AppKitNsView(ns_view) => {
                RawWindowHandle::AppKit(AppKitWindowHandle::new(NonNull::new(ns_view).unwrap()))
            }
            ParentWindowHandle::Win32Hwnd(hwnd) => RawWindowHandle::Win32(Win32WindowHandle::new(
                NonZeroIsize::new(hwnd as isize).unwrap(),
            )),
            ParentWindowHandle::Web(id) => RawWindowHandle::Web(WebWindowHandle::new(id)),
        };
        let raw_handle = RawSlintEditor::open(
            parent,
            WindowAttributes::new(self.slint_state.size(), self.slint_state.scale_factor()),
            {
                let editor = self.editor.clone();
                let context = context.clone();
                move |window| editor.build(context.clone(), window)
            },
        );

        let handle = Arc::new(EditorHandle {
            raw_handle,
            slint_state: self.slint_state.clone(),
            context,
        });
        self.editor.on_created(&handle);
        self.slint_state.open.store(true, Ordering::Release);

        Box::new(EditorHandleWrapper { _handle: handle })
    }

    fn size(&self) -> (u32, u32) {
        let size = self
            .slint_state
            .size()
            .to_physical(self.slint_state.scale_factor());
        (size.width, size.height)
    }

    fn set_scale_factor(&self, factor: f32) -> bool {
        self.slint_state.scale_factor.store(factor as _);
        true
    }

    fn param_value_changed(&self, id: &str, normalized_value: f32) {
        self.editor.on_param_event(ParamEvent::ValueChanged {
            id,
            normalized_value,
        });
    }

    fn param_modulation_changed(&self, id: &str, modulation_offset: f32) {
        self.editor.on_param_event(ParamEvent::ModChanged {
            id,
            modulation_offset,
        });
    }

    fn param_values_changed(&self) {
        self.editor.on_param_event(ParamEvent::ValuesChanged);
    }
}

struct EditorHandleWrapper {
    _handle: Arc<EditorHandle>,
}

pub struct EditorHandle {
    raw_handle: Arc<RawEditorHandle>,
    context: Arc<dyn GuiContext>,
    slint_state: Arc<SlintState>,
}

impl EditorHandle {
    pub fn window_adapter(&self) -> Option<&PluginCanvasWindowAdapter> {
        self.raw_handle.window_adapter()
    }

    pub fn scale_factor(&self) -> f64 {
        self.slint_state.scale_factor()
    }

    pub fn set_scale_factor(&self, factor: f64) {
        self.raw_handle.set_scale(factor);

        self.slint_state.scale_factor.store(factor);
        if let Some(adapter) = self.window_adapter() {
            let size = adapter.logical_size();
            self.slint_state
                .size
                .store(LogicalSize::new(size.width as _, size.height as _));
        }

        self.context.request_resize();
    }
}

impl Drop for EditorHandle {
    fn drop(&mut self) {
        self.slint_state.open.store(false, Ordering::Release);
    }
}

unsafe impl Send for EditorHandle {}
unsafe impl Sync for EditorHandle {}

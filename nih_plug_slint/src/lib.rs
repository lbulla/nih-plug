use crossbeam::atomic::AtomicCell;
use nih_plug::params::persist::PersistentField;
use nih_plug::prelude::{Editor, GuiContext};
use plugin_canvas::dimensions::LogicalSize;
pub use plugin_canvas::event::EventResponse;
pub use plugin_canvas::Event;
pub use plugin_canvas::Window;
pub use plugin_canvas_slint::view::PluginView;
pub use plugin_canvas_slint::window_adapter::PluginCanvasWindowAdapter;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub use crate::editor::EditorHandle;
use crate::editor::SlintEditorWrapper;

pub mod assets;
mod editor;

pub fn create_slint_editor<B>(slint_state: Arc<SlintState>, editor: B) -> Option<Box<dyn Editor>>
where
    B: SlintEditor + 'static,
{
    Some(Box::new(SlintEditorWrapper {
        slint_state,
        editor: Arc::new(editor),
    }))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SlintState {
    #[serde(with = "nih_plug::params::persist::serialize_atomic_cell")]
    size: AtomicCell<LogicalSize>,

    #[serde(with = "nih_plug::params::persist::serialize_atomic_cell")]
    scale_factor: AtomicCell<f64>,

    #[serde(skip)]
    open: AtomicBool,
}

impl SlintState {
    pub fn new(width: f64, height: f64, scale_factor: f64) -> Arc<SlintState> {
        Arc::new(SlintState {
            size: AtomicCell::new(LogicalSize::new(width, height)),
            scale_factor: AtomicCell::new(scale_factor),
            open: AtomicBool::new(false),
        })
    }

    pub fn size(&self) -> LogicalSize {
        self.size.load()
    }

    pub fn scale_factor(&self) -> f64 {
        self.scale_factor.load()
    }

    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire)
    }
}

impl<'a> PersistentField<'a, SlintState> for Arc<SlintState> {
    fn set(&self, new_value: SlintState) {
        self.size.store(new_value.size.load());
        self.scale_factor.store(new_value.scale_factor.load());
    }

    fn map<F, R>(&self, f: F) -> R
    where
        F: Fn(&SlintState) -> R,
    {
        f(self)
    }
}

pub trait SlintEditor: Send + Sync {
    type View: PluginView;

    fn build(&self, context: Arc<dyn GuiContext>, window: Arc<Window>) -> Self::View;
    fn on_created(&self, handle: &Arc<EditorHandle>);
    fn on_param_event(&self, event: ParamEvent);
}

pub enum ParamEvent<'a> {
    ValueChanged { id: &'a str, normalized_value: f32 },
    ModChanged { id: &'a str, modulation_offset: f32 },
    ValuesChanged,
}

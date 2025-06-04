use plugin_canvas_slint::slint::platform::WindowAdapter;
use plugin_canvas_slint::window_adapter::PluginCanvasWindowAdapter;
use std::error::Error;

pub use nih_plug_assets::*;

pub fn register_noto_sans_regular(
    adapter: &PluginCanvasWindowAdapter,
) -> Result<(), Box<dyn Error>> {
    adapter
        .renderer()
        .register_font_from_memory(fonts::NOTO_SANS_REGULAR)
}

pub fn register_noto_sans_regular_italic(
    adapter: &PluginCanvasWindowAdapter,
) -> Result<(), Box<dyn Error>> {
    adapter
        .renderer()
        .register_font_from_memory(fonts::NOTO_SANS_REGULAR_ITALIC)
}

pub fn register_noto_sans_thin(adapter: &PluginCanvasWindowAdapter) -> Result<(), Box<dyn Error>> {
    adapter
        .renderer()
        .register_font_from_memory(fonts::NOTO_SANS_THIN)
}

pub fn register_noto_sans_thin_italic(
    adapter: &PluginCanvasWindowAdapter,
) -> Result<(), Box<dyn Error>> {
    adapter
        .renderer()
        .register_font_from_memory(fonts::NOTO_SANS_THIN_ITALIC)
}

pub fn register_noto_sans_light(adapter: &PluginCanvasWindowAdapter) -> Result<(), Box<dyn Error>> {
    adapter
        .renderer()
        .register_font_from_memory(fonts::NOTO_SANS_LIGHT)
}

pub fn register_noto_sans_light_italic(
    adapter: &PluginCanvasWindowAdapter,
) -> Result<(), Box<dyn Error>> {
    adapter
        .renderer()
        .register_font_from_memory(fonts::NOTO_SANS_LIGHT_ITALIC)
}

pub fn register_noto_sans_bold(adapter: &PluginCanvasWindowAdapter) -> Result<(), Box<dyn Error>> {
    adapter
        .renderer()
        .register_font_from_memory(fonts::NOTO_SANS_BOLD)
}

pub fn register_noto_sans_bold_italic(
    adapter: &PluginCanvasWindowAdapter,
) -> Result<(), Box<dyn Error>> {
    adapter
        .renderer()
        .register_font_from_memory(fonts::NOTO_SANS_BOLD_ITALIC)
}

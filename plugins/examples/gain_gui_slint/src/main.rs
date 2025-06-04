mod editor;
mod gain;

use nih_plug::prelude::nih_export_standalone;

use gain::Gain;

#[cfg(not(target_arch = "wasm32"))]
pub fn main() {
    nih_export_standalone::<Gain>();
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn main() {
    nih_export_standalone::<Gain>();
}

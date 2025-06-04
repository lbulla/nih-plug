use nih_plug::prelude::*;

use gain_gui_slint::Gain;

pub fn main() {
    nih_export_standalone::<Gain>();
}

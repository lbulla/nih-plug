use atomic_refcell::AtomicRefCell;
use instant::{Duration, Instant};
use nih_plug::prelude::{AtomicF32, Editor, GuiContext, Param, Sample};
use nih_plug::{nih_error, util};
use nih_plug_slint::*;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::gain::GainParams;

slint::include_modules!();

// Makes sense to also define this here, makes it a bit easier to keep track of
pub(crate) fn default_state() -> Arc<SlintState> {
    SlintState::new(200.0, 150.0, 1.0)
}

pub(crate) fn create(
    params: Arc<GainParams>,
    peak_meter: Arc<AtomicF32>,
    editor_state: Arc<SlintState>,
) -> Option<Box<dyn Editor>> {
    create_slint_editor(
        editor_state,
        GainEditorBuilder {
            params,
            peak_meter,
            gain_window: Default::default(),
        },
    )
}

struct GainEditorBuilder {
    params: Arc<GainParams>,
    peak_meter: Arc<AtomicF32>,
    gain_window: AtomicRefCell<slint::Weak<GainWindow>>,
}

impl SlintEditor for GainEditorBuilder {
    type View = GainView;

    fn build(&self, context: Arc<dyn GuiContext>, _window: Arc<Window>) -> Self::View {
        let gain_window = GainWindow::new().unwrap();
        let param_wrapper = Arc::new(ParamWrapper {
            params: self.params.clone(),
            context,
        });

        let unmodulated_normalized_value = self.params.gain.unmodulated_normalized_value();
        gain_window.set_gain(unmodulated_normalized_value);
        gain_window.set_gain_mod_offset(
            self.params.gain.modulated_normalized_value() - unmodulated_normalized_value,
        );
        gain_window.set_default_gain(param_wrapper.params.gain.default_normalized_value());

        gain_window.on_gain_changed({
            let param_wrapper = param_wrapper.clone();
            move |gain| unsafe {
                param_wrapper
                    .context
                    .raw_set_parameter_normalized(param_wrapper.params.gain.as_ptr(), gain);
            }
        });
        gain_window.on_gain_step({
            let param_wrapper = param_wrapper.clone();
            move |next, finer| {
                let param_ptr = param_wrapper.params.gain.as_ptr();
                unsafe {
                    param_wrapper.context.raw_begin_set_parameter(param_ptr);
                }

                let value = if next {
                    param_wrapper.params.gain.next_normalized_step(
                        param_wrapper.params.gain.unmodulated_normalized_value(),
                        finer,
                    )
                } else {
                    param_wrapper.params.gain.previous_normalized_step(
                        param_wrapper.params.gain.unmodulated_normalized_value(),
                        finer,
                    )
                };

                unsafe {
                    param_wrapper
                        .context
                        .raw_set_parameter_normalized(param_ptr, value);
                    param_wrapper.context.raw_end_set_parameter(param_ptr);
                }
                value
            }
        });
        gain_window.on_gain_pressed({
            let param_wrapper = param_wrapper.clone();
            move || unsafe {
                param_wrapper
                    .context
                    .raw_begin_set_parameter(param_wrapper.params.gain.as_ptr());
            }
        });
        gain_window.on_gain_released({
            let param_wrapper = param_wrapper.clone();
            move || unsafe {
                param_wrapper
                    .context
                    .raw_end_set_parameter(param_wrapper.params.gain.as_ptr());
            }
        });

        gain_window.on_gain_text({
            let param_wrapper = param_wrapper.clone();
            move |gain| {
                param_wrapper
                    .params
                    .gain
                    .normalized_value_to_string(gain, true)
                    .into()
            }
        });
        gain_window.on_gain_accepted({
            let param_wrapper = param_wrapper.clone();
            move |gain_text| match param_wrapper
                .params
                .gain
                .string_to_normalized_value(gain_text.as_str())
            {
                Some(gain) => {
                    let gain_ptr = param_wrapper.params.gain.as_ptr();
                    unsafe {
                        param_wrapper.context.raw_begin_set_parameter(gain_ptr);
                        param_wrapper
                            .context
                            .raw_set_parameter_normalized(gain_ptr, gain);
                        param_wrapper.context.raw_end_set_parameter(gain_ptr);
                    }
                    gain
                }
                None => param_wrapper.params.gain.unmodulated_normalized_value(),
            }
        });

        gain_window.on_peakemter({
            let peak_meter = self.peak_meter.clone();

            let hold_time = Duration::from_millis(600);
            let mut hold_db = <f32 as Sample>::MINUS_INFINITY_DB;
            let mut last_hold_time = Instant::now();

            move || {
                let db = util::gain_to_db(peak_meter.load(Ordering::Relaxed));
                let now = Instant::now();
                if db > hold_db || now > last_hold_time + hold_time {
                    hold_db = db;
                    last_hold_time = now;
                }

                PeakmeterValues { db, hold_db }
            }
        });

        *self.gain_window.borrow_mut() = gain_window.as_weak();

        GainView { gain_window }
    }

    fn on_created(&self, handle: &Arc<EditorHandle>) {
        if let Some(adapter) = handle.window_adapter() {
            if let Err(err) = assets::register_noto_sans_regular(adapter) {
                nih_error!("Failed to register Noto Sans Regular font: {}", err);
            }
            if let Err(err) = assets::register_noto_sans_thin(adapter) {
                nih_error!("Failed to register Noto Sans Thin font: {}", err);
            }
            if let Err(err) = assets::register_noto_sans_light(adapter) {
                nih_error!("Failed to register Noto Sans Light font: {}", err);
            }
        }

        if let Some(ui) = self.gain_window.borrow().upgrade() {
            ui.set_scale_factor(handle.scale_factor() as _);
            ui.on_set_scale_factor({
                let handle_weak = Arc::downgrade(handle);
                move |factor| {
                    if let Some(handle) = handle_weak.upgrade() {
                        handle.set_scale_factor(factor as _);
                    }
                }
            });
        }
    }

    fn on_param_event(&self, event: ParamEvent) {
        let Some(gain_window) = self.gain_window.borrow().upgrade() else {
            return;
        };

        match event {
            ParamEvent::ValueChanged {
                id,
                normalized_value,
            } => {
                if id == "gain" {
                    gain_window.set_gain(normalized_value);
                }
            }
            ParamEvent::ModChanged {
                id,
                modulation_offset,
            } => {
                if id == "gain" {
                    gain_window.set_gain_mod_offset(modulation_offset);
                }
            }
            ParamEvent::ValuesChanged => {
                let unmodulated_normalized_value = self.params.gain.unmodulated_normalized_value();
                gain_window.set_gain(unmodulated_normalized_value);
                gain_window.set_gain_mod_offset(
                    self.params.gain.modulated_normalized_value() - unmodulated_normalized_value,
                );
            }
        }
    }
}

struct GainView {
    gain_window: GainWindow,
}

impl PluginView for GainView {
    fn window(&self) -> &slint::Window {
        self.gain_window.window()
    }

    fn on_event(&self, _event: &Event) -> EventResponse {
        EventResponse::Handled
    }
}

struct ParamWrapper {
    params: Arc<GainParams>,
    context: Arc<dyn GuiContext>,
}

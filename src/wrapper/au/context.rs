use std::collections::BTreeMap;
use std::sync::Arc;

use crate::prelude::{AuPlugin, GuiContext, InitContext, ParamPtr, PluginApi, PluginState};
use crate::wrapper::au::Wrapper;

// ---------- WrapperGuiContext ---------- //

pub(super) struct WrapperGuiContext<P: AuPlugin> {
    pub(super) wrapper: Arc<Wrapper<P>>,
}

impl<P: AuPlugin> GuiContext for WrapperGuiContext<P> {
    fn plugin_api(&self) -> PluginApi {
        PluginApi::Au
    }

    fn request_resize(&self) -> bool {
        self.wrapper.request_resize()
    }

    unsafe fn raw_begin_set_parameter(&self, _param: ParamPtr) {}

    unsafe fn raw_set_parameter_normalized(&self, _param: ParamPtr, _normalized: f32) {}

    unsafe fn raw_end_set_parameter(&self, _param: ParamPtr) {}

    fn get_state(&self) -> PluginState {
        PluginState {
            version: String::new(),
            params: BTreeMap::new(),
            fields: BTreeMap::new(),
        }
    }

    fn set_state(&self, _state: PluginState) {}
}

// ---------- WrapperInitContext ---------- //

pub(super) struct WrapperInitContext<'a, P: AuPlugin> {
    pub(super) wrapper: &'a Wrapper<P>,
}

impl<'a, P: AuPlugin> InitContext<P> for WrapperInitContext<'a, P> {
    fn plugin_api(&self) -> PluginApi {
        PluginApi::Au
    }

    fn execute(&self, task: P::BackgroundTask) {
        (self.wrapper.task_executor.lock())(task);
    }

    fn set_latency_samples(&self, samples: u32) {
        self.wrapper.set_latency_samples(samples);
    }

    fn set_current_voice_capacity(&self, _capacity: u32) {}
}

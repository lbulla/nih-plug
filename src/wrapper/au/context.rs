use std::sync::Arc;

use crate::event_loop::EventLoop;
use crate::prelude::{
    AuPlugin, GuiContext, InitContext, ParamPtr, PluginApi, PluginNoteEvent, PluginState,
    ProcessContext, Transport,
};
use crate::wrapper::au::wrapper::{EditorParamEvent, EventRef, Task};
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

    unsafe fn raw_begin_set_parameter(&self, param: ParamPtr) {
        match self.wrapper.param_ptr_to_hash(&param) {
            Some(hash) => {
                self.wrapper
                    .post_editor_param_event_gui(EditorParamEvent::BeginGesture {
                        param_hash: *hash,
                    });
            }
            _ => {
                nih_debug_assert_failure!("`raw_begin_set_parameter` called with an unknown param")
            }
        }
    }

    unsafe fn raw_set_parameter_normalized(&self, param: ParamPtr, normalized: f32) {
        match self.wrapper.param_ptr_to_hash(&param) {
            Some(hash) => {
                self.wrapper
                    .post_editor_param_event_gui(EditorParamEvent::SetValueFromEditor {
                        param_hash: *hash,
                        param,
                        normalized_value: normalized,
                    });
            }
            _ => nih_debug_assert_failure!(
                "`raw_set_parameter_normalized` called with an unknown param"
            ),
        }
    }

    unsafe fn raw_end_set_parameter(&self, param: ParamPtr) {
        match self.wrapper.param_ptr_to_hash(&param) {
            Some(hash) => {
                self.wrapper
                    .post_editor_param_event_gui(EditorParamEvent::EndGesture {
                        param_hash: *hash,
                    });
            }
            _ => nih_debug_assert_failure!("`raw_end_set_parameter` called with an unknown param"),
        }
    }

    fn get_state(&self) -> PluginState {
        self.wrapper.get_state_object()
    }

    fn set_state(&self, state: PluginState) {
        self.wrapper.set_state_object_from_gui(state);
    }
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

// ---------- WrapperProcessContext ---------- //

pub(super) struct WrapperProcessContext<'a, P: AuPlugin> {
    pub(super) wrapper: &'a Wrapper<P>,
    pub(super) transport: Transport,
    pub(super) input_events_guard: EventRef<'a, P>,
    pub(super) output_events_guard: EventRef<'a, P>,
}

impl<'a, P: AuPlugin> ProcessContext<P> for WrapperProcessContext<'a, P> {
    fn plugin_api(&self) -> PluginApi {
        PluginApi::Au
    }

    fn execute_background(&self, task: P::BackgroundTask) {
        let task_posted = self.wrapper.schedule_background(Task::PluginTask(task));
        nih_debug_assert!(task_posted, "The task queue is full, dropping task...");
    }

    fn execute_gui(&self, task: P::BackgroundTask) {
        let task_posted = self.wrapper.schedule_gui(Task::PluginTask(task));
        nih_debug_assert!(task_posted, "The task queue is full, dropping task...");
    }

    fn transport(&self) -> &Transport {
        &self.transport
    }

    fn next_event(&mut self) -> Option<PluginNoteEvent<P>> {
        self.input_events_guard.pop_front()
    }

    fn send_event(&mut self, event: PluginNoteEvent<P>) {
        self.output_events_guard.push_back(event);
    }

    fn set_latency_samples(&self, samples: u32) {
        self.wrapper.set_latency_samples(samples);
    }

    fn set_current_voice_capacity(&self, _capacity: u32) {}
}

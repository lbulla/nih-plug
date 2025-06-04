use std::sync::Weak;
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::{JsCast, JsValue};

use super::{BackgroundThread, EventLoop, MainThreadExecutor};

pub(crate) struct WebEventLoop<T: Send, E> {
    executor: Weak<E>,
    background_thread: BackgroundThread<T, E>,
}

impl<T, E> EventLoop<T, E> for WebEventLoop<T, E>
where
    T: Send + 'static,
    E: MainThreadExecutor<T> + 'static,
{
    fn new_and_spawn(executor: Weak<E>) -> Self {
        Self {
            executor: executor.clone(),
            background_thread: BackgroundThread::get_or_create(executor),
        }
    }

    fn schedule_gui(&self, task: T) -> bool {
        if self.is_main_thread() {
            match self.executor.upgrade() {
                Some(executor) => executor.execute(task, true),
                None => nih_debug_assert_failure!("GUI task posted after the executor was dropped"),
            }

            true
        } else {
            self.background_thread.schedule(task)
        }
    }

    fn schedule_background(&self, task: T) -> bool {
        self.background_thread.schedule(task)
    }

    // Taken from: https://github.com/rust-windowing/winit/blob/59e3dda89fe578e1b28efa25181418b35f11a69d/src/platform_impl/web/main_thread.rs#L30.
    fn is_main_thread(&self) -> bool {
        #[wasm_bindgen]
        extern "C" {
            #[derive(Clone)]
            type Global;

            #[wasm_bindgen(method, getter, js_name = Window)]
            fn window(this: &Global) -> JsValue;
        }

        let global: Global = js_sys::global().unchecked_into();
        !global.window().is_undefined()
    }
}

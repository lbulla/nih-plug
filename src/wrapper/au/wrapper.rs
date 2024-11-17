use atomic_refcell::AtomicRefCell;
use crossbeam::queue::ArrayQueue;
use dispatch::Queue;
use objc2::rc::Retained;
use objc2_app_kit::NSView;
use objc2_foundation::NSThread;
use parking_lot::Mutex;
use std::any::Any;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, Weak};

use crate::event_loop::{BackgroundThread, EventLoop, MainThreadExecutor, TASK_QUEUE_CAPACITY};
use crate::prelude::{AsyncExecutor, AuPlugin, Editor, ParentWindowHandle, TaskExecutor};
use crate::wrapper::au::context::WrapperGuiContext;
use crate::wrapper::au::editor::WrapperViewHolder;
use crate::wrapper::au::properties::{PropertyDispatcher, PropertyDispatcherImpl};
use crate::wrapper::au::util::ThreadWrapper;
use crate::wrapper::au::{au_sys, AuPropertyListenerProc, NO_ERROR};

// ---------- Types ---------- //

struct PropertyListener {
    proc: AuPropertyListenerProc,
    data: ThreadWrapper<*mut c_void>,
}

enum Task<P: AuPlugin> {
    PluginTask(P::BackgroundTask),

    // NOTE: A `NSView` must be resized on the main thread.
    RequestResize,
}

// ---------- Wrapper ---------- //

pub(super) struct Wrapper<P: AuPlugin> {
    unit: ThreadWrapper<au_sys::AudioUnit>,

    this: AtomicRefCell<Weak<Wrapper<P>>>,
    plugin: Mutex<P>,
    property_listeners: AtomicRefCell<HashMap<au_sys::AudioUnitPropertyID, Vec<PropertyListener>>>,

    task_executor: Mutex<TaskExecutor<P>>,
    tasks: ArrayQueue<Task<P>>,
    background_thread: AtomicRefCell<Option<BackgroundThread<Task<P>, Self>>>,

    editor: AtomicRefCell<Option<Mutex<Box<dyn Editor>>>>,
    wrapper_view_holder: AtomicRefCell<WrapperViewHolder>,
}

impl<P: AuPlugin> Wrapper<P> {
    pub(super) fn new(unit: au_sys::AudioUnit) -> Arc<Self> {
        let mut plugin = P::default();
        let task_executor = plugin.task_executor();

        let wrapper = Arc::new(Self {
            unit: ThreadWrapper::new(unit),

            this: AtomicRefCell::new(Weak::new()),
            plugin: Mutex::new(plugin),
            property_listeners: AtomicRefCell::new(HashMap::new()),

            task_executor: Mutex::new(task_executor),
            tasks: ArrayQueue::new(TASK_QUEUE_CAPACITY),
            background_thread: AtomicRefCell::new(None),

            editor: AtomicRefCell::new(None),
            wrapper_view_holder: AtomicRefCell::new(WrapperViewHolder::default()),
        });

        *wrapper.this.borrow_mut() = Arc::downgrade(&wrapper);

        *wrapper.background_thread.borrow_mut() =
            Some(BackgroundThread::get_or_create(Arc::downgrade(&wrapper)));

        *wrapper.editor.borrow_mut() = wrapper
            .plugin
            .lock()
            .editor(AsyncExecutor {
                execute_background: Arc::new({
                    let wrapper = wrapper.clone();

                    move |task| {
                        let task_posted = wrapper.schedule_background(Task::PluginTask(task));
                        nih_debug_assert!(task_posted, "The task queue is full, dropping task...");
                    }
                }),
                execute_gui: Arc::new({
                    let wrapper = wrapper.clone();

                    move |task| {
                        let task_posted = wrapper.schedule_gui(Task::PluginTask(task));
                        nih_debug_assert!(task_posted, "The task queue is full, dropping task...");
                    }
                }),
            })
            .map(Mutex::new);

        wrapper
    }

    // ---------- Getter ---------- //

    pub(super) fn unit(&self) -> au_sys::AudioUnit {
        self.unit.get()
    }

    pub(super) fn as_arc(&self) -> Arc<Self> {
        self.this.borrow().upgrade().unwrap()
    }

    // ---------- Contexts ---------- //

    fn make_gui_context(&self) -> Arc<WrapperGuiContext<P>> {
        Arc::new(WrapperGuiContext {
            wrapper: self.as_arc(),
        })
    }

    // ---------- Editor ---------- //

    pub(super) fn has_editor(&self) -> bool {
        self.editor.borrow().is_some()
    }

    #[must_use]
    pub(super) fn spawn_editor(&self, view: &Retained<NSView>) -> Box<dyn Any + Send> {
        let editor = self.editor.borrow();
        let editor = editor
            .as_ref()
            .expect("`spawn_editor` called without an editor")
            .lock();

        let parent_handle = ParentWindowHandle::AppKitNsView(Retained::as_ptr(view) as _);
        let editor_handle = editor.spawn(parent_handle, self.make_gui_context());
        self.wrapper_view_holder
            .borrow_mut()
            .init(view, editor.size());
        editor_handle
    }

    pub(super) fn request_resize(&self) -> bool {
        if self.wrapper_view_holder.borrow().has_view() {
            let task_posted = self.schedule_gui(Task::RequestResize);
            nih_debug_assert!(task_posted, "The task queue is full, dropping task...");
            true
        } else {
            false
        }
    }

    // ---------- Setup ---------- //

    pub(super) fn init(&self) -> au_sys::OSStatus {
        NO_ERROR
    }

    pub(super) fn uninit(&self) -> au_sys::OSStatus {
        NO_ERROR
    }

    // ---------- Properties ---------- //

    pub(super) fn get_property_info(
        &self,
        in_id: au_sys::AudioUnitPropertyID,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        out_data_size: *mut au_sys::UInt32,
        out_writable: *mut au_sys::Boolean,
    ) -> au_sys::OSStatus {
        PropertyDispatcher::info(
            in_id,
            self,
            in_scope,
            in_element,
            out_data_size,
            out_writable,
        )
    }

    pub(super) fn get_property(
        &self,
        in_id: au_sys::AudioUnitPropertyID,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        out_data: *mut c_void,
        io_data_size: *mut au_sys::UInt32,
    ) -> au_sys::OSStatus {
        PropertyDispatcher::get(in_id, self, in_scope, in_element, out_data, io_data_size)
    }

    pub(super) fn set_property(
        &mut self,
        in_id: au_sys::AudioUnitPropertyID,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
        in_data: *const c_void,
        in_data_size: au_sys::UInt32,
    ) -> au_sys::OSStatus {
        PropertyDispatcher::set(in_id, self, in_scope, in_element, in_data, in_data_size)
    }

    pub(super) fn add_property_listener(
        &self,
        in_id: au_sys::AudioUnitPropertyID,
        in_proc: AuPropertyListenerProc,
        in_proc_data: *mut c_void,
    ) -> au_sys::OSStatus {
        self.property_listeners
            .borrow_mut()
            .entry(in_id)
            .or_default()
            .push(PropertyListener {
                proc: in_proc,
                data: ThreadWrapper::new(in_proc_data),
            });
        NO_ERROR
    }

    pub(super) fn remove_property_listener(
        &self,
        in_id: au_sys::AudioUnitPropertyID,
        in_proc: AuPropertyListenerProc,
        in_proc_data: *mut c_void,
    ) -> au_sys::OSStatus {
        if let Some(listeners) = self.property_listeners.borrow_mut().get_mut(&in_id) {
            listeners
                .retain(|listener| listener.proc != in_proc && listener.data.get() != in_proc_data);
        }
        NO_ERROR
    }

    pub(super) fn call_property_listeners(
        &self,
        in_id: au_sys::AudioUnitPropertyID,
        in_scope: au_sys::AudioUnitScope,
        in_element: au_sys::AudioUnitElement,
    ) {
        if let Some(listeners) = self.property_listeners.borrow().get(&in_id) {
            for listener in listeners {
                unsafe {
                    (listener.proc)(
                        listener.data.get(),
                        self.unit(),
                        in_id,
                        in_scope,
                        in_element,
                    );
                }
            }
        }
    }
}

// ---------- Events / Tasks ---------- //

impl<P: AuPlugin> EventLoop<Task<P>, Wrapper<P>> for Wrapper<P> {
    fn new_and_spawn(_executor: Weak<Self>) -> Self {
        panic!("What are you doing");
    }

    fn schedule_gui(&self, task: Task<P>) -> bool {
        if self.is_main_thread() {
            self.execute(task, true);
            true
        } else {
            let success = self.tasks.push(task).is_ok();
            if success {
                Queue::main().exec_async({
                    let wrapper = self.as_arc();
                    move || {
                        while let Some(task) = wrapper.tasks.pop() {
                            wrapper.execute(task, true);
                        }
                    }
                });
            }
            success
        }
    }

    fn schedule_background(&self, task: Task<P>) -> bool {
        self.background_thread
            .borrow()
            .as_ref()
            .unwrap()
            .schedule(task)
    }

    fn is_main_thread(&self) -> bool {
        NSThread::isMainThread_class()
    }
}

impl<P: AuPlugin> MainThreadExecutor<Task<P>> for Wrapper<P> {
    fn execute(&self, task: Task<P>, is_gui_thread: bool) {
        match task {
            Task::PluginTask(task) => (self.task_executor.lock())(task),
            Task::RequestResize => {
                nih_debug_assert!(
                    is_gui_thread,
                    "A `NSView` must be resized on the main thread"
                );
                self.wrapper_view_holder
                    .borrow()
                    .resize(self.editor.borrow().as_ref().unwrap().lock().size());
            }
        }
    }
}

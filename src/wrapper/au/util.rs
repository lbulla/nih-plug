// ---------- ThreadWrapper ---------- //

// NOTE: Make types like pointers Send and Sync. Must obviously be used with care.
pub(super) struct ThreadWrapper<T: Clone>(T);

impl<T: Clone> ThreadWrapper<T> {
    pub(super) fn new(value: T) -> Self {
        Self(value)
    }

    pub(super) fn get(&self) -> T {
        self.0.clone()
    }

    pub(super) fn as_ref(&self) -> &T {
        &self.0
    }

    pub(super) fn as_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

unsafe impl<T: Clone> Send for ThreadWrapper<T> {}
unsafe impl<T: Clone> Sync for ThreadWrapper<T> {}

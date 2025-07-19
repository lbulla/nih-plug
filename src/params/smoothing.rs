//! Utilities to handle smoothing parameter changes over time.

use atomic_refcell::AtomicRefCell;
use std::fmt::{Debug, Error, Formatter};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use crate::sample::{Sample, Smoothable};

/// Controls if and how parameters get smoothed.
#[derive(Debug)]
pub enum SmoothingStyle<S: Sample> {
    /// Wraps another smoothing style to create a multi-rate oversampling-aware smoother for a
    /// parameter that's used in an oversampled part of the plugin. The `Arc<AtomicF32>` indicates
    /// the oversampling amount, where `1.0` means no oversampling. This value can change at
    /// runtime, and it effectively scales the sample rate when computing new smoothing coefficients
    /// when the parameter's value changes.
    OversamplingAware(Arc<S::Atomic>, &'static SmoothingStyle<S>),

    /// No smoothing is applied. The parameter's `value` field contains the latest sample value
    /// available for the parameters.
    None,
    /// Smooth parameter changes so the current value approaches the target value at a constant
    /// rate. The target value will be reached in exactly this many milliseconds.
    Linear(S),
    /// Smooth parameter changes such that the rate matches the curve of a logarithmic function,
    /// starting out slow and then constantly increasing the slope until the value is reached. The
    /// target value will be reached in exactly this many milliseconds. This is useful for smoothing
    /// things like frequencies and decibel gain value. **The caveat is that the value may never
    /// reach 0**, or you will end up multiplying and dividing things by zero. Make sure your value
    /// ranges don't include 0.
    Logarithmic(S),
    /// Smooth parameter changes such that the rate matches the curve of an exponential function,
    /// starting out fast and then tapering off until the end. This is a single-pole IIR filter
    /// under the hood, while the other smoothing options are FIR filters. This means that the exact
    /// value would never be reached. Instead, this reaches 99.99% of the value target value in the
    /// specified number of milliseconds, and it then snaps to the target value in the last step.
    /// This results in a smoother transition, with the caveat being that there will be a tiny jump
    /// at the end. Unlike the `Logarithmic` option, this does support crossing the zero value.
    Exponential(S),
}

// FIXME: Used for the AU wrapper. See: `ScheduleParamRamp`.
pub struct AtomicSmoothingStyle<S: Sample>(AtomicRefCell<SmoothingStyle<S>>);

impl<S: Sample> AtomicSmoothingStyle<S> {
    pub fn new(style: SmoothingStyle<S>) -> Self {
        Self(AtomicRefCell::new(style))
    }

    pub fn clone(&self) -> Self {
        Self(self.0.clone())
    }

    pub fn as_ref(&self) -> &AtomicRefCell<SmoothingStyle<S>> {
        &self.0
    }
}

impl<S: Sample> Debug for AtomicSmoothingStyle<S> {
    fn fmt(&self, f: &mut Formatter) -> Result<(), Error> {
        self.0.borrow().fmt(f)
    }
}

/// A smoother, providing a smoothed value for each sample.
//
// TODO: We need to use atomics here so we can share the params object with the GUI. Is there a
//       better alternative to allow the process function to mutate these smoothers?
#[derive(Debug)]
pub struct Smoother<T: Smoothable, S: Sample> {
    /// The kind of smoothing that needs to be applied, if any.
    pub style: AtomicSmoothingStyle<S>,
    /// The number of steps of smoothing left to take.
    ///
    // This is a signed integer because we can skip multiple steps, which would otherwise make it
    // possible to get an underflow here.
    steps_left: AtomicI32,
    /// The amount we should adjust the current value each sample to be able to reach the target in
    /// the specified tiem frame. This is also a floating point number to keep the smoothing
    /// uniform.
    ///
    /// In the case of the `Exponential` smoothing style this is the coefficient `x` that the
    /// previous sample is multiplied by.
    step_size: S::Atomic,
    /// The value for the current sample. Always stored as floating point for obvious reasons.
    current: S::Atomic,
    /// The value we're smoothing towards
    target: T::Atomic,
}

/// An iterator that continuously produces smoothed values. Can be used as an alternative to the
/// block-based smoothing API. Since the iterator itself is infinite, you can use
/// [`Smoother::is_smoothing()`] and [`Smoother::steps_left()`] to get information on the current
/// smoothing status.
pub struct SmootherIter<'a, T: Smoothable, S: Sample> {
    smoother: &'a Smoother<T, S>,
}

impl<S: Sample> SmoothingStyle<S> {
    /// Compute the number of steps to reach the target value based on the sample rate and this
    /// smoothing style's duration.
    #[inline]
    pub fn num_steps(&self, sample_rate: S) -> u32 {
        nih_debug_assert!(sample_rate > S::ZERO);

        match self {
            Self::OversamplingAware(oversampling_times, style) => {
                style.num_steps(sample_rate * S::atomic_load(oversampling_times, Ordering::Relaxed))
            }

            Self::None => 1,
            Self::Linear(time) | Self::Logarithmic(time) | Self::Exponential(time) => {
                nih_debug_assert!(*time >= S::ZERO);
                (sample_rate * *time * S::from_p(0.001)).round().to_p()
            }
        }
    }

    /// Compute the step size for this smoother. `num_steps` can be obtained using
    /// [`SmoothingStyle::num_steps()`]. Check the source code of the [`SmoothingStyle::next()`] and
    /// [`SmoothingStyle::next_step()`] functions for details on how these values should be used.
    #[inline]
    pub fn step_size(&self, start: S, target: S, num_steps: u32) -> S {
        nih_debug_assert!(num_steps >= 1);

        match self {
            Self::OversamplingAware(_, style) => style.step_size(start, target, num_steps),

            Self::None => S::ZERO,
            Self::Linear(_) => (target - start) / S::from_p(num_steps),
            Self::Logarithmic(_) => {
                // We need to solve `start * (step_size ^ num_steps) = target` for `step_size`
                nih_debug_assert_ne!(start, S::ZERO);
                S::from_p(f64::powf(
                    (target / start).to_p(),
                    (num_steps as f64).recip(),
                ))
            }
            // In this case the step size value is the coefficient the current value will be
            // multiplied by, while the target value is multiplied by one minus the coefficient. This
            // reaches 99.99% of the target value after `num_steps`. The smoother will snap to the
            // target value after that point.
            Self::Exponential(_) => S::from_p(f64::powf(0.0001, (num_steps as f64).recip())),
        }
    }

    /// Compute the next value from `current` leading up to `target` using the `step_size` computed
    /// using [`SmoothingStyle::step_size()`]. Depending on the smoothing style this function may
    /// never completely reach `target`, so you will need to snap to `target` yourself after
    /// computing the target number of steps.
    ///
    /// See the docstring on the [`SmoothingStyle::next_step()`] function for the formulas used.
    #[inline]
    pub fn next(&self, current: S, target: S, step_size: S) -> S {
        match self {
            Self::OversamplingAware(_, style) => style.next(current, target, step_size),

            Self::None => target,
            Self::Linear(_) => current + step_size,
            Self::Logarithmic(_) => current * step_size,
            Self::Exponential(_) => (current * step_size) + (target * (S::ONE - step_size)),
        }
    }

    /// The same as [`next()`][Self::next()], but with the option to take more than one step at a
    /// time. Calling `next_step()` with step count `n` gives the same result as applying `next()`
    /// `n` times to a value, but is more efficient to compute. `next_step()` with 1 step is
    /// equivalent to `step()`.
    ///
    /// See the docstring on the [`SmoothingStyle::next_step()`] function for the formulas used.
    #[inline]
    pub fn next_step(&self, current: S, target: S, step_size: S, steps: u32) -> S {
        nih_debug_assert!(steps >= 1);

        match self {
            Self::OversamplingAware(_, style) => style.next_step(current, target, step_size, steps),

            Self::None => target,
            Self::Linear(_) => current + (step_size * S::from_p(steps)),
            Self::Logarithmic(_) => current * (step_size.powi(steps as i32)),
            Self::Exponential(_) => {
                // This is the same as calculating `current = (current * step_size) +
                // (target * (1 - step_size))` in a loop since the target value won't change
                let coefficient = step_size.powi(steps as i32);
                (current * coefficient) + (target * (S::ONE - coefficient))
            }
        }
    }
}

impl<T: Smoothable, S: Sample> Default for Smoother<T, S> {
    fn default() -> Self {
        Self {
            style: AtomicSmoothingStyle::new(SmoothingStyle::None),
            steps_left: AtomicI32::new(0),
            step_size: Default::default(),
            current: S::atomic_new(S::ZERO),
            target: Default::default(),
        }
    }
}

impl<T: Smoothable, S: Sample> Iterator for SmootherIter<'_, T, S> {
    type Item = T;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        Some(self.smoother.next())
    }
}

impl<S: Sample> Clone for SmoothingStyle<S> {
    fn clone(&self) -> Self {
        match self {
            Self::OversamplingAware(oversampling_times, style) => {
                Self::OversamplingAware(oversampling_times.clone(), *style)
            }

            Self::None => Self::None,
            Self::Linear(time) => Self::Linear(*time),
            Self::Logarithmic(time) => Self::Logarithmic(*time),
            Self::Exponential(time) => Self::Exponential(*time),
        }
    }
}

impl<T: Smoothable, S: Sample> Clone for Smoother<T, S> {
    fn clone(&self) -> Self {
        // We can't derive clone because of the atomics, but these atomics are only here to allow
        // Send+Sync interior mutability
        Self {
            style: self.style.clone(),
            steps_left: AtomicI32::new(self.steps_left.load(Ordering::Relaxed)),
            step_size: S::atomic_new(S::atomic_load(&self.step_size, Ordering::Relaxed)),
            current: S::atomic_new(S::atomic_load(&self.current, Ordering::Relaxed)),
            target: T::atomic_new(T::atomic_load(&self.target, Ordering::Relaxed)),
        }
    }
}

impl<T: Smoothable, S: Sample> Smoother<T, S> {
    /// Use the specified style for the smoothing.
    pub fn new(style: SmoothingStyle<S>) -> Self {
        Self {
            style: AtomicSmoothingStyle::new(style),
            ..Default::default()
        }
    }

    /// Convenience function for not applying any smoothing at all. Same as `Smoother::default`.
    pub fn none() -> Self {
        Default::default()
    }

    /// The number of steps left until calling [`next()`][Self::next()] will stop yielding new
    /// values.
    #[inline]
    pub fn steps_left(&self) -> i32 {
        self.steps_left.load(Ordering::Relaxed)
    }

    /// Whether calling [`next()`][Self::next()] will yield a new value or an old value. Useful if
    /// you need to recompute something whenever this parameter changes.
    #[inline]
    pub fn is_smoothing(&self) -> bool {
        self.steps_left() > 0
    }

    /// Produce an iterator that yields smoothed values. These are not iterators already for the
    /// sole reason that this will always yield a value, and needing to unwrap all of those options
    /// is not going to be very fun.
    #[inline]
    pub fn iter(&self) -> SmootherIter<'_, T, S> {
        SmootherIter { smoother: self }
    }

    /// Reset the smoother the specified value.
    pub fn reset(&self, value: T) {
        T::atomic_store(&self.target, value, Ordering::Relaxed);
        S::atomic_store(&self.current, value.to_s(), Ordering::Relaxed);
        self.steps_left.store(0, Ordering::Relaxed);
    }

    /// Set the target value.
    pub fn set_target(&self, sample_rate: S, target: T) {
        T::atomic_store(&self.target, target, Ordering::Relaxed);

        let steps_left = self.style.0.borrow().num_steps(sample_rate) as i32;
        self.steps_left.store(steps_left, Ordering::Relaxed);

        let current = S::atomic_load(&self.current, Ordering::Relaxed);
        let target_s = target.to_s();
        S::atomic_store(
            &self.step_size,
            if steps_left > 0 {
                self.style
                    .0
                    .borrow()
                    .step_size(current, target_s, steps_left as u32)
            } else {
                S::ZERO
            },
            Ordering::Relaxed,
        );
    }

    /// Get the next value from this smoother. The value will be equal to the previous value once
    /// the smoothing period is over. This should be called exactly once per sample.
    // Yes, Clippy, like I said, this was intentional
    #[allow(clippy::should_implement_trait)]
    #[inline]
    pub fn next(&self) -> T {
        let target = T::atomic_load(&self.target, Ordering::Relaxed);

        // NOTE: Shis used to be implemented in terms of `next_step()`, but this is more efficient
        //       for the common use case of single steps
        if self.steps_left.load(Ordering::Relaxed) > 0 {
            let current = S::atomic_load(&self.current, Ordering::Relaxed);
            let target_s = target.to_s();
            let step_size = S::atomic_load(&self.step_size, Ordering::Relaxed);

            // The number of steps usually won't fit exactly, so make sure we don't end up with
            // quantization errors on overshoots or undershoots. We also need to account for the
            // possibility that we only have `n < steps` steps left. This is especially important
            // for the `Exponential` smoothing style, since that won't reach the target value
            // exactly.
            let old_steps_left = self.steps_left.fetch_sub(1, Ordering::Relaxed);
            let new = if old_steps_left == 1 {
                self.steps_left.store(0, Ordering::Relaxed);
                target_s
            } else {
                self.style.0.borrow().next(current, target_s, step_size)
            };
            S::atomic_store(&self.current, new, Ordering::Relaxed);

            T::from_s(new)
        } else {
            target
        }
    }

    /// [`next()`][Self::next()], but with the ability to skip forward in the smoother.
    /// [`next()`][Self::next()] is equivalent to calling this function with a `steps` value of 1.
    /// Calling this function with a `steps` value of `n` means will cause you to skip the next `n -
    /// 1` values and return the `n`th value.
    #[inline]
    pub fn next_step(&self, steps: u32) -> T {
        nih_debug_assert_ne!(steps, 0);

        let target = T::atomic_load(&self.target, Ordering::Relaxed);

        if self.steps_left.load(Ordering::Relaxed) > 0 {
            let current = S::atomic_load(&self.current, Ordering::Relaxed);
            let target_s = target.to_s();
            let step_size = S::atomic_load(&self.step_size, Ordering::Relaxed);

            // The number of steps usually won't fit exactly, so make sure we don't end up with
            // quantization errors on overshoots or undershoots. We also need to account for the
            // possibility that we only have `n < steps` steps left. This is especially important
            // for the `Exponential` smoothing style, since that won't reach the target value
            // exactly.
            let old_steps_left = self.steps_left.fetch_sub(steps as i32, Ordering::Relaxed);
            let new = if old_steps_left <= steps as i32 {
                self.steps_left.store(0, Ordering::Relaxed);
                target_s
            } else {
                self.style
                    .0
                    .borrow()
                    .next_step(current, target_s, step_size, steps)
            };
            S::atomic_store(&self.current, new, Ordering::Relaxed);

            T::from_s(new)
        } else {
            target
        }
    }

    /// Get previous value returned by this smoother. This may be useful to save some boilerplate
    /// when [`is_smoothing()`][Self::is_smoothing()] is used to determine whether an expensive
    /// calculation should take place, and [`next()`][Self::next()] gets called as part of that
    /// calculation.
    pub fn previous_value(&self) -> T {
        T::from_s(S::atomic_load(&self.current, Ordering::Relaxed))
    }

    /// Produce smoothed values for an entire block of audio. This is useful when iterating the same
    /// block of audio multiple times. For instance when summing voices for a synthesizer.
    /// `block_values[..block_len]` will be filled with the smoothed values. This is simply a
    /// convenient function for [`next_block_exact()`][Self::next_block_exact()] when iterating over
    /// variable length blocks with a known maximum size.
    ///
    /// # Panics
    ///
    /// Panics if `block_len > block_values.len()`.
    pub fn next_block(&self, block_values: &mut [T], block_len: usize) {
        self.next_block_exact(&mut block_values[..block_len])
    }

    /// The same as [`next_block()`][Self::next_block()], but filling the entire slice.
    pub fn next_block_exact(&self, block_values: &mut [T]) {
        let target = T::atomic_load(&self.target, Ordering::Relaxed);

        // `self.next()` will yield the current value if the parameter is no longer smoothing, but
        // it's a bit of a waste to continuously call that if only the first couple or none of the
        // values in `block_values` would require smoothing and the rest don't. Instead, we'll just
        // smooth the values as necessary, and then reuse the target value for the rest of the
        // block.
        let steps_left = self.steps_left.load(Ordering::Relaxed) as usize;
        let num_smoothed_values = block_values.len().min(steps_left);
        if num_smoothed_values > 0 {
            let mut current = S::atomic_load(&self.current, Ordering::Relaxed);
            let target_s = target.to_s();
            let step_size = S::atomic_load(&self.step_size, Ordering::Relaxed);

            if num_smoothed_values == steps_left {
                // This is the same as calling `next()` `num_smoothed_values` times, but with some
                // conditionals optimized out
                block_values[..num_smoothed_values - 1].fill_with(|| {
                    current = self.style.0.borrow().next(current, target_s, step_size);
                    T::from_s(current)
                });

                // In `next()` the last step snaps the value to the target value, so we'll do the
                // same thing here
                current = target_s;
                block_values[num_smoothed_values - 1] = target;
            } else {
                block_values[..num_smoothed_values].fill_with(|| {
                    current = self.style.0.borrow().next(current, target_s, step_size);
                    T::from_s(current)
                });
            }

            block_values[num_smoothed_values..].fill(target);

            S::atomic_store(&self.current, current, Ordering::Relaxed);
            self.steps_left
                .fetch_sub(num_smoothed_values as i32, Ordering::Relaxed);
        } else {
            block_values.fill(target);
        }
    }

    /// The same as [`next_block()`][Self::next_block()], but with a function applied to each
    /// produced value. The mapping function takes an index in the block and a floating point
    /// representation of the smoother's current value. This allows the modulation to be consistent
    /// during smoothing. Additionally, the mapping function is always called even if the smoothing
    /// is finished.
    pub fn next_block_mapped(
        &self,
        block_values: &mut [T],
        block_len: usize,
        f: impl FnMut(usize, S) -> T,
    ) {
        self.next_block_exact_mapped(&mut block_values[..block_len], f)
    }

    /// The same as [`next_block_exact()`][Self::next_block()], but with a function applied to each
    /// produced value. Useful when applying modulation to a smoothed parameter.
    pub fn next_block_exact_mapped(
        &self,
        block_values: &mut [T],
        mut f: impl FnMut(usize, S) -> T,
    ) {
        // This works exactly the same as `next_block_exact()`, except for the addition of the
        // mapping function
        let target_s = T::atomic_load(&self.target, Ordering::Relaxed).to_s();

        let steps_left = self.steps_left.load(Ordering::Relaxed) as usize;
        let num_smoothed_values = block_values.len().min(steps_left);
        if num_smoothed_values > 0 {
            let mut current = S::atomic_load(&self.current, Ordering::Relaxed);
            let step_size = S::atomic_load(&self.step_size, Ordering::Relaxed);

            // See `next_block_exact()` for more details
            if num_smoothed_values == steps_left {
                for (idx, value) in block_values
                    .iter_mut()
                    .enumerate()
                    .take(num_smoothed_values - 1)
                {
                    current = self.style.0.borrow().next(current, target_s, step_size);
                    *value = f(idx, current);
                }

                current = target_s;
                block_values[num_smoothed_values - 1] = f(num_smoothed_values - 1, target_s);
            } else {
                for (idx, value) in block_values
                    .iter_mut()
                    .enumerate()
                    .take(num_smoothed_values)
                {
                    current = self.style.0.borrow().next(current, target_s, step_size);
                    *value = f(idx, current);
                }
            }

            for (idx, value) in block_values
                .iter_mut()
                .enumerate()
                .skip(num_smoothed_values)
            {
                *value = f(idx, target_s);
            }

            S::atomic_store(&self.current, current, Ordering::Relaxed);
            self.steps_left
                .fetch_sub(num_smoothed_values as i32, Ordering::Relaxed);
        } else {
            for (idx, value) in block_values.iter_mut().enumerate() {
                *value = f(idx, target_s);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Applying `next()` `n` times should be the same as `next_step()` for `n` steps.
    #[test]
    fn linear_f32_next_equivalence() {
        let style = SmoothingStyle::Linear(100.0);

        let mut current = 0.4;
        let target = 0.8;
        let steps = 15;
        let step_size = style.step_size(current, target, steps);

        let expected_result = style.next_step(current, target, step_size, steps);
        for _ in 0..steps {
            current = style.next(current, target, step_size);
        }

        approx::assert_relative_eq!(current, expected_result, epsilon = 1e-5);
    }

    #[test]
    fn logarithmic_f32_next_equivalence() {
        let style = SmoothingStyle::Logarithmic(100.0);

        let mut current = 0.4;
        let target = 0.8;
        let steps = 15;
        let step_size = style.step_size(current, target, steps);

        let expected_result = style.next_step(current, target, step_size, steps);
        for _ in 0..steps {
            current = style.next(current, target, step_size);
        }

        approx::assert_relative_eq!(current, expected_result, epsilon = 1e-5);
    }

    #[test]
    fn exponential_f32_next_equivalence() {
        let style = SmoothingStyle::Exponential(100.0);

        let mut current = 0.4;
        let target = 0.8;
        let steps = 15;
        let step_size = style.step_size(current, target, steps);

        let expected_result = style.next_step(current, target, step_size, steps);
        for _ in 0..steps {
            current = style.next(current, target, step_size);
        }

        approx::assert_relative_eq!(current, expected_result, epsilon = 1e-5);
    }

    #[test]
    fn linear_f32_smoothing() {
        let smoother: Smoother<f32, f32> = Smoother::new(SmoothingStyle::Linear(100.0));
        smoother.reset(10.0);
        assert_eq!(smoother.next(), 10.0);

        // Instead of testing the actual values, we'll make sure that we reach the target values at
        // the expected time.
        smoother.set_target(100.0, 20.0);
        for _ in 0..(10 - 2) {
            smoother.next();
        }
        assert_ne!(smoother.next(), 20.0);
        assert_eq!(smoother.next(), 20.0);
    }

    #[test]
    fn linear_i32_smoothing() {
        let smoother: Smoother<i32> = Smoother::new(SmoothingStyle::Linear(100.0));
        smoother.reset(10);
        assert_eq!(smoother.next(), 10);

        // Integers are rounded, but with these values we can still test this
        smoother.set_target(100.0, 20);
        for _ in 0..(10 - 2) {
            smoother.next();
        }
        assert_ne!(smoother.next(), 20);
        assert_eq!(smoother.next(), 20);
    }

    #[test]
    fn logarithmic_f32_smoothing() {
        let smoother: Smoother<f32, f32> = Smoother::new(SmoothingStyle::Logarithmic(100.0));
        smoother.reset(10.0);
        assert_eq!(smoother.next(), 10.0);

        // Instead of testing the actual values, we'll make sure that we reach the target values at
        // the expected time.
        smoother.set_target(100.0, 20.0);
        for _ in 0..(10 - 2) {
            smoother.next();
        }
        assert_ne!(smoother.next(), 20.0);
        assert_eq!(smoother.next(), 20.0);
    }

    #[test]
    fn logarithmic_i32_smoothing() {
        let smoother: Smoother<i32> = Smoother::new(SmoothingStyle::Logarithmic(100.0));
        smoother.reset(10);
        assert_eq!(smoother.next(), 10);

        // Integers are rounded, but with these values we can still test this
        smoother.set_target(100.0, 20);
        for _ in 0..(10 - 2) {
            smoother.next();
        }
        assert_ne!(smoother.next(), 20);
        assert_eq!(smoother.next(), 20);
    }

    /// Same as [`linear_f32_smoothing`], but skipping steps instead.
    #[test]
    fn skipping_linear_f32_smoothing() {
        let smoother: Smoother<f32, f32> = Smoother::new(SmoothingStyle::Linear(100.0));
        smoother.reset(10.0);
        assert_eq!(smoother.next(), 10.0);

        smoother.set_target(100.0, 20.0);
        smoother.next_step(8);
        assert_ne!(smoother.next(), 20.0);
        assert_eq!(smoother.next(), 20.0);
    }

    /// Same as [`linear_i32_smoothing`], but skipping steps instead.
    #[test]
    fn skipping_linear_i32_smoothing() {
        let smoother: Smoother<i32> = Smoother::new(SmoothingStyle::Linear(100.0));
        smoother.reset(10);
        assert_eq!(smoother.next(), 10);

        smoother.set_target(100.0, 20);
        smoother.next_step(8);
        assert_ne!(smoother.next(), 20);
        assert_eq!(smoother.next(), 20);
    }

    /// Same as [`logarithmic_f32_smoothing`], but skipping steps instead.
    #[test]
    fn skipping_logarithmic_f32_smoothing() {
        let smoother: Smoother<f32, f32> = Smoother::new(SmoothingStyle::Logarithmic(100.0));
        smoother.reset(10.0);
        assert_eq!(smoother.next(), 10.0);

        smoother.set_target(100.0, 20.0);
        smoother.next_step(8);
        assert_ne!(smoother.next(), 20.0);
        assert_eq!(smoother.next(), 20.0);
    }

    /// Same as [`logarithmic_i32_smoothing`], but skipping steps instead.
    #[test]
    fn skipping_logarithmic_i32_smoothing() {
        let smoother: Smoother<i32> = Smoother::new(SmoothingStyle::Logarithmic(100.0));
        smoother.reset(10);
        assert_eq!(smoother.next(), 10);

        smoother.set_target(100.0, 20);
        smoother.next_step(8);
        assert_ne!(smoother.next(), 20);
        assert_eq!(smoother.next(), 20);
    }

    // TODO: Tests for the exponential smoothing
}

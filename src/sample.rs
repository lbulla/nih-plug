use std::fmt::{Debug, Display};
use std::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Rem, Sub, SubAssign};
use std::str::FromStr;
use std::sync::atomic::{AtomicI32, Ordering};

pub use atomic_float::{AtomicF32, AtomicF64};

pub trait Sample:
    FromPrimitive<u8>
    + FromPrimitive<u16>
    + FromPrimitive<u32>
    + FromPrimitive<u64>
    + FromPrimitive<usize>
    + FromPrimitive<i8>
    + FromPrimitive<i16>
    + FromPrimitive<i32>
    + FromPrimitive<i64>
    + FromPrimitive<isize>
    + FromPrimitive<f32>
    + FromPrimitive<f64>
    + FromStr
    + ToPrimitive<i8>
    + ToPrimitive<i16>
    + ToPrimitive<i32>
    + ToPrimitive<i64>
    + ToPrimitive<u8>
    + ToPrimitive<u16>
    + ToPrimitive<u32>
    + ToPrimitive<u64>
    + ToPrimitive<usize>
    + ToPrimitive<f32>
    + ToPrimitive<f64>
    + Add<Output = Self>
    + AddAssign
    + Div<Output = Self>
    + DivAssign
    + Mul<Output = Self>
    + MulAssign
    + Sub<Output = Self>
    + SubAssign
    + Neg<Output = Self>
    + Rem<Output = Self>
    + Clone
    + Copy
    + Debug
    + Default
    + Display
    + PartialEq
    + PartialOrd
    + Send
    + Smoothable
    + Sync
    + 'static
{
    const ZERO: Self;
    const HALF: Self;
    const ONE: Self;
    const TWO: Self;

    // TODO: Add more constants / functions?
    const EPSILON: Self;
    const FRAC_1_SQRT_2: Self;
    const LN_10: Self;
    const LOG10_E: Self;

    const PI: Self;
    const TWO_PI: Self;

    const MINUS_INFINITY_DB: Self;
    const MINUS_INFINITY_GAIN: Self;
    const CONVERSION_FACTOR_DB_GAIN: Self;
    const CONVERSION_FACTOR_GAIN_DB: Self;

    #[must_use]
    fn abs(self) -> Self;
    #[must_use]
    fn ceil(self) -> Self;
    #[must_use]
    fn clamp(self, min: Self, max: Self) -> Self;
    #[must_use]
    fn clamp_0_1(self) -> Self;
    #[must_use]
    fn clamp_1_1(self) -> Self;
    #[must_use]
    fn cos(self) -> Self;
    #[must_use]
    fn cosh(self) -> Self;
    #[must_use]
    fn exp(self) -> Self;
    #[must_use]
    fn floor(self) -> Self;
    #[must_use]
    fn ln(self) -> Self;
    #[must_use]
    fn log(self, base: Self) -> Self;
    #[must_use]
    fn log2(self) -> Self;
    #[must_use]
    fn log10(self) -> Self;
    #[must_use]
    fn min(self, other: Self) -> Self;
    #[must_use]
    fn max(self, other: Self) -> Self;
    #[must_use]
    fn powf(self, n: Self) -> Self;
    #[must_use]
    fn powi(self, n: i32) -> Self;
    #[must_use]
    fn recip(self) -> Self;
    #[must_use]
    fn round(self) -> Self;
    #[must_use]
    fn signum(self) -> Self;
    #[must_use]
    fn sin(self) -> Self;
    #[must_use]
    fn sinh(self) -> Self;
    #[must_use]
    fn sqrt(self) -> Self;
    #[must_use]
    fn tan(self) -> Self;
    #[must_use]
    fn tanh(self) -> Self;
}

macro_rules! impl_sample {
    ($sample:ident) => {
        impl Sample for $sample {
            const ZERO: Self = 0.0;
            const HALF: Self = 0.5;
            const ONE: Self = 1.0;
            const TWO: Self = 2.0;

            const EPSILON: Self = std::$sample::EPSILON;
            const FRAC_1_SQRT_2: Self = std::$sample::consts::FRAC_1_SQRT_2;
            const LN_10: Self = std::$sample::consts::LN_10;
            const LOG10_E: Self = std::$sample::consts::LOG10_E;

            const PI: Self = std::$sample::consts::PI;
            const TWO_PI: Self = 2.0 * Self::PI;

            const MINUS_INFINITY_DB: Self = -100.0;
            const MINUS_INFINITY_GAIN: Self = 1e-5; // 10.0.powf(MINUS_INFINITY_DB / 20.0)
            const CONVERSION_FACTOR_DB_GAIN: Self = Self::LN_10 / 20.0;
            const CONVERSION_FACTOR_GAIN_DB: Self = Self::LOG10_E * 20.0;

            #[inline]
            fn abs(self) -> Self {
                $sample::abs(self)
            }

            #[inline]
            fn ceil(self) -> Self {
                $sample::ceil(self)
            }

            #[inline]
            fn clamp(self, min: Self, max: Self) -> Self {
                $sample::clamp(self, min, max)
            }

            #[inline]
            fn clamp_0_1(self) -> Self {
                self.clamp(0.0, 1.0)
            }

            #[inline]
            fn clamp_1_1(self) -> Self {
                self.clamp(-1.0, 1.0)
            }

            #[inline]
            fn cos(self) -> Self {
                $sample::cos(self)
            }

            #[inline]
            fn cosh(self) -> Self {
                $sample::cosh(self)
            }

            #[inline]
            fn exp(self) -> Self {
                $sample::exp(self)
            }

            #[inline]
            fn floor(self) -> Self {
                $sample::floor(self)
            }

            #[inline]
            fn ln(self) -> Self {
                $sample::ln(self)
            }

            #[inline]
            fn log(self, base: Self) -> Self {
                $sample::log(self, base)
            }

            #[inline]
            fn log10(self) -> Self {
                $sample::log10(self)
            }

            #[inline]
            fn log2(self) -> Self {
                $sample::log2(self)
            }

            #[inline]
            fn min(self, other: Self) -> Self {
                $sample::min(self, other)
            }

            #[inline]
            fn max(self, other: Self) -> Self {
                $sample::max(self, other)
            }

            #[inline]
            fn powf(self, n: Self) -> Self {
                $sample::powf(self, n)
            }

            #[inline]
            fn powi(self, n: i32) -> Self {
                $sample::powi(self, n)
            }

            #[inline]
            fn recip(self) -> Self {
                $sample::recip(self)
            }

            #[inline]
            fn round(self) -> Self {
                $sample::round(self)
            }

            #[inline]
            fn signum(self) -> Self {
                $sample::signum(self)
            }

            #[inline]
            fn sin(self) -> Self {
                $sample::sin(self)
            }

            #[inline]
            fn sinh(self) -> Self {
                $sample::sinh(self)
            }

            #[inline]
            fn sqrt(self) -> Self {
                $sample::sqrt(self)
            }

            #[inline]
            fn tan(self) -> Self {
                $sample::tan(self)
            }

            #[inline]
            fn tanh(self) -> Self {
                $sample::tanh(self)
            }
        }
    };
}

impl_sample!(f32);
impl_sample!(f64);

pub trait FromPrimitive<P> {
    fn from_p(p: P) -> Self;
}

pub trait ToPrimitive<P> {
    fn to_p(self) -> P;
}

macro_rules! impl_primitive {
    ($sample:ident, $prim:ident) => {
        impl FromPrimitive<$prim> for $sample {
            fn from_p(p: $prim) -> Self {
                p as _
            }
        }

        impl ToPrimitive<$prim> for $sample {
            fn to_p(self) -> $prim {
                self as _
            }
        }
    };
}

macro_rules! impl_primitives {
    ($sample:ident) => {
        impl_primitive!($sample, u8);
        impl_primitive!($sample, u16);
        impl_primitive!($sample, u32);
        impl_primitive!($sample, u64);
        impl_primitive!($sample, usize);
        impl_primitive!($sample, i8);
        impl_primitive!($sample, i16);
        impl_primitive!($sample, i32);
        impl_primitive!($sample, i64);
        impl_primitive!($sample, isize);
        impl_primitive!($sample, f32);
        impl_primitive!($sample, f64);
    };
}

impl_primitives!(f32);
impl_primitives!(f64);

/// A type that can be smoothed. This exists just to avoid duplicate explicit implementations for
/// the smoothers.
pub trait Smoothable: Default + Clone + Copy {
    /// The atomic representation of `Self`.
    type Atomic: Default + Debug + Send + Sync;

    fn from_s<S: Sample>(s: S) -> Self;
    fn to_s<S: Sample>(self) -> S;

    fn atomic_new(value: Self) -> Self::Atomic;
    fn atomic_load(this: &Self::Atomic, ordering: Ordering) -> Self;
    fn atomic_store(this: &Self::Atomic, value: Self, ordering: Ordering);
    fn atomic_swap(this: &Self::Atomic, value: Self, ordering: Ordering) -> Self;
}

macro_rules! impl_smoothable {
    ($name:ident, $atomic:ident) => {
        impl Smoothable for $name {
            type Atomic = $atomic;

            #[inline]
            fn from_s<S: Sample>(s: S) -> Self {
                s.to_p()
            }

            #[inline]
            fn to_s<S: Sample>(self) -> S {
                S::from_p(self)
            }

            #[inline]
            fn atomic_new(value: Self) -> Self::Atomic {
                Self::Atomic::new(value)
            }

            #[inline]
            fn atomic_load(this: &Self::Atomic, ordering: Ordering) -> Self {
                this.load(ordering)
            }

            #[inline]
            fn atomic_store(this: &Self::Atomic, value: Self, ordering: Ordering) {
                this.store(value, ordering)
            }

            #[inline]
            fn atomic_swap(this: &Self::Atomic, value: Self, ordering: Ordering) -> Self {
                this.swap(value, ordering)
            }
        }
    };
}

impl_smoothable!(i32, AtomicI32);
impl_smoothable!(f32, AtomicF32);
impl_smoothable!(f64, AtomicF64);

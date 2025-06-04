//! Wrappers for different plugin types. Each wrapper has an entry point macro that you can pass the
//! name of a type that implements `Plugin` to. The macro will handle the rest.

#[cfg(feature = "clap")]
pub mod clap;
pub mod state;
pub(crate) mod util;

#[cfg(all(feature = "au", target_os = "macos"))]
pub mod au;
#[cfg(feature = "standalone")]
pub mod standalone;
#[cfg(feature = "vst3")]
pub mod vst3;

// This is used by the wrappers.
pub use util::setup_logger;

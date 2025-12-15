//! Audio abstraction with pluggable backends.
//!
//! The rest of the codebase only talks to `AudioSink`, while the concrete
//! backend is selected at compile time via Cargo features. This keeps the
//! `#[cfg]` usage localized to this module instead of leaking through the app.

use std::time::Duration;

use crate::HardwareError;

#[cfg(target_os = "espidf")]
mod backends;
#[cfg(not(target_os = "espidf"))]
pub mod host;

/// Minimal audio surface area needed by the app.
pub trait AudioSink {
    fn tx_enable(&mut self) -> Result<(), HardwareError>;
    fn tx_disable(&mut self) -> Result<(), HardwareError>;
    fn preload_data(&mut self, data: &[u8]) -> Result<usize, HardwareError>;
    fn write(&mut self, data: &[u8], timeout: Duration) -> Result<usize, HardwareError>;
}

/// Concrete audio device selected for this build.
#[cfg(target_os = "espidf")]
pub use backends::{init_default_audio, DefaultAudio as AudioDevice};

/// Host stub used on non-ESP targets so tests/desktop runs work.
#[cfg(not(target_os = "espidf"))]
pub use host::{init_default_audio, HostAudio as AudioDevice};

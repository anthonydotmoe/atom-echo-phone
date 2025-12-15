use esp_idf_hal::peripherals::{I2s0, Pins};

use crate::HardwareError;

mod es8311;
mod i2s_simple;

pub use es8311::Es8311Audio;
pub use i2s_simple::I2sAudio;

#[cfg(all(feature = "audio-es8311", feature = "audio-i2s-simple"))]
compile_error!("Enable only one of audio-es8311 or audio-i2s-simple");

#[cfg(feature = "audio-es8311")]
pub type DefaultAudio = Es8311Audio;

#[cfg(all(not(feature = "audio-es8311"), feature = "audio-i2s-simple"))]
pub type DefaultAudio = I2sAudio;

#[cfg(all(not(feature = "audio-es8311"), not(feature = "audio-i2s-simple")))]
compile_error!("Enable an audio backend feature (audio-es8311 or audio-i2s-simple)");

/// Construct the selected backend using the Atom Echo pinout.
pub fn init_default_audio(i2s: I2s0, pins: &mut Pins) -> Result<DefaultAudio, HardwareError> {
    #[cfg(feature = "audio-es8311")]
    {
        return Es8311Audio::new_atom_echo(i2s, pins);
    }

    #[cfg(not(feature = "audio-es8311"))]
    {
        // Default to the bare I2S amp.
        I2sAudio::new_atom_echo(i2s, pins)
    }
}

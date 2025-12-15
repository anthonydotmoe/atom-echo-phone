use std::time::Duration;

use esp_idf_hal::delay::TickType;
use esp_idf_hal::gpio::AnyIOPin;
use esp_idf_hal::i2s::{config::StdConfig, I2sDriver, I2sTx};
use esp_idf_hal::peripherals::{I2s0, Pins};
use esp_idf_sys::EspError;

use crate::audio::AudioSink;
use crate::HardwareError;

/// ES8311 codec backend.
///
/// For now we reuse the same I2S wiring as the bare amp and leave codec
/// register programming as a follow-up. The goal is to keep the call sites
/// identical while letting the backend evolve independently.
pub struct Es8311Audio {
    speaker: I2sDriver<'static, I2sTx>,
}

impl Es8311Audio {
    pub fn new_atom_echo(i2s: I2s0, pins: &mut Pins) -> Result<Self, HardwareError> {
        let bclk = pins.gpio19;
        let dout = pins.gpio22;
        let ws = pins.gpio33;

        // The ES8311 can be clocked from 8 kHz frames; revise as the codec
        // configuration gets fleshed out.
        let speaker_cfg =
            StdConfig::msb(8_000, esp_idf_hal::i2s::config::DataBitWidth::Bits16);

        let speaker = I2sDriver::<I2sTx>::new_std_tx(
            i2s,
            &speaker_cfg,
            bclk,
            dout,
            Option::<AnyIOPin>::None,
            ws,
        )
        .map_err(map_audio_err)?;

        Ok(Self { speaker })
    }
}

impl AudioSink for Es8311Audio {
    fn tx_enable(&mut self) -> Result<(), HardwareError> {
        self.speaker.tx_enable().map_err(map_audio_err)
    }

    fn tx_disable(&mut self) -> Result<(), HardwareError> {
        self.speaker.tx_disable().map_err(map_audio_err)
    }

    fn preload_data(&mut self, data: &[u8]) -> Result<usize, HardwareError> {
        self.speaker.preload_data(data).map_err(map_audio_err)
    }

    fn write(&mut self, data: &[u8], timeout: Duration) -> Result<usize, HardwareError> {
        let tick_timeout = TickType::from(timeout);
        self.speaker.write(data, tick_timeout.into()).map_err(map_audio_err)
    }
}

fn map_audio_err(err: EspError) -> HardwareError {
    log::error!("Audio error: {:?}", err);
    HardwareError::Audio("audio error")
}

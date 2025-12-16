use super::super::{HardwareError, WifiConfig};
use esp_idf_hal::sys::EspError;

mod wifi;

#[cfg(feature = "board_atom_echo")]
mod board_atom_echo;
#[cfg(feature = "board_s3_touch")]
mod board_s3_touch;

#[cfg(feature = "board_atom_echo")]
pub use board_atom_echo::{DeviceInner, UiDevice, init_device};

#[cfg(feature = "board_s3_touch")]
pub use board_s3_touch::{DeviceInner, UiDevice, init_device};

pub fn map_audio_err(err: EspError) -> HardwareError {
    // We log the detailed error; the enum just carries a coarse category.
    log::error!("Audio error: {:?}", err);
    HardwareError::Wifi("audio error")
}

pub fn map_gpio_err(err: EspError) -> HardwareError {
    // We log the detailed error; the enum just carries a coarse category.
    log::error!("GPIO error: {:?}", err);
    HardwareError::Wifi("gpio error")
}

pub fn random_u32() -> u32 {
    unsafe { esp_idf_svc::sys::esp_random() }
}

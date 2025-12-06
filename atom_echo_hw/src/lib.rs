use thiserror::Error;

#[derive(Debug, Error)]
pub enum HardwareError {
    #[error("operation not available: {0}")]
    Unsupported(&'static str),
    #[error("operation failed: {0}")]
    Failure(&'static str),
}

#[derive(Debug, Clone)]
pub struct WifiConfig {
    pub ssid: String,
    pub password: String,
}

#[derive(Debug, Clone, Copy)]
pub struct LedState {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonState {
    Pressed,
    Released,
}

#[derive(Debug, Default)]
pub struct WifiHandle;

#[derive(Debug, Default)]
pub struct AudioHandle;

pub fn init_wifi(config: WifiConfig) -> Result<WifiHandle, HardwareError> {
    #[cfg(target_os = "espidf")]
    return imp::init_wifi(config);

    #[cfg(not(target_os = "espidf"))]
    return imp::init_wifi(config);
}

pub fn init_audio() -> Result<AudioHandle, HardwareError> {
    #[cfg(target_os = "espidf")]
    return imp::init_audio();

    #[cfg(not(target_os = "espidf"))]
    return imp::init_audio();
}

pub fn read_mic_frame(handle: &mut AudioHandle, buf: &mut [i16]) -> Result<usize, HardwareError> {
    #[cfg(target_os = "espidf")]
    return imp::read_mic_frame(handle, buf);

    #[cfg(not(target_os = "espidf"))]
    return imp::read_mic_frame(handle, buf);
}

pub fn write_speaker_frame(handle: &mut AudioHandle, buf: &[i16]) -> Result<usize, HardwareError> {
    #[cfg(target_os = "espidf")]
    return imp::write_speaker_frame(handle, buf);

    #[cfg(not(target_os = "espidf"))]
    return imp::write_speaker_frame(handle, buf);
}

pub fn read_button_state(handle: &AudioHandle) -> ButtonState {
    #[cfg(target_os = "espidf")]
    return imp::read_button_state(handle);

    #[cfg(not(target_os = "espidf"))]
    return imp::read_button_state(handle);
}

pub fn set_led_state(handle: &mut AudioHandle, state: LedState) -> Result<(), HardwareError> {
    #[cfg(target_os = "espidf")]
    return imp::set_led_state(handle, state);

    #[cfg(not(target_os = "espidf"))]
    return imp::set_led_state(handle, state);
}

#[cfg(target_os = "espidf")]
mod imp {
    use super::*;
    use log::info;

    pub fn init_wifi(config: WifiConfig) -> Result<WifiHandle, HardwareError> {
        info!("initializing Wi-Fi for ssid {}", config.ssid);
        Ok(WifiHandle)
    }

    pub fn init_audio() -> Result<AudioHandle, HardwareError> {
        info!("initializing audio peripherals");
        Ok(AudioHandle)
    }

    pub fn read_mic_frame(_handle: &mut AudioHandle, buf: &mut [i16]) -> Result<usize, HardwareError> {
        buf.fill(0);
        Ok(buf.len())
    }

    pub fn write_speaker_frame(_handle: &mut AudioHandle, buf: &[i16]) -> Result<usize, HardwareError> {
        Ok(buf.len())
    }

    pub fn read_button_state(_handle: &AudioHandle) -> ButtonState {
        ButtonState::Released
    }

    pub fn set_led_state(_handle: &mut AudioHandle, _state: LedState) -> Result<(), HardwareError> {
        Ok(())
    }
}

#[cfg(not(target_os = "espidf"))]
mod imp {
    use super::*;
    use log::debug;

    pub fn init_wifi(config: WifiConfig) -> Result<WifiHandle, HardwareError> {
        debug!("simulated Wi-Fi init for ssid {}", config.ssid);
        Ok(WifiHandle)
    }

    pub fn init_audio() -> Result<AudioHandle, HardwareError> {
        debug!("simulated audio init");
        Ok(AudioHandle)
    }

    pub fn read_mic_frame(_handle: &mut AudioHandle, buf: &mut [i16]) -> Result<usize, HardwareError> {
        buf.fill(0);
        Ok(buf.len())
    }

    pub fn write_speaker_frame(_handle: &mut AudioHandle, buf: &[i16]) -> Result<usize, HardwareError> {
        Ok(buf.len())
    }

    pub fn read_button_state(_handle: &AudioHandle) -> ButtonState {
        ButtonState::Released
    }

    pub fn set_led_state(_handle: &mut AudioHandle, _state: LedState) -> Result<(), HardwareError> {
        Ok(())
    }
}

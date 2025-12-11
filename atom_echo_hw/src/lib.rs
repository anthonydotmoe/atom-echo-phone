//! Hardware abstraction for the M5Stack Atom Echo.
//!
//! This crate provides a single `Device` handle which owns all the
//! peripherals we care about (Wi-Fi, I2S audio, button, and LED),
//! plus a small, stable API the rest of the app can call.

#![cfg_attr(not(target_os = "espidf"), allow(unused))]

use std::net::Ipv4Addr;

use heapless::String;

pub use crate::imp::{AudioDevice, UiDevice};

pub type SmallString<const N: usize> = String<N>;

#[derive(Debug, Clone)]
pub struct WifiConfig {
    pub ssid: SmallString<32>,
    pub password: SmallString<64>,
    pub username: Option<SmallString<32>>,
}

impl WifiConfig {
    pub fn new(ssid: &str, password: &str, username: Option<&str>) -> Result<Self, HardwareError> {
        let mut ssid_buf = SmallString::<32>::new();
        ssid_buf
            .push_str(ssid)
            .map_err(|_| HardwareError::Config("SSID too long"))?;

        let mut pwd_buf = SmallString::<64>::new();
        pwd_buf
            .push_str(password)
            .map_err(|_| HardwareError::Config("password too long"))?;

        let user_buf = if let Some(user) = username {
            let mut username_buf = SmallString::<32>::new();
            username_buf
                .push_str(user)
                .map_err(|_| HardwareError::Config("username too long"))?;

            Some(username_buf)
        } else {
            None
        };

        Ok(Self {
            ssid: ssid_buf,
            password: pwd_buf,
            username: user_buf,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedState {
    Off,
    Color { red: u8, green: u8, blue: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonState {
    Pressed,
    Released,
}

#[derive(Debug)]
pub enum HardwareError {
    Wifi(&'static str),
    Audio(&'static str),
    Gpio(&'static str),
    Config(&'static str),
    Other(&'static str),
}

/// Public device handle used by the rest of the app.
///
/// Internally this wraps a platform-specific `DeviceInner` that owns
/// all peripherals (Wi-Fi, I2S, button GPIO, LED driver, etc.).
pub struct Device {
    inner: imp::DeviceInner,
}

impl Device {
    /// Initialize Atom Echo hardware and connect to Wi-Fi.
    ///
    /// On `espidf` this configures:
    /// - Wi-Fi in client mode
    /// - I2S in 16-bit, 8 kHz bidirectional mode
    /// - (later) button GPIO and neopixel driver
    ///
    /// On non-`espidf` targets this creates a simulated device for host testing.
    pub fn init(config: WifiConfig) -> Result<Self, HardwareError> {
        let inner = imp::init_device(config)?;
        Ok(Device { inner })
    }

    pub fn get_audio_device(&mut self) -> Result<AudioDevice, HardwareError> {
        self.inner.get_audio_device()
    }

    pub fn get_ui_device(&mut self) -> Result<UiDevice, HardwareError> {
        self.inner.get_ui_device()
    }

    pub fn get_ip_addr(&self) -> Ipv4Addr {
        self.inner.get_ip_addr()
    }
}

// Platform-specific implementation lives in `imp`:
mod imp;

/// Return a random 32-bit value.
/// 
/// On ESP-IDF this uses `esp_random`, on hosts it falls back to the `rand`
/// crate.
pub fn random_u32() -> u32 {
    imp::random_u32()
}

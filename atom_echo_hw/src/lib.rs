//! Hardware abstraction for the M5Stack Atom Echo.
//!
//! This crate provides a single `Device` handle which owns all the
//! peripherals we care about (Wi-Fi, I2S audio, button, and LED),
//! plus a small, stable API the rest of the app can call.

#![cfg_attr(not(target_os = "espidf"), allow(unused))]

use heapless::String;

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

    /// Read a frame of PCM samples from the microphone.
    ///
    /// The exact semantics (frame size, blocking vs non-blocking) are
    /// up to the caller; this function will try to fill `buf` and
    /// return the number of samples actually read.
    pub fn read_mic_frame(&mut self, buf: &mut [i16]) -> Result<usize, HardwareError> {
        self.inner.read_mic_frame(buf)
    }

    // Write a frame of PCM samples to the speaker.
    //
    // Returns the number of samples accepted.
    /*
    pub fn write_speaker_frame(&mut self, buf: &[i16]) -> Result<usize, HardwareError> {
        self.inner.write_speaker_frame(buf)
    }
    */

    // Read the current debounced button state.
    /*
    pub fn read_button_state(&self) -> ButtonState {
        self.inner.read_button_state()
    }
    */

    // Set the neopixel LED to a given state.
    /*
    pub fn set_led_state(&mut self, state: LedState) -> Result<(), HardwareError> {
        self.inner.set_led_state(state)
    }
    */
}

// Platform-specific implementation lives in `imp`:
mod imp;

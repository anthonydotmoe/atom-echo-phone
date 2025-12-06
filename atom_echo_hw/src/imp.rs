use super::{ButtonState, HardwareError, LedState, WifiConfig};

#[cfg(target_os = "espidf")]
mod esp {
    use super::*;
    use esp_idf_hal::gpio::AnyIOPin;
    use esp_idf_hal::i2s::{config::StdConfig, I2sBiDir, I2sDriver};
    use esp_idf_svc::eventloop::EspSystemEventLoop;
    use esp_idf_svc::wifi::{ClientConfiguration, Configuration, EspWifi};
    use esp_idf_sys::EspError;
    use heapless::String;
    use log::info;

    /// Concrete device handle on ESP-IDF.
    ///
    /// Owns Wi-Fi and I2S; button and LED will be wired in here as they
    /// are implemented.
    pub struct DeviceInner {
        wifi: EspWifi<'static>,
        i2s: I2sDriver<I2sBiDir>,
        // TODO: add button GPIO and LED driver fields here.
    }

    pub fn init_device(config: WifiConfig) -> Result<DeviceInner, HardwareError> {
        // Take all shared peripherals once and wire them into the handle.
        let peripherals = esp_idf_hal::peripherals::Peripherals::take()
            .map_err(map_wifi_err)?;
        let sysloop = EspSystemEventLoop::take().map_err(map_wifi_err)?;

        // --- Wi-Fi ---
        let mut wifi = EspWifi::new(peripherals.modem, sysloop, None)
            .map_err(map_wifi_err)?;

        // Convert heapless::String to the types EspWifi expects.
        let mut ssid = String::<32>::new();
        ssid.push_str(&config.ssid)
            .map_err(|_| HardwareError::Config("SSID too long"))?;

        let mut password = String::<64>::new();
        password
            .push_str(&config.password)
            .map_err(|_| HardwareError::Config("password too long"))?;

        let client_conf = ClientConfiguration {
            ssid,
            password,
            ..Default::default()
        };

        wifi.set_configuration(&Configuration::Client(client_conf))
            .map_err(map_wifi_err)?;
        wifi.start().map_err(map_wifi_err)?;
        wifi.connect().map_err(map_wifi_err)?;

        info!("Wi-Fi connected");

        // --- I2S audio ---
        let pins = peripherals.pins;

        let bclk = pins.gpio19;
        let din = pins.gpio23;
        let dout = pins.gpio22;
        let ws = pins.gpio33;
        let mclk: Option<AnyIOPin> = None;

        // 16-bit PCM at 8 kHz, Philips standard.
        let std_config = StdConfig::philips(8_000, esp_idf_hal::i2s::config::DataBitWidth::Bits16);

        let i2s = I2sDriver::<I2sBiDir>::new_std_bidir(
            peripherals.i2s0,
            &std_config,
            bclk,
            din,
            dout,
            mclk,
            ws,
        )
        .map_err(map_audio_err)?;

        info!("I2S configured for bidirectional audio");

        Ok(DeviceInner {
            wifi,
            i2s,
            // button, led fields to be added when implemented
        })
    }

    impl DeviceInner {
        pub fn read_mic_frame(&mut self, buf: &mut [i16]) -> Result<usize, HardwareError> {
            // TODO: implement real I2S read
            //
            // For now, just fill with silence so the rest of the stack
            // can be exercised without audio hardware wired up.
            buf.fill(0);
            Ok(buf.len())
        }

        pub fn write_speaker_frame(&mut self, buf: &[i16]) -> Result<usize, HardwareError> {
            // TODO: implement real I2S write
            let _ = &self.i2s; // keep field "used" for now
            let _ = buf;
            Ok(buf.len())
        }

        pub fn read_button_state(&self) -> ButtonState {
            // TODO: configure and read the actual GPIO (e.g., GPIO 39)
            ButtonState::Released
        }

        pub fn set_led_state(&mut self, state: LedState) -> Result<(), HardwareError> {
            // TODO: drive the Atom Echo neopixel via RMT or bit-banged GPIO.
            match state {
                LedState::Off => {
                    // turn off LED
                }
                LedState::Color { red, green, blue } => {
                    let _ = (red, green, blue);
                    // set LED color
                }
            }
            Ok(())
        }
    }

    fn map_wifi_err(err: EspError) -> HardwareError {
        // We log the detailed error; the enum just carries a coarse category.
        log::error!("Wi-Fi error: {:?}", err);
        HardwareError::Wifi("Wi-Fi error")
    }

    fn map_audio_err(err: EspError) -> HardwareError {
        log::error!("Audio error: {:?}", err);
        HardwareError::Audio("audio error")
    }
}

#[cfg(not(target_os = "espidf"))]
mod host {
    use super::*;
    use log::debug;

    /// Host-side fake device handle for unit tests / desktop builds.
    #[derive(Debug, Default)]
    pub struct DeviceInner;

    pub fn init_device(config: WifiConfig) -> Result<DeviceInner, HardwareError> {
        debug!(
            "simulated Atom Echo init: ssid='{}'",
            config.ssid
        );
        Ok(DeviceInner)
    }

    impl DeviceInner {
        pub fn read_mic_frame(&mut self, buf: &mut [i16]) -> Result<usize, HardwareError> {
            // host: just zero-fill
            buf.fill(0);
            Ok(buf.len())
        }

        pub fn write_speaker_frame(&mut self, buf: &[i16]) -> Result<usize, HardwareError> {
            debug!("simulated speaker write: {} samples", buf.len());
            Ok(buf.len())
        }

        pub fn read_button_state(&self) -> ButtonState {
            ButtonState::Released
        }

        pub fn set_led_state(&mut self, state: LedState) -> Result<(), HardwareError> {
            debug!("simulated LED state: {:?}", state);
            Ok(())
        }
    }
}

#[cfg(target_os = "espidf")]
pub use esp::DeviceInner;
#[cfg(not(target_os = "espidf"))]
pub use host::DeviceInner;

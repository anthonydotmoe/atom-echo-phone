use super::{ButtonState, HardwareError, LedState, WifiConfig};

#[cfg(target_os = "espidf")]
mod esp {
    use super::*;
    use esp_idf_hal::gpio::AnyIOPin;
    use esp_idf_hal::gpio::{Gpio39, Input, PinDriver};
    use esp_idf_hal::i2s::{config::StdConfig, I2sBiDir, I2sDriver};
    use esp_idf_hal::peripherals::Peripherals;
    use esp_idf_hal::rmt::{config::TransmitConfig, FixedLengthSignal, PinState, Pulse, TxRmtDriver};
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
        i2s: I2sDriver<'static, I2sBiDir>,
        button: PinDriver<'static, Gpio39, Input>,
        led: TxRmtDriver<'static>,
    }

    pub fn init_device(config: WifiConfig) -> Result<DeviceInner, HardwareError> {
        // Take all shared peripherals once and wire them into the handle.
        let peripherals = Peripherals::take().map_err(map_wifi_err)?;
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

        // 16-bit PCM at 8 kHz, Philips standard.
        let std_config = StdConfig::philips(8_000, esp_idf_hal::i2s::config::DataBitWidth::Bits16);

        let i2s = I2sDriver::<I2sBiDir>::new_std_bidir(
            peripherals.i2s0,
            &std_config,
            bclk,
            din,
            dout,
            Option::<AnyIOPin>::None,
            ws,
        )
        .map_err(map_audio_err)?;

        info!("I2S configured for bidirectional audio");

        // Button input (pull-up, active-low)
        let button_pin = pins.gpio39;
        let button = PinDriver::input(button_pin).map_err(map_gpio_err)?;

        // LED via RMT-driven WS2812
        let led_pin = pins.gpio27;
        let led = TxRmtDriver::new(
            peripherals.rmt.channel0,
            led_pin,
            &TransmitConfig::new().clock_divider(2),
        )
        .map_err(map_gpio_err)?;

        Ok(DeviceInner {
            wifi,
            i2s,
            button,
            led,
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
            // Active-low button: low means pressed.
            if self.button.is_low() {
                ButtonState::Pressed
            } else {
                ButtonState::Released
            }
        }

        pub fn set_led_state(&mut self, state: LedState) -> Result<(), HardwareError> {
            let (g, r, b) = match state {
                LedState::Off => (0, 0, 0),
                LedState::Color { red, green, blue } => (green, red, blue), // GRB order
            };

            // WS2812 timing: T0H=0.35us, T0L=0.8us, T1H=0.7us, T1L=0.6us
            let ticks_hz = self.led.counter_clock().map_err(map_gpio_err)?;
            let t0h = Pulse::new_with_duration(ticks_hz, PinState::High, &core::time::Duration::from_nanos(350))
                .map_err(map_gpio_err)?;
            let t0l = Pulse::new_with_duration(ticks_hz, PinState::Low, &core::time::Duration::from_nanos(800))
                .map_err(map_gpio_err)?;
            let t1h = Pulse::new_with_duration(ticks_hz, PinState::High, &core::time::Duration::from_nanos(700))
                .map_err(map_gpio_err)?;
            let t1l = Pulse::new_with_duration(ticks_hz, PinState::Low, &core::time::Duration::from_nanos(600))
                .map_err(map_gpio_err)?;

            let mut signal = FixedLengthSignal::<24>::new();
            let bits = [g, r, b];
            let mut idx = 0;
            for &component in &bits {
                for bit in (0..8).rev() {
                    let is_one = (component >> bit) & 1 == 1;
                    let (h, l) = if is_one { (t1h, t1l) } else { (t0h, t0l) };
                    signal.set(idx, &(h, l)).map_err(map_gpio_err)?;
                    idx += 1;
                }
            }

            self.led.start_blocking(&signal).map_err(map_gpio_err)
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

    fn map_gpio_err(err: EspError) -> HardwareError {
        log::error!("GPIO error: {:?}", err);
        HardwareError::Gpio("gpio error")
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

#[cfg(target_os = "espidf")]
pub use esp::init_device;
#[cfg(not(target_os = "espidf"))]
pub use host::init_device;

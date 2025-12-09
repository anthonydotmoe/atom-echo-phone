use super::{ButtonState, HardwareError, LedState, WifiConfig};

#[cfg(target_os = "espidf")]
mod esp {
    use std::time::Duration;

    use esp_idf_svc::hal as esp_idf_hal;
    use esp_idf_svc::sys::{esp_eap_client_set_password, esp_eap_client_set_username, esp_wifi_sta_enterprise_enable};
    use esp_idf_svc::sys as esp_idf_sys;

    use super::*;
    use esp_idf_hal::gpio::AnyIOPin;
    use esp_idf_hal::gpio::{Gpio39, Input, PinDriver};
    use esp_idf_hal::i2s::{config::StdConfig, I2sBiDir, I2sDriver};
    use esp_idf_hal::peripherals::Peripherals;
    use esp_idf_hal::rmt::{config::TransmitConfig, FixedLengthSignal, PinState, Pulse};
    use esp_idf_svc::eventloop::EspSystemEventLoop;
    use esp_idf_svc::wifi::{AuthMethod, ClientConfiguration, Configuration, EspWifi};
    use esp_idf_svc::nvs::EspDefaultNvsPartition;
    use esp_idf_sys::esp_eap_client_set_identity;
    use esp_idf_sys::EspError;
    use heapless::String;

    /// Concrete device handle on ESP-IDF.
    ///
    /// Owns Wi-Fi and I2S; button and LED will be wired in here as they
    /// are implemented.
    pub struct DeviceInner {
        wifi: EspWifi<'static>,
        /*
        i2s: I2sDriver<'static, I2sBiDir>,
        button: PinDriver<'static, Gpio39, Input>,
        led: TxRmtDriver<'static>,
        */
    }

    pub fn init_device(config: WifiConfig) -> Result<DeviceInner, HardwareError> {
        // Take all shared peripherals once and wire them into the handle.
        let peripherals = Peripherals::take().map_err(map_wifi_err)?;
        let sysloop = EspSystemEventLoop::take().map_err(map_wifi_err)?;
        let nvs = EspDefaultNvsPartition::take().map_err(map_wifi_err)?;

        // --- Wi-Fi ---
        let mut wifi = EspWifi::new(
            peripherals.modem,
            sysloop,
            Some(nvs)
        )
            .map_err(map_wifi_err)?;

        // If there's a username, use WPAn-Enterprise
        if let Some(username) = config.username {
            init_wifi_enterprise(&mut wifi, &config.ssid, &username, &config.password)?;
        } else {
            init_wifi_personal(&mut wifi, &config.ssid, &config.password)?;
        }

        wifi.start().map_err(map_wifi_err)?;
        wifi.connect().map_err(map_wifi_err)?;


        loop {
            let ret = wifi.is_connected().unwrap();
            if ret {
                break;
            }

            log::info!("WiFi connecting...");
            std::thread::sleep(Duration::from_secs(1));
        }
        
        let ip = loop {
            // Wait for address
            let netif = wifi.sta_netif();
            match netif.get_ip_info() {
                Ok(info) => {
                    if !info.ip.is_unspecified() {
                        break info.ip
                    }
                }
                Err(e) => {
                    log::error!("get_ip_info: {}", e);
                }
            }
            std::thread::sleep(Duration::from_secs(1));
        };


        log::info!("Wi-Fi connected");
        log::info!("IP: {}", ip);

        /*
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

        log::info!("I2S configured for bidirectional audio");

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
        */

        Ok(DeviceInner {
            wifi,
            //i2s,
            //button,
            //led,
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

        /*
        pub fn write_speaker_frame(&mut self, buf: &[i16]) -> Result<usize, HardwareError> {
            // TODO: implement real I2S write
            let _ = &self.i2s; // keep field "used" for now
            let _ = buf;
            Ok(buf.len())
        }
        */

        /*
        pub fn read_button_state(&self) -> ButtonState {
            // Active-low button: low means pressed.
            if self.button.is_low() {
                ButtonState::Pressed
            } else {
                ButtonState::Released
            }
        }
        */

        /*
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
    */
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

    fn init_wifi_personal(
        wifi: &mut EspWifi,
        ssid: &str,
        pass: &str,
    ) -> Result<(), HardwareError> {
        let mut h_ssid = String::<32>::new();
        h_ssid.push_str(ssid)
            .map_err(|_| HardwareError::Config("SSID too long"))?;

        let mut password = String::<64>::new();
        password.push_str(pass)
            .map_err(|_| HardwareError::Config("Password too long"))?;

        let config = ClientConfiguration {
            ssid: h_ssid,
            password,
            ..Default::default()
        };

        wifi.set_configuration(&Configuration::Client(config))
            .map_err(map_wifi_err)
    }

    fn init_wifi_enterprise(
        wifi: &mut EspWifi,
        ssid: &str,
        user: &str,
        pass: &str,
    ) -> Result<(), HardwareError> {
        log::debug!("Connecting to \"{}\"", &ssid);
        log::debug!("  user: {}", &user);
        log::debug!("  pass: {}", &pass);

        let mut h_ssid = String::<32>::new();
        h_ssid.push_str(ssid)
            .map_err(|_| HardwareError::Config("SSID too long"))?;

        // Configure with svc::wifi::set_configuration, then override
        let config = ClientConfiguration {
            ssid: h_ssid,
            ..Default::default()
        };

        wifi.set_configuration(&Configuration::Client(config))
            .map_err(map_wifi_err)?;

        // Begin override
        set_enterprise_username(user).map_err(map_wifi_err)?;
        set_enterprise_password(pass).map_err(map_wifi_err)?;

        let err = unsafe { esp_wifi_sta_enterprise_enable() };
        EspError::convert(err).map_err(map_wifi_err)
    }

    /// Configure the WPA2-Enterprise username (PEAP/TTLS)
    /// 
    /// Requirements from ESP-IDF:
    /// - length must be between 1 and 127 bytes (inclusive)
    fn set_enterprise_username(username: &str) -> Result<(), EspError> {
        let bytes = username.as_bytes();
        let len = bytes.len();

        // Enforce the documented limits: 1..=127 bytes
        if len == 0 || len >= 128 {
            return Err(EspError::from_infallible::<{ esp_idf_sys::ESP_ERR_INVALID_ARG }>());
        }

        let ptr = bytes.as_ptr() as *const _;
        let len_c = len as _;

        let err = unsafe { esp_eap_client_set_identity(ptr, len_c) };
        EspError::convert(err)?;

        let err = unsafe { esp_eap_client_set_username(ptr, len_c) };
        EspError::convert(err)
    }

    /// Configure the WPA2-Enterprise password (PEAP/TTLS)
    /// 
    /// Requirements from ESP-IDF:
    /// - length must be non-zero
    fn set_enterprise_password(password: &str) -> Result<(), EspError> {
        let bytes = password.as_bytes();
        let len = bytes.len();

        // Enforce the documented limits
        if len == 0 {
            return Err(EspError::from_infallible::<{ esp_idf_sys::ESP_ERR_INVALID_ARG }>());
        }

        let ptr = bytes.as_ptr() as *const _;
        let len_c = len as _;

        let err = unsafe { esp_eap_client_set_password(ptr, len_c) };
        EspError::convert(err)
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

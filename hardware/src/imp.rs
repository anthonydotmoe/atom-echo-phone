use super::{ButtonState, HardwareError, LedState, WifiConfig};

#[cfg(target_os = "espidf")]
mod esp {
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::Duration;

    use esp_idf_hal::delay::TickType;
    use esp_idf_svc::sys as esp_idf_sys;
    use esp_idf_sys::{
        esp_eap_client_set_password, esp_eap_client_set_username,
        esp_random, esp_wifi_sta_enterprise_enable,
    };

    use super::*;
    use esp_idf_hal::gpio::{AnyIOPin, AnyInputPin, InputPin};
    use esp_idf_hal::gpio::{Input, PinDriver};
    use esp_idf_hal::i2s::{config::StdConfig, I2sTx, I2sDriver};
    use esp_idf_hal::peripherals::Peripherals;
    use esp_idf_hal::rmt::{config::TransmitConfig, FixedLengthSignal, PinState, Pulse, TxRmtDriver};
    use esp_idf_svc::eventloop::EspSystemEventLoop;
    use esp_idf_svc::wifi::{ClientConfiguration, Configuration, EspWifi};
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
        addr: Ipv4Addr,
        ui_device: Option<UiDevice>,
        audio_device: Option<AudioDevice>,
    }

    pub struct AudioDevice {
        speaker: I2sDriver<'static, I2sTx>,
        /* mic: I2sDriver<'static, I2sRx>, */
    }

    pub struct UiDevice {
        led: TxRmtDriver<'static>,
        button: PinDriver<'static, AnyInputPin, Input>,
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

        let pins = peripherals.pins;

        // --- I2S audio ---

        let bclk = pins.gpio19;
        let _din = pins.gpio23;
        let dout = pins.gpio22;
        let ws = pins.gpio33;

        // 16-bit PCM at 8 kHz, Philips standard.
        let speaker_config = StdConfig::msb(8_000, esp_idf_hal::i2s::config::DataBitWidth::Bits16);

        let speaker = I2sDriver::<I2sTx>::new_std_tx(
            peripherals.i2s0,
            &speaker_config,
            bclk,
            dout,
            Option::<AnyIOPin>::None,
            ws
        )
        .map_err(map_audio_err)?;

        /*
        // PDM
        let mic_config = {
            let channel_cfg = i2s::config::Config::default();
            let clk_cfg = i2s::config::PdmRxClkConfig::from_sample_rate_hz(8_000);
            let slot_cfg = i2s::config::PdmRxSlotConfig::from_bits_per_sample_and_slot_mode(
                i2s::config::DataBitWidth::Bits16,
                i2s::config::SlotMode::Mono,
            );
            let gpio_cfg = i2s::config::PdmRxGpioConfig::new(false);

            PdmRxConfig::new(channel_cfg, clk_cfg, slot_cfg, gpio_cfg)
        };

        let mic = I2sDriver::<I2sRx>::new_pdm_rx(
            peripherals.i2s1,
            &mic_config,
            bclk,
            din
        )
        .map_err(map_audio_err)?;
        */

        // Button input (pull-up, active-low)
        let button_pin = pins.gpio39;
        let button = PinDriver::input(button_pin.downgrade_input()).map_err(map_gpio_err)?;

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
            addr: ip,
            ui_device: Some(UiDevice { led, button }),
            audio_device: Some(AudioDevice { speaker: speaker, /* mic: mic */ })
        })
    }

    impl DeviceInner {
        pub fn get_audio_device(&mut self) -> Result<AudioDevice, HardwareError> {
            if self.audio_device.is_none() {
                return Err(HardwareError::Other("AudioDevice already taken"));
            }

            Ok(self.audio_device.take().unwrap())
        }

        pub fn get_ui_device(&mut self) -> Result<UiDevice, HardwareError> {
            if self.ui_device.is_none() {
                return Err(HardwareError::Other("UiDevice already taken"));
            }

            Ok(self.ui_device.take().unwrap())
        }

        pub fn get_ip_addr(&self) -> IpAddr {
            return IpAddr::V4(self.addr)
        }

    }

    impl AudioDevice {
        /// Disable the I2S transmit channel.
        ///
        /// # Note
        /// This can only be called when the channel is in the `RUNNING` state: the channel has been previously enabled
        /// via a call to [`tx_enable()`][I2sTxChannel::tx_enable]. The channel will enter the `READY` state if it is disabled
        /// successfully.
        ///
        /// Disabling the channel will stop I2S communications on the hardware. BCLK and WS signals will stop being
        /// generated if this is a controller. MCLK will continue to be generated.
        ///
        /// # Errors
        /// This will return an [`EspError`] with `ESP_ERR_INVALID_STATE` if the channel is not in the `RUNNING` state.
        pub fn tx_disable(&mut self) -> Result<(), HardwareError> {
            self.speaker.tx_disable().map_err(map_audio_err)
        }

        /// Enable the I2S transmit channel.
        ///
        /// # Note
        /// This can only be called when the channel is in the `READY` state: initialized but not yet started from a driver
        /// constructor, or disabled from the `RUNNING` state via [`tx_disable()`][I2sTxChannel::tx_disable]. The channel
        /// will enter the `RUNNING` state if it is enabled successfully.
        ///
        /// Enabling the channel will start I2S communications on the hardware. BCLK and WS signals will be generated if
        /// this is a controller. MCLK will be generated once initialization is finished.
        ///
        /// # Errors
        /// This will return an [`EspError`] with `ESP_ERR_INVALID_STATE` if the channel is not in the `READY` state.
        pub fn tx_enable(&mut self) -> Result<(), HardwareError> {
            self.speaker.tx_enable().map_err(map_audio_err)
        }

        /// Preload data into the transmit channel DMA buffer.
        ///
        /// This may be called only when the channel is in the `READY` state: initialized but not yet started.
        ///
        /// This is used to preload data into the DMA buffer so that valid data can be transmitted immediately after the
        /// channel is enabled via [`tx_enable()`][I2sTxChannel::tx_enable]. If this function is not called before enabling the channel,
        /// empty data will be transmitted.
        ///
        /// This function can be called multiple times before enabling the channel. Additional calls will concatenate the
        /// data to the end of the buffer until the buffer is full.
        ///
        /// # Returns
        /// This returns the number of bytes that have been loaded into the buffer. If this is less than the length of
        /// the data provided, the buffer is full and no more data can be loaded.
        pub fn preload_data(&mut self, data: &[u8]) -> Result<usize, HardwareError> {
            self.speaker.preload_data(data).map_err(map_audio_err)
        }

        /// Write data to the channel.
        ///
        /// This may be called only when the channel is in the `RUNNING` state.
        ///
        /// # Returns
        /// This returns the number of bytes sent. This may be less than the length of the data provided.
        pub fn write(&mut self, data: &[u8], timeout: Duration) -> Result<usize, HardwareError> {
            let tick_timeout = TickType::from(timeout);
            self.speaker.write(data, tick_timeout.into()).map_err(map_audio_err)
        }
    }

    impl UiDevice {
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

        pub fn read_button_state(&self) -> ButtonState {
            // Active-low button: low means pressed.
            if self.button.is_low() {
                ButtonState::Pressed
            } else {
                ButtonState::Released
            }
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

    pub fn random_u32() -> u32 {
        unsafe { esp_random() }
    }
}

#[cfg(not(target_os = "espidf"))]
mod host {
    use super::*;
    use log::debug;
    use std::{net::{IpAddr, UdpSocket}, time::Duration};

    /// Host-side fake device handle for unit tests / desktop builds.
    #[derive(Debug)]
    pub struct DeviceInner {
        addr: IpAddr,
    }

    #[derive(Debug)]
    pub struct AudioDevice {
        buf: Vec<u8>,
        sample_rate: u32,
        channels: u16,
        bits_per_sample: u16,
    }

    impl Default for AudioDevice {
        fn default() -> Self {
            Self {
                buf: Vec::new(),
                sample_rate: 8_000,
                channels: 2,
                bits_per_sample: 16,
            }
        }
    }

    #[derive(Debug, Default)]
    pub struct UiDevice;

    pub fn init_device(config: WifiConfig) -> Result<DeviceInner, HardwareError> {
        debug!(
            "simulated Atom Echo init: ssid='{}'",
            config.ssid
        );

        // Create a socket to get ip addr
        let sock = UdpSocket::bind("0.0.0.0:0").unwrap();
        let addr = sock.local_addr().unwrap().ip();
        Ok(DeviceInner { addr })
    }

    impl DeviceInner {
        pub fn get_audio_device(&mut self) -> Result<AudioDevice, HardwareError> {
            Ok(AudioDevice::default())
        }

        pub fn get_ui_device(&mut self) -> Result<UiDevice, HardwareError> {
            Ok(UiDevice)
        }

        pub fn get_ip_addr(&self) -> IpAddr {
            return self.addr;
        }
    }
    
    impl AudioDevice {
        fn dump_wav_to_path<P: AsRef<std::path::Path>>(
            &self,
            path: P,
        ) -> std::io::Result<()> {
            use std::fs::File;
            use std::io::Write;

            if self.buf.is_empty() {
                return Ok(())
            }

            let sample_rate = self.sample_rate;
            let channels = self.channels;
            let bits_per_sample = self.bits_per_sample;

            let byte_rate =
                sample_rate * channels as u32 * (bits_per_sample as u32 / 8);
            let block_align = channels * bits_per_sample / 8;
            let subchunk2_size = self.buf.len() as u32;
            let chunk_size = 4 + (8 + 16) + (8 + subchunk2_size);

            let mut f = File::create(path)?;

            // RIFF header
            f.write_all(b"RIFF")?;
            f.write_all(&chunk_size.to_le_bytes())?;
            f.write_all(b"WAVE")?;

            // fmt chunk
            f.write_all(b"fmt ")?;
            f.write_all(&16u32.to_le_bytes())?;          // Subchunk1Size
            f.write_all(&1u16.to_le_bytes())?;           // AudioFormat = PCM
            f.write_all(&channels.to_le_bytes())?;
            f.write_all(&sample_rate.to_le_bytes())?;
            f.write_all(&byte_rate.to_le_bytes())?;
            f.write_all(&block_align.to_le_bytes())?;
            f.write_all(&bits_per_sample.to_le_bytes())?;

            // data chunk
            f.write_all(b"data")?;
            f.write_all(&subchunk2_size.to_le_bytes())?;
            f.write_all(&self.buf)?;

            Ok(())
        }
    }

    impl UiDevice {
        pub fn read_button_state(&self) -> ButtonState {
            ButtonState::Released
        }

        pub fn set_led_state(&mut self, state: LedState) -> Result<(), HardwareError> {
            debug!("simulated LED state: {:?}", state);
            Ok(())
        }
    }

    impl AudioDevice {
        /// Disable the I2S transmit channel.
        pub fn tx_disable(&mut self) -> Result<(), HardwareError> {
            let path = format!("audio_{:#08x}.wav", random_u32());
            if let Err(e) = self.dump_wav_to_path(&path) {
                eprintln!("failed to write {}: {}", &path, e);
            } else {
                eprintln!(
                    "write {} ({} bytes of audio)",
                    &path,
                    self.buf.len()
                );
            }
            
            Ok(())
        }

        /// Enable the I2S transmit channel.
        pub fn tx_enable(&mut self) -> Result<(), HardwareError> {
            Ok(())
        }

        /// Preload data into the transmit channel DMA buffer.
        pub fn preload_data(&mut self, data: &[u8]) -> Result<usize, HardwareError> {
            self.buf.extend_from_slice(data);
            Ok(data.len())
        }

        /// Write data to the channel.
        pub fn write(&mut self, data: &[u8], timeout: Duration) -> Result<usize, HardwareError> {
            self.buf.extend_from_slice(data);
            Ok(data.len())
        }
    }

    pub fn random_u32() -> u32 {
        rand::random::<u32>()
    }
}

#[cfg(target_os = "espidf")]
pub use esp::{DeviceInner, AudioDevice, UiDevice, random_u32};
#[cfg(not(target_os = "espidf"))]
pub use host::{DeviceInner, AudioDevice, UiDevice, random_u32};

#[cfg(target_os = "espidf")]
pub use esp::init_device;
#[cfg(not(target_os = "espidf"))]
pub use host::init_device;

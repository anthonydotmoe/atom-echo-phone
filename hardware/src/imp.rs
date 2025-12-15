use super::{ButtonState, HardwareError, LedState, WifiConfig};

#[cfg(target_os = "espidf")]
pub use esp::{init_device, random_u32, AudioDevice, DeviceInner, UiDevice};
#[cfg(not(target_os = "espidf"))]
pub use host::{init_device, random_u32, AudioDevice, DeviceInner, UiDevice};

#[cfg(target_os = "espidf")]
mod esp {
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::Duration;

    use esp_idf_hal::gpio::{AnyInputPin, Input, PinDriver};
    use esp_idf_hal::peripherals::Peripherals;
    use esp_idf_hal::rmt::{config::TransmitConfig, FixedLengthSignal, PinState, Pulse, TxRmtDriver};
    use esp_idf_svc::eventloop::EspSystemEventLoop;
    use esp_idf_svc::nvs::EspDefaultNvsPartition;
    use esp_idf_svc::wifi::{ClientConfiguration, Configuration, EspWifi};
    use esp_idf_svc::sys as esp_idf_sys;
    use esp_idf_sys::{
        esp_eap_client_set_identity, esp_eap_client_set_password, esp_eap_client_set_username,
        esp_random, esp_wifi_sta_enterprise_enable, EspError,
    };
    use heapless::String;

    use super::*;
    use crate::audio;

    /// Concrete device handle on ESP-IDF.
    pub struct DeviceInner {
        wifi: EspWifi<'static>,
        addr: Ipv4Addr,
        ui_device: Option<UiDevice>,
        audio_device: Option<AudioDevice>,
    }

    pub type AudioDevice = audio::AudioDevice;

    pub struct UiDevice {
        led: TxRmtDriver<'static>,
        button: PinDriver<'static, AnyInputPin, Input>,
    }

    pub fn init_device(config: WifiConfig) -> Result<DeviceInner, HardwareError> {
        let mut peripherals = Peripherals::take().map_err(map_wifi_err)?;
        let sysloop = EspSystemEventLoop::take().map_err(map_wifi_err)?;
        let nvs = EspDefaultNvsPartition::take().map_err(map_wifi_err)?;

        let mut wifi = EspWifi::new(peripherals.modem, sysloop, Some(nvs)).map_err(map_wifi_err)?;

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
            let netif = wifi.sta_netif();
            match netif.get_ip_info() {
                Ok(info) => {
                    if !info.ip.is_unspecified() {
                        break info.ip;
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

        // Pins are consumed backend-by-backend to avoid scattering cfgs.
        let mut pins = peripherals.pins;
        let rmt_channel = peripherals.rmt.channel0;
        let button_pin = pins.gpio39;
        let led_pin = pins.gpio27;

        let audio = audio::init_default_audio(peripherals.i2s0, &mut pins)?;

        // Button input (pull-up, active-low)
        let button = PinDriver::input(button_pin).map_err(map_gpio_err)?;

        // LED via RMT-driven WS2812
        let led = TxRmtDriver::new(
            rmt_channel,
            led_pin,
            &TransmitConfig::new().clock_divider(2),
        )
        .map_err(map_gpio_err)?;

        Ok(DeviceInner {
            wifi,
            addr: ip,
            ui_device: Some(UiDevice { led, button }),
            audio_device: Some(audio),
        })
    }

    impl DeviceInner {
        pub fn get_audio_device(&mut self) -> Result<AudioDevice, HardwareError> {
            self.audio_device
                .take()
                .ok_or(HardwareError::Other("AudioDevice already taken"))
        }

        pub fn get_ui_device(&mut self) -> Result<UiDevice, HardwareError> {
            self.ui_device
                .take()
                .ok_or(HardwareError::Other("UiDevice already taken"))
        }

        pub fn get_ip_addr(&self) -> IpAddr {
            IpAddr::V4(self.addr)
        }
    }

    impl UiDevice {
        pub fn set_led_state(&mut self, state: LedState) -> Result<(), HardwareError> {
            let (g, r, b) = match state {
                LedState::Off => (0, 0, 0),
                LedState::Color { red, green, blue } => (green, red, blue),
            };

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
            if self.button.is_low() {
                ButtonState::Pressed
            } else {
                ButtonState::Released
            }
        }
    }

    fn map_wifi_err(err: EspError) -> HardwareError {
        log::error!("Wi-Fi error: {:?}", err);
        HardwareError::Wifi("Wi-Fi error")
    }

    fn map_gpio_err(err: EspError) -> HardwareError {
        log::error!("GPIO error: {:?}", err);
        HardwareError::Gpio("gpio error")
    }

    fn init_wifi_personal(wifi: &mut EspWifi, ssid: &str, pass: &str) -> Result<(), HardwareError> {
        let mut h_ssid = String::<32>::new();
        h_ssid
            .push_str(ssid)
            .map_err(|_| HardwareError::Config("SSID too long"))?;

        let mut password = String::<64>::new();
        password
            .push_str(pass)
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
        h_ssid
            .push_str(ssid)
            .map_err(|_| HardwareError::Config("SSID too long"))?;

        let config = ClientConfiguration {
            ssid: h_ssid,
            ..Default::default()
        };

        wifi.set_configuration(&Configuration::Client(config))
            .map_err(map_wifi_err)?;

        set_enterprise_username(user).map_err(map_wifi_err)?;
        set_enterprise_password(pass).map_err(map_wifi_err)?;

        let err = unsafe { esp_wifi_sta_enterprise_enable() };
        EspError::convert(err).map_err(map_wifi_err)
    }

    fn set_enterprise_username(username: &str) -> Result<(), EspError> {
        let bytes = username.as_bytes();
        let len = bytes.len();

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

    fn set_enterprise_password(password: &str) -> Result<(), EspError> {
        let bytes = password.as_bytes();
        let len = bytes.len();

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
    use crate::audio;
    use std::net::IpAddr;

    /// Host-side fake device handle for unit tests / desktop builds.
    #[derive(Debug)]
    pub struct DeviceInner {
        addr: IpAddr,
        audio_device: Option<AudioDevice>,
    }

    pub type AudioDevice = audio::AudioDevice;

    #[derive(Debug, Default)]
    pub struct UiDevice;

    pub fn init_device(config: WifiConfig) -> Result<DeviceInner, HardwareError> {
        log::debug!("simulated Atom Echo init: ssid='{}'", config.ssid);

        let addr = audio::host::host_ip_addr();
        let audio_device = Some(audio::init_default_audio()?);

        Ok(DeviceInner { addr, audio_device })
    }

    impl DeviceInner {
        pub fn get_audio_device(&mut self) -> Result<AudioDevice, HardwareError> {
            self.audio_device
                .take()
                .ok_or(HardwareError::Other("AudioDevice already taken"))
        }

        pub fn get_ui_device(&mut self) -> Result<UiDevice, HardwareError> {
            Ok(UiDevice)
        }

        pub fn get_ip_addr(&self) -> IpAddr {
            self.addr
        }
    }

    impl UiDevice {
        pub fn read_button_state(&self) -> ButtonState {
            ButtonState::Released
        }

        pub fn set_led_state(&mut self, state: LedState) -> Result<(), HardwareError> {
            log::debug!("simulated LED state: {:?}", state);
            Ok(())
        }
    }

    pub fn random_u32() -> u32 {
        rand::random::<u32>()
    }
}

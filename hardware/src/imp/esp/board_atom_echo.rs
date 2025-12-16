use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use esp_idf_hal::delay::TickType;
use esp_idf_svc::wifi::EspWifi;

use super::wifi::{
    map_wifi_err, init_wifi_enterprise, init_wifi_personal
};

use crate::audio::{AudioCaps, AudioDevice, AudioDeviceImpl};
use crate::{ButtonState, LedState};

use super::*;
use esp_idf_hal::gpio::{AnyIOPin, AnyInputPin, InputPin};
use esp_idf_hal::gpio::{Input, PinDriver};
use esp_idf_hal::i2s::{config::StdConfig, I2sTx, I2sDriver};
use esp_idf_hal::peripherals::Peripherals;
use esp_idf_hal::rmt::{config::TransmitConfig, FixedLengthSignal, PinState, Pulse, TxRmtDriver};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::nvs::EspDefaultNvsPartition;

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

pub struct AtomEchoAudioDevice {
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

    let audio_impl = AtomEchoAudioDevice { speaker };

    Ok(DeviceInner {
        wifi,
        addr: ip,
        ui_device: Some(UiDevice { led, button }),
        audio_device: Some(
            crate::audio::AudioDevice::new(
                Box::new(audio_impl)
            )
        ),
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

impl AudioDeviceImpl for AtomEchoAudioDevice {
    fn caps(&self) -> AudioCaps {
        AudioCaps { full_duplex: false }
    }

    fn tx_disable(&mut self) -> Result<(), HardwareError> {
        self.speaker.tx_disable().map_err(map_audio_err)
    }

    fn tx_enable(&mut self) -> Result<(), HardwareError> {
        self.speaker.tx_enable().map_err(map_audio_err)
    }

    fn preload_data(&mut self, data: &[u8]) -> Result<usize, HardwareError> {
        self.speaker.preload_data(data).map_err(map_audio_err)
    }

    fn write(&mut self, data: &[u8], timeout: Duration) -> Result<usize, HardwareError> {
        let tick_timeout = TickType::from(timeout);
        self.speaker.write(data, tick_timeout.into()).map_err(map_audio_err)
    }

    fn read(&mut self, pcm: &mut [u8], timeout: Duration) -> Result<usize, HardwareError> {
        Err(HardwareError::Audio("E_NOTIMPL"))
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
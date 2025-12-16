use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use esp_idf_hal::delay::TickType;
use esp_idf_hal::i2c::{self, I2cDriver};
use esp_idf_svc::wifi::EspWifi;

use super::wifi::{
    map_wifi_err, init_wifi_enterprise, init_wifi_personal
};

use crate::audio::{AudioCaps, AudioDevice, AudioDeviceImpl, SampleInfo};
use crate::{ButtonState, LedState};

use super::*;
use esp_idf_hal::gpio::{AnyIOPin, AnyInputPin, Gpio46, InputPin, Output};
use esp_idf_hal::gpio::{Input, PinDriver};
use esp_idf_hal::i2s::{config::StdConfig, I2sTx, I2sDriver};
use esp_idf_hal::peripherals::Peripherals;
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

pub struct MyAudioDevice {
    speaker: I2sDriver<'static, I2sTx>,
    pa_en: PinDriver<'static, Gpio46, Output>,
    /* mic: I2sDriver<'static, I2sRx>, */
}

pub struct UiDevice {
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

    let bclk = pins.gpio41;
    let _din = pins.gpio42;
    let dout = pins.gpio40;
    let ws = pins.gpio33;
    let mclk = pins.gpio16;

    // 16-bit PCM at 8 kHz, Philips standard.
    let speaker_config = StdConfig::philips(8_000, esp_idf_hal::i2s::config::DataBitWidth::Bits16);

    let speaker = I2sDriver::<I2sTx>::new_std_tx(
        peripherals.i2s0,
        &speaker_config,
        bclk,
        dout,
        Some(mclk),
        ws
    )
    .map_err(map_audio_err)?;

    log::info!("I2S initialized");

    /* Config ES8311 */
    let i2c_config = i2c::config::Config {
        baudrate: esp_idf_hal::units::Hertz(100000),
        sda_pullup_enabled: true,
        scl_pullup_enabled: true,
        ..Default::default()
    };
    let mut i2c = I2cDriver::new(
        peripherals.i2c0,
        pins.gpio15,
        pins.gpio14,
        &i2c_config,
    )
    .map_err(map_gpio_err)?;

    log::info!("I2C initialized");

    let master_mode = true;
    let use_mclk = true;
    let invert_mclk = false;
    let invert_sclk = false;
    let no_dac_ref = false;

    es8311_open_like_esp_codec_dev(
        &mut i2c,
        master_mode,
        use_mclk,
        invert_mclk,
        invert_sclk,
        no_dac_ref,
    )?;

    log::info!("ES8311 opened");

    es8311_set_fs_bringup(&mut i2c, SampleInfo {
        sample_rate: 8_000,
        bits_per_sample: 16,
    })?;

    log::info!("ES8311 initialized!");

    es8311_start_dac(&mut i2c, master_mode, use_mclk, invert_mclk)?;
    es8311_set_vol_raw(&mut i2c, 0xC0)?;
    es8311_set_mute(&mut i2c, false)?;

    log::info!("ES8311 started + unmuted!");

    // Power on PA
    let mut pa_en = PinDriver::output(pins.gpio46).map_err(map_gpio_err)?;
    pa_en.set_high().map_err(map_gpio_err)?;

    log::info!("PA Enabled!");

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
    let button_pin = pins.gpio10;
    let button = PinDriver::input(button_pin.downgrade_input())
        .map_err(map_gpio_err)?;

    let audio_impl = MyAudioDevice { speaker, pa_en };

    Ok(DeviceInner {
        wifi,
        addr: ip,
        ui_device: Some(UiDevice { button }),
        audio_device: Some(
            crate::audio::AudioDevice::new(
                Box::new(audio_impl)
            )
        ),
    })
}

const ES8311_ADDR: u8 = 0x18;
fn es8311_write_reg(i2c: &mut I2cDriver<'static>, reg: u8, val: u8) -> Result<(), HardwareError> {
    i2c.write(ES8311_ADDR, &[reg, val], 1000).map_err(map_gpio_err)
}

fn es8311_read_reg(i2c: &mut I2cDriver<'static>, reg: u8) -> Result<u8, HardwareError> {
    let mut out = [0u8];
    // Typical pattern: write register index, then read data
    i2c.write_read(ES8311_ADDR, &[reg], &mut out, 1000).map_err(map_gpio_err)?;
    Ok(out[0])
}

fn es8311_open_like_esp_codec_dev(
    i2c: &mut I2cDriver<'static>,
    master_mode: bool,
    use_mclk: bool,
    invert_mclk: bool,
    invert_sclk: bool,
    no_dac_ref: bool,
) -> Result<(), HardwareError> {
    // Enhance ES8311 I2C noise immunity (GPIO_REG44 / 0x44)
    es8311_write_reg(i2c, 0x44, 0x08)?;
    es8311_write_reg(i2c, 0x44, 0x08)?; // intentional double-write per esp_codec_dev

    // The “bulk init” block
    es8311_write_reg(i2c, 0x01, 0x30)?; // CLK_MANAGER_REG01
    es8311_write_reg(i2c, 0x02, 0x00)?; // CLK_MANAGER_REG02
    es8311_write_reg(i2c, 0x03, 0x10)?; // CLK_MANAGER_REG03
    es8311_write_reg(i2c, 0x16, 0x24)?; // ADC_REG16
    es8311_write_reg(i2c, 0x04, 0x10)?; // CLK_MANAGER_REG04
    es8311_write_reg(i2c, 0x05, 0x00)?; // CLK_MANAGER_REG05
    es8311_write_reg(i2c, 0x0B, 0x00)?; // SYSTEM_REG0B
    es8311_write_reg(i2c, 0x0C, 0x00)?; // SYSTEM_REG0C
    es8311_write_reg(i2c, 0x10, 0x1F)?; // SYSTEM_REG10
    es8311_write_reg(i2c, 0x11, 0x7F)?; // SYSTEM_REG11
    es8311_write_reg(i2c, 0x00, 0x80)?; // RESET_REG00

    // RESET_REG00: set master/slave bit
    let mut reg0 = es8311_read_reg(i2c, 0x00)?;
    if master_mode { reg0 |= 0x40; } else { reg0 &= 0xBF; }
    es8311_write_reg(i2c, 0x00, reg0)?;

    // CLK_MANAGER_REG01: use_mclk + invert_mclk
    let mut reg1 = 0x3F;
    if use_mclk { reg1 &= 0x7F; } else { reg1 |= 0x80; }
    if invert_mclk { reg1 |= 0x40; } else { reg1 &= !0x40; }
    es8311_write_reg(i2c, 0x01, reg1)?;

    // CLK_MANAGER_REG06: invert_sclk bit (read-modify-write)
    let mut reg6 = es8311_read_reg(i2c, 0x06)?;
    if invert_sclk { reg6 |= 0x20; } else { reg6 &= !0x20; }
    es8311_write_reg(i2c, 0x06, reg6)?;

    es8311_write_reg(i2c, 0x13, 0x10)?; // SYSTEM_REG13
    es8311_write_reg(i2c, 0x1B, 0x0A)?; // ADC_REG1B
    es8311_write_reg(i2c, 0x1C, 0x6A)?; // ADC_REG1C

    // DAC ref selection via GPIO_REG44
    if !no_dac_ref {
        es8311_write_reg(i2c, 0x44, 0x58)?;
    } else {
        es8311_write_reg(i2c, 0x44, 0x08)?;
    }

    Ok(())
}

fn es8311_set_bits_per_sample(i2c: &mut I2cDriver<'static>, bits: u8) -> Result<(), HardwareError> {
    let mut dac_iface = es8311_read_reg(i2c, 0x09)?; // SDP IN
    let mut adc_iface = es8311_read_reg(i2c, 0x0A)?; // SDP OUT

    match bits {
        24 => {
            dac_iface &= !0x1C;
            adc_iface &= !0x1C;
        }
        32 => {
            dac_iface |= 0x10;
            adc_iface |= 0x10;
        }
        16 | _ => {
            dac_iface |= 0x0C;
            adc_iface |= 0x0C;
        }
    }

    es8311_write_reg(i2c, 0x09, dac_iface)?;
    es8311_write_reg(i2c, 0x0A, adc_iface)?;
    Ok(())
}

fn es8311_config_i2s_normal(i2c: &mut I2cDriver<'static>) -> Result<(), HardwareError> {
    let mut dac_iface = es8311_read_reg(i2c, 0x09)?;
    let mut adc_iface = es8311_read_reg(i2c, 0x0A)?;
    dac_iface &= 0xFC;
    adc_iface &= 0xFC;
    es8311_write_reg(i2c, 0x09, dac_iface)?;
    es8311_write_reg(i2c, 0x0A, adc_iface)?;
    Ok(())
}

// Minimal “config_sample” for 8kHz when MCLK = Fs*256 = 2.048MHz.
// This corresponds to the coeff_div entry {2048000, 8000, pre_div=0x01, pre_multi=0x01, adc_div=0x01, dac_div=0x01, fs_mode=0, lrck_h=0x00, lrck_l=0xff, bclk_div=0x04, adc_osr=0x10, dac_osr=0x20}
fn es8311_config_sample_8k_mclk256(i2c: &mut I2cDriver<'static>) -> Result<(), HardwareError> {
    // CLK_MANAGER_REG02: pre_div + pre_multi
    // regv = (regv & 0x07) | ((pre_div-1)<<5) | (datmp<<3)
    // pre_div=1 => (0<<5); pre_multi=1 => datmp=0
    let mut reg2 = es8311_read_reg(i2c, 0x02)?;
    reg2 &= 0x07;
    reg2 |= 0 << 5;
    reg2 |= 0 << 3;
    es8311_write_reg(i2c, 0x02, reg2)?;

    // CLK_MANAGER_REG05: adc_div + dac_div
    // regv = ((adc_div-1)<<4) | ((dac_div-1)<<0) ; both 1 => 0
    es8311_write_reg(i2c, 0x05, 0x00)?;

    // CLK_MANAGER_REG03: fs_mode + adc_osr
    let mut reg3 = es8311_read_reg(i2c, 0x03)?;
    reg3 &= 0x80;
    reg3 |= (0 << 6) | 0x10;
    es8311_write_reg(i2c, 0x03, reg3)?;

    // CLK_MANAGER_REG04: dac_osr
    let mut reg4 = es8311_read_reg(i2c, 0x04)?;
    reg4 &= 0x80;
    reg4 |= 0x20;
    es8311_write_reg(i2c, 0x04, reg4)?;

    // CLK_MANAGER_REG07 / REG08: LRCK dividers
    let mut reg7 = es8311_read_reg(i2c, 0x07)?;
    reg7 &= 0xC0;
    reg7 |= 0x00;
    es8311_write_reg(i2c, 0x07, reg7)?;
    es8311_write_reg(i2c, 0x08, 0xFF)?;

    // CLK_MANAGER_REG06: BCLK divider (bclk_div=4 => write (4-1)=3 into low bits when <19)
    let mut reg6 = es8311_read_reg(i2c, 0x06)?;
    reg6 &= 0xE0;
    reg6 |= 0x07;
    es8311_write_reg(i2c, 0x06, reg6)?;

    Ok(())
}

fn es8311_set_fs_bringup(i2c: &mut I2cDriver<'static>, fs: SampleInfo) -> Result<(), HardwareError> {
    es8311_set_bits_per_sample(i2c, fs.bits_per_sample)?;
    es8311_config_i2s_normal(i2c)?;

    // For your very first bring-up: only support 8kHz with MCLK=256*Fs.
    if fs.sample_rate == 8_000 {
        es8311_config_sample_8k_mclk256(i2c)?;
        Ok(())
    } else {
        Err(HardwareError::Audio("unsupported sample_rate in bring-up"))
    }
}

fn es8311_start_dac(i2c: &mut I2cDriver<'static>, master_mode: bool, use_mclk: bool, invert_mclk: bool) -> Result<(), HardwareError> {
    // Mirrors the important parts of es8311_start() for DAC-only use.

    // RESET_REG00: reset + master/slave
    let mut reg0 = 0x80;
    if master_mode { reg0 |= 0x40; }
    es8311_write_reg(i2c, 0x00, reg0)?;

    // CLK_MANAGER_REG01: use_mclk + invert_mclk
    let mut reg1 = 0x3F;
    if use_mclk { reg1 &= 0x7F; } else { reg1 |= 0x80; }
    if invert_mclk { reg1 |= 0x40; } else { reg1 &= !0x40; }
    es8311_write_reg(i2c, 0x01, reg1)?;

    // Clear BITS(6) in SDP IN/OUT (enables I2S input/output paths)
    let mut sdpin = es8311_read_reg(i2c, 0x09)?;
    let mut sdpout = es8311_read_reg(i2c, 0x0A)?;
    sdpin &= 0xBF;
    sdpout &= 0xBF;
    // For DAC path, es8311_start() clears bit6 for DAC
    sdpin &= !0x40;
    es8311_write_reg(i2c, 0x09, sdpin)?;
    es8311_write_reg(i2c, 0x0A, sdpout)?;

    // Power sequencing for DAC path (matches es8311_start())
    es8311_write_reg(i2c, 0x17, 0xBF)?; // ADC_REG17 (harmless even if you don't use ADC)
    es8311_write_reg(i2c, 0x0E, 0x02)?; // SYSTEM_REG0E
    es8311_write_reg(i2c, 0x12, 0x00)?; // SYSTEM_REG12 (DAC on)
    es8311_write_reg(i2c, 0x14, 0x1A)?; // SYSTEM_REG14
    es8311_write_reg(i2c, 0x0D, 0x01)?; // SYSTEM_REG0D
    es8311_write_reg(i2c, 0x15, 0x40)?; // ADC_REG15 (again harmless)
    es8311_write_reg(i2c, 0x37, 0x08)?; // DAC_REG37
    es8311_write_reg(i2c, 0x45, 0x00)?; // GP_REG45

    Ok(())
}

fn es8311_set_mute(i2c: &mut I2cDriver<'static>, mute: bool) -> Result<(), HardwareError> {
    let mut reg31 = es8311_read_reg(i2c, 0x31)?; // DAC_REG31
    reg31 &= 0x9F;
    if mute {
        reg31 |= 0x60;
    }
    es8311_write_reg(i2c, 0x31, reg31)
}

// “Just make it loud enough”: write the raw register.
fn es8311_set_vol_raw(i2c: &mut I2cDriver<'static>, vol: u8) -> Result<(), HardwareError> {
    es8311_write_reg(i2c, 0x32, vol) // DAC_REG32
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

impl AudioDeviceImpl for MyAudioDevice {
    fn caps(&self) -> AudioCaps {
        AudioCaps { full_duplex: true }
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
    pub fn set_led_state(&mut self, _state: LedState) -> Result<(), HardwareError> {
        Ok(())
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

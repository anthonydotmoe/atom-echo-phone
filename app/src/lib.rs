use std::net::UdpSocket;
use std::sync::mpsc::channel;
use std::thread;
use std::time::Duration;

use atom_echo_hw::{AudioDevice, Device, WifiConfig};
use log::info;
use thiserror::Error;

mod messages;
mod settings;
mod tasks;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("hardware error: {0}")]
    Hardware(String),
    #[error("sip error: {0}")]
    Sip(String),
}

pub fn run() -> Result<(), AppError> {
    info!("starting Atom Echo phone runtime");

    let wifi_config = WifiConfig::new(
        settings::SETTINGS.wifi_ssid,
        settings::SETTINGS.wifi_password,
        settings::SETTINGS.wifi_username,
    )
        .map_err(|err| AppError::Hardware(format!("{err:?}")))?;

    let mut device = Device::init(wifi_config)
        .map_err(|err| AppError::Hardware(format!("{err:?}")))?;

    // Split device
    let ui_device = device.get_ui_device().unwrap();
    let audio_device = device.get_audio_device().unwrap();

    //audio_test(audio_device);

    let addr = device.get_ip_addr();

    let rtp_socket = UdpSocket::bind((addr, 0))
        .map_err(|err| AppError::Sip(format!("{err:?}")))?;
    let _ = rtp_socket.set_nonblocking(true);
    let local_rtp_port = rtp_socket
        .local_addr()
        .map(|addr| addr.port())
        .unwrap_or(10_000);

    log::info!("rtp_socket.local_addr(): {:?}", rtp_socket.local_addr());
    
    // Create channels
    let (sip_tx, sip_rx) = channel::<messages::SipCommand>();
    let (audio_tx, audio_rx) = channel::<messages::AudioCommand>();
    let (rtp_tx_tx, _rtp_tx_rx) = channel::<messages::RtpTxCommand>();
    let (rtp_rx_tx, rtp_rx_rx) = channel::<messages::RtpRxCommand>();
    let (ui_tx, ui_rx) = channel::<messages::UiCommand>();
    let (media_in_tx, media_in_rx) = channel::<messages::MediaIn>();
    let (_media_out_tx, _media_out_rx) = channel::<messages::MediaOut>();

    let _ui_handle = tasks::ui::spawn_ui_task(ui_device, ui_rx, sip_tx);
    let _rtp_rx_handle = tasks::rtp_rx::spawn_rtp_rx_task(rtp_socket, rtp_rx_rx, media_in_tx);
    let _sip_handle = tasks::sip::spawn_sip_task(&settings::SETTINGS, addr, local_rtp_port, sip_rx, ui_tx, audio_tx, rtp_tx_tx, rtp_rx_tx);
    let _audio_handle = tasks::audio::spawn_audio_task(audio_rx, audio_device, media_in_rx);

    /*
    let _rtp_tx_handle = tasks::rtp_tx::spawn_rtp_tx_task(rtp_tx_rx);
    */

    loop {
        thread::sleep(Duration::from_secs(1));
    }
}

use std::f32::consts::PI;

fn audio_test(mut audio: AudioDevice) -> ! {
    audio.tx_enable().unwrap();

    let mut frame_count = 100;

    const SR: u32 = 48_000;
    const FRAME: usize = 160; // 20 ms
    const F_TONE: f32 = 447.0;

    let mut phase: f32 = 0.0;
    let step = 2.0 * PI * F_TONE / SR as f32;

    loop {
        let mut frame_mono = [0i16; FRAME];
        for s in &mut frame_mono {
            *s = (phase.sin() * 8000.0) as i16;
            phase += step;
            if phase > 2.0 * PI {
                phase -= 2.0 * PI;
            }
        }

        // Duplicate to stereo
        let mut stereo = [0i16; FRAME * 2];
        for (i, &s) in frame_mono.iter().enumerate() {
            let idx = i * 2;
            stereo[idx] = s;
            stereo[idx + 1] = s;
        }

        let bytes: &[u8] = bytemuck::cast_slice(&stereo);
        let _ = audio.write(bytes, Duration::from_millis(50)).unwrap();
        //frame_count -= 1;

        if frame_count == 0 {
            break;
        }
    }

    audio.tx_disable();
    log::debug!("DONE");

    loop {}
}


use std::sync::mpsc::channel;
use std::thread;
use std::time::Duration;

use atom_echo_hw::{Device, WifiConfig};
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

    let wifi_config = WifiConfig::new(settings::SETTINGS.wifi_ssid, settings::SETTINGS.wifi_password)
        .map_err(|err| AppError::Hardware(format!("{err:?}")))?;
    let device = Device::init(wifi_config).map_err(|err| AppError::Hardware(format!("{err:?}")))?;

    // Create channels
    let (sip_tx, sip_rx) = channel::<messages::SipCommand>();
    let (audio_tx, audio_rx) = channel::<messages::AudioCommand>();
    let (rtp_tx_tx, rtp_tx_rx) = channel::<messages::RtpTxCommand>();
    let (rtp_rx_tx, rtp_rx_rx) = channel::<messages::RtpRxCommand>();
    let (ui_tx, ui_rx) = channel::<messages::UiCommand>();
    let (media_in_tx, media_in_rx) = channel::<messages::MediaIn>();
    let (media_out_tx, media_out_rx) = channel::<messages::MediaOut>();

    /*
    TODO: Just mapping out who needs what

    UI -> SIP: SipCommand
    SIP -> Audio: AudioCommand
    SIP -> RTP TX: RtpTxCommand
    SIP -> RTP RX: RtpRxCommand
    SIP -> UI: UiCommand
    Audio -> RTP TX (mic): MediaOut
    RTP RX -> Audio (speaker): MediaIn
    */

    //let _ui_handle = tasks::ui::spawn_ui_task(device, ui_rx, sip_tx);
    let _sip_handle = tasks::sip::spawn_sip_task(&settings::SETTINGS, sip_rx, audio_tx, rtp_tx_tx, rtp_rx_tx);

    /*
    let _rtp_tx_handle = tasks::rtp_tx::spawn_rtp_tx_task(rtp_tx_rx);
    let _rtp_rx_handle = tasks::rtp_rx::spawn_rtp_rx_task(rtp_rx_rx);
    let _audio_handle = tasks::audio::spawn_audio_task(device, audio_rx, media_in_rx, media_out_tx);
    */

    loop {
        thread::sleep(Duration::from_secs(1));
    }
}

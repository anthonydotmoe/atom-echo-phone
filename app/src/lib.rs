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

    let (sip_tx, sip_rx) = channel::<messages::SipCommand>();
    let (audio_tx, audio_rx) = channel::<messages::AudioCommand>();

    let _hw_handle = tasks::hardware::spawn_hardware_task(device, sip_tx, audio_rx);
    let _sip_handle = tasks::sip::spawn_sip_task(&settings::SETTINGS, sip_rx, audio_tx);

    loop {
        thread::sleep(Duration::from_secs(1));
    }
}

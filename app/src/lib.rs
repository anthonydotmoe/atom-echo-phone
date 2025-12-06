use atom_echo_hw::{init_audio, init_wifi, LedState, WifiConfig};
use log::info;
use rtp_audio::JitterBuffer;
use sdp::SessionDescription;
use sip_core::SipStack;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("hardware error: {0}")]
    Hardware(String),
    #[error("sip error: {0}")]
    Sip(String),
}

pub fn run() -> Result<(), AppError> {
    info!("starting application skeleton");

    let wifi_config = WifiConfig {
        ssid: "ssid".into(),
        password: "password".into(),
    };
    init_wifi(wifi_config).map_err(|err| AppError::Hardware(err.to_string()))?;
    let mut audio = init_audio().map_err(|err| AppError::Hardware(err.to_string()))?;
    let mut sip = SipStack::new();
    sip.register().map_err(|err| AppError::Sip(err.to_string()))?;

    let _local_sdp = SessionDescription::offer();
    let mut jitter = JitterBuffer::new(4);
    jitter.push_frame(vec![0; 160]);
    let _ = jitter.pop_frame();

    let _ = atom_echo_hw::set_led_state(&mut audio, LedState {
        red: 0,
        green: 255,
        blue: 0,
    });

    info!("application skeleton initialized");
    Ok(())
}

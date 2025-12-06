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
    let mut sip = SipStack::default();
    let _register = sip
        .register("sip:user@example.com", "sip:user@example.com")
        .map_err(|err| AppError::Sip(err.to_string()))?;
    sip.on_register_response(200);

    let _local_sdp = SessionDescription::offer("atom-echo", "0.0.0.0", 10_000)
        .map_err(|err| AppError::Sip(format!("sdp render: {err}")))?;
    let mut jitter: JitterBuffer<4, 160> = JitterBuffer::new();
    jitter.push_frame(0, &[0; 160]);
    let _ = jitter.pop_frame();

    let _ = atom_echo_hw::set_led_state(&mut audio, LedState::Color {
        red: 0,
        green: 255,
        blue: 0,
    });

    info!("application skeleton initialized");
    Ok(())
}

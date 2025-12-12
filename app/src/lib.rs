use std::net::UdpSocket;
use std::sync::mpsc::channel;
use std::thread;
use std::time::Duration;

use atom_echo_hw::{Device, WifiConfig};
use log::info;
use thiserror::Error;

use crate::tasks::{
    task::{AppTask, start_all},
    audio::AudioTask,
    rtp_rx::RtpRxTask,
    sip::SipTask,
    ui::UiTask,
};

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

    let ui_task = Box::new(UiTask::new(
        ui_device,
        ui_rx,
        sip_tx
    ));

    let rtp_rx_task = Box::new(RtpRxTask::new(
        rtp_socket,
        rtp_rx_rx,
        media_in_tx
    ));

    let sip_task = Box::new(SipTask::new(
        &settings::SETTINGS,
        addr,
        local_rtp_port,
        sip_rx,
        ui_tx,
        audio_tx,
        rtp_tx_tx,
        rtp_rx_tx,
    ));

    let audio_task = Box::new(AudioTask::new(
        audio_rx,
        audio_device,
        media_in_rx,
    ));

    let tasks: Vec<Box<dyn AppTask>> = vec![
        audio_task,
        ui_task,
        rtp_rx_task,
        sip_task,
    ];

    start_all(tasks);

    let mut stats_buf = [0u8; 512];

    loop {
        thread::sleep(Duration::from_secs(1));
        
        match freertos_runtime_stats(&mut stats_buf) {
            Ok(s) => log::info!("runtime stats:\r\n{s}"),
            Err(e) => log::warn!("run-time stats error: {:?}", e),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunTimeStatsError {
    NotNulTerminated,
    InvalidUtf8,
}

extern "C" {
    fn vTaskGetRunTimeStats(pcWriteBuffer: *mut core::ffi::c_char);
}

fn freertos_runtime_stats<'a>(buf: &'a mut [u8]) -> Result<&'a str, RunTimeStatsError> {
    buf.fill(0);

    unsafe {
        vTaskGetRunTimeStats(buf.as_mut_ptr() as *mut core::ffi::c_char);
    }

    let nul = buf.iter().position(|&b| b == 0)
        .ok_or(RunTimeStatsError::NotNulTerminated)?;
    str::from_utf8(&buf[..nul]).map_err(|_| RunTimeStatsError::InvalidUtf8)
}

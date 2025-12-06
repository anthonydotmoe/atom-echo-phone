use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};

use atom_echo_hw::Device;
use log::info;

use crate::messages::{
    AudioCommandReceiver, AudioControlReceiver, AudioControlSender, SipCommandSender,
    UiCommandReceiver, UiCommandSender,
};
use crate::tasks::{audio, ui, wifi};

pub struct HardwareHandles {
    pub audio: std::thread::JoinHandle<()>,
    pub ui: std::thread::JoinHandle<()>,
    pub wifi: std::thread::JoinHandle<()>,
}

pub fn spawn_hardware_tasks(
    device: Device,
    sip_tx: SipCommandSender,
    audio_rx: AudioCommandReceiver,
) -> HardwareHandles {
    let device = Arc::new(Mutex::new(device));

    let (ui_tx, ui_rx): (UiCommandSender, UiCommandReceiver) = channel();
    let (audio_ctrl_tx, audio_ctrl_rx): (AudioControlSender, AudioControlReceiver) = channel();

    info!("spawning audio task");
    let audio_handle = audio::spawn_audio_task(
        Arc::clone(&device),
        sip_tx.clone(),
        audio_rx,
        audio_ctrl_rx,
        ui_tx.clone(),
    );

    info!("spawning UI task");
    let ui_handle =
        ui::spawn_ui_task(Arc::clone(&device), sip_tx, ui_rx, audio_ctrl_tx);

    info!("spawning Wi-Fi task");
    let wifi_handle = wifi::spawn_wifi_task();

    HardwareHandles {
        audio: audio_handle,
        ui: ui_handle,
        wifi: wifi_handle,
    }
}

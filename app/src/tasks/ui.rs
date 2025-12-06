use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use atom_echo_hw::{ButtonState, Device, LedState};
use log::{debug, warn};

use crate::messages::{
    SipCommand, SipCommandSender, UiCommand, UiCommandReceiver,
};

pub fn spawn_ui_task(
    device: Arc<Mutex<Device>>,
    ui_rx: UiCommandReceiver,
    sip_tx: SipCommandSender,
) -> thread::JoinHandle<()> {
    thread::spawn(move || ui_loop(device, ui_rx, sip_tx))
}

fn ui_loop(
    device: Arc<Mutex<Device>>,
    ui_rx: UiCommandReceiver,
    sip_tx: SipCommandSender,
    audio_ctrl_tx: AudioControlSender,
) {
    let mut last_button = read_button_state(&device);

    loop {
        // Handle UI commands (LED updates, dialog changes).
        loop {
            match ui_rx.try_recv() {
                Ok(cmd) => match cmd {
                    UiCommand::DialogStateChanged(state) => {
                        let led = match state {
                            sip_core::DialogState::Idle => LedState::Color {
                                red: 0,
                                green: 32,
                                blue: 0,
                            },
                            sip_core::DialogState::Inviting | sip_core::DialogState::Ringing => {
                                LedState::Color {
                                    red: 32,
                                    green: 32,
                                    blue: 0,
                                }
                            }
                            sip_core::DialogState::Established => LedState::Color {
                                red: 0,
                                green: 0,
                                blue: 48,
                            },
                            sip_core::DialogState::Terminated => LedState::Off,
                        };
                        debug!("ui_task: LED set for state {:?}", state);
                        set_led(&device, led);
                    }
                    UiCommand::SetLed(state) => {
                        debug!("ui_task: LED override {:?}", state);
                        set_led(&device, state);
                    }
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    warn!("ui_task: UI channel closed; exiting");
                    return;
                }
            }
        }

        // Poll button for edge detection.
        let btn = read_button_state(&device);
        if btn != last_button {
            last_button = btn;
            let event = match btn {
                ButtonState::Pressed => SipCommand::PttPressed,
                ButtonState::Released => SipCommand::PttReleased,
            };
            debug!("ui_task: button {:?} -> {:?}", btn, event);
            let _ = sip_tx.send(event);
            let _ = audio_ctrl_tx.send(AudioControl::ButtonState(btn));
        }

        thread::sleep(Duration::from_millis(10));
    }
}

fn read_button_state(device: &Arc<Mutex<Device>>) -> ButtonState {
    device
        .lock()
        .map(|d| d.read_button_state())
        .unwrap_or(ButtonState::Released)
}

fn set_led(device: &Arc<Mutex<Device>>, state: LedState) {
    if let Ok(mut dev) = device.lock() {
        let _ = dev.set_led_state(state);
    }
}

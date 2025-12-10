use std::sync::mpsc::TryRecvError;
use std::thread;
use std::time::{Duration, Instant};

use atom_echo_hw::{UiDevice, LedState};

use crate::messages::{
    SipCommandSender, UiCommand, UiCommandReceiver, PhoneState
};

pub fn spawn_ui_task(
    device: UiDevice,
    ui_rx: UiCommandReceiver,
    sip_tx: SipCommandSender,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("ui".into())
        .spawn(move || {
            let mut task = Box::new(
                UiTask::new(device, ui_rx, sip_tx)
            );
            task.run();
        })
        .expect("failed to spawn UI task")
}

struct UiTask {
    ui_device: UiDevice,
    ui_rx: UiCommandReceiver,
    _sip_tx: SipCommandSender,
}

impl UiTask {
    fn new(
        ui_device: UiDevice,
        ui_rx: UiCommandReceiver,
        _sip_tx: SipCommandSender,
    ) -> Self {
        Self {
            ui_device,
            ui_rx,
            _sip_tx,
        }
    }

    fn run(&mut self) {
        log::info!("UI task started");

        loop {
            let _now = Instant::now();

            if !self.poll_commands() {
                log::info!("UI task exiting: command channel closed");
                break;
            }

            thread::sleep(Duration::from_millis(25));
        }
    }

    fn handle_dialog_state_changed(&mut self, state: PhoneState) {
        let led = match state {
            PhoneState::Idle => LedState::Color {
                red: 0,
                green: 127,
                blue: 0,
            },
            PhoneState::Ringing => {
                LedState::Color {
                    red: 255,
                    green: 127,
                    blue: 0,
                }
            }
            PhoneState::Established => LedState::Color {
                red: 0,
                green: 0,
                blue: 64,
            },
        };
        log::debug!("ui_task: LED set for state {:?}", state);
        let _ = self.ui_device.set_led_state(led);
    }

    fn poll_commands(&mut self) -> bool {
        loop {
            match self.ui_rx.try_recv() {
                Ok(cmd) => self.handle_command(cmd),
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => {
                    log::warn!("UI command channel closed");
                    return false;
                }
            }
        }
    }

    fn handle_command(&mut self, cmd: UiCommand) {
        match cmd {
            UiCommand::SetLed(_l) => {
                //self.handle_set_led(l);
            }
            UiCommand::DialogStateChanged(p) => {
                self.handle_dialog_state_changed(p);
            }
        }
    }

}

/*
fn ui_loop(
    device: Arc<Mutex<Device>>,
    ui_rx: UiCommandReceiver,
    sip_tx: SipCommandSender,
) {
    let mut last_button = read_button_state(&device);

    loop {
        // Handle UI commands (LED updates, dialog changes).
        loop {
            match ui_rx.try_recv() {
                Ok(cmd) => match cmd {
                    UiCommand::DialogStateChanged(state) => {
                        handle_dialog_state_changed(state);
                    }
                    UiCommand::SetLed(state) => {
                        log::debug!("ui_task: LED override {:?}", state);
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
            log::debug!("ui_task: button {:?} -> {:?}", btn, event);
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
*/

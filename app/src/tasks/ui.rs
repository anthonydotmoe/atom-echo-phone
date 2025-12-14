use std::sync::mpsc::TryRecvError;
use std::thread;
use std::time::{Duration, Instant};

use hardware::{ButtonState, LedState, UiDevice};

use crate::messages::{
    PhoneState, SipCommand, SipCommandSender, UiCommand, UiCommandReceiver
};

use crate::tasks::task::{AppTask, TaskMeta};

pub struct UiTask {
    ui_device: UiDevice,
    ui_rx: UiCommandReceiver,
    sip_tx: SipCommandSender,
    last_button_state: ButtonState,
    auto_answer_deadline: Option<Instant>,
}

impl AppTask for UiTask {
    fn into_runner(mut self: Box<Self>) -> Box<dyn FnOnce() + Send + 'static> {

        Box::new(move || {
            self.run();
        })
    }

    fn meta(&self) -> TaskMeta {
        TaskMeta {
            name: "ui",
            stack_bytes: Some(16384),
        }
    }
}

impl UiTask {
    pub fn new(
        ui_device: UiDevice,
        ui_rx: UiCommandReceiver,
        sip_tx: SipCommandSender,
    ) -> Self {
        let initial_state = ui_device.read_button_state();

        Self {
            ui_device,
            ui_rx,
            sip_tx,
            last_button_state: initial_state,
            auto_answer_deadline: None,
        }
    }

    fn run(&mut self) {
        log::info!("UI task started");

        loop {
            let now = Instant::now();

            if !self.poll_commands() {
                log::info!("UI task exiting: command channel closed");
                break;
            }

            self.poll_button();
            self.poll_auto_answer(now);

            thread::sleep(Duration::from_millis(40));
        }
    }

    fn handle_dialog_state_changed(&mut self, state: PhoneState) {
        let now = Instant::now();
        match state {
            PhoneState::Ringing => {
                // Only arm if not already armed
                if self.auto_answer_deadline.is_none() {
                    self.auto_answer_deadline = Some(now + Duration::from_secs(3));
                    log::info!("auto-answer armed for 3 seconds");
                }
            }
            _ => {
                // Any non-ringing state cancels the auto-answer
                if self.auto_answer_deadline.take().is_some() {
                    log::info!("auto-answer cancelled");
                }
            }
        }

        let led = match state {
            PhoneState::Idle => LedState::Color {
                red: 0,
                green: 255,
                blue: 0,
            },
            PhoneState::Ringing => {
                LedState::Color {
                    red: 255,
                    green: 255,
                    blue: 0,
                }
            }
            PhoneState::Established => LedState::Color {
                red: 0,
                green: 0,
                blue: 255,
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

    fn poll_button(&mut self) {
        let state = self.ui_device.read_button_state();

        // Edge: button was just pressed
        if matches!(self.last_button_state, ButtonState::Released)
            && matches!(state, ButtonState::Pressed)
        {
            log::info!("ui_task: button press detected");
            let _ = self.sip_tx.send(SipCommand::Button(crate::messages::ButtonEvent::ShortPress));
        }

        self.last_button_state = state;
    }

    fn poll_auto_answer(&mut self, now: Instant) {
        // Hack for broken button
        if let Some(deadline) = self.auto_answer_deadline {
            if now >= deadline {
                log::info!("auto-answer timeout reached, simulating button");

                // Send button pressed message after ring delay
                let _ = self
                    .sip_tx
                    .send(SipCommand::Button(crate::messages::ButtonEvent::ShortPress));

                self.auto_answer_deadline = None;
            }
        }
    }

}

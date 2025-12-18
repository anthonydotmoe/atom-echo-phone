use std::sync::mpsc::TryRecvError;
use std::thread;
use std::time::{Duration, Instant};

use hardware::{ButtonState, LedState, UiDevice};

use crate::messages::{
    ButtonEvent, PhoneState, SipCommand, SipCommandSender, UiCommand, UiCommandReceiver
};

use crate::tasks::task::{AppTask, TaskMeta};

pub struct UiTask {
    ui_device: UiDevice,
    ui_rx: UiCommandReceiver,
    sip_tx: SipCommandSender,
    phone_state: PhoneState,
    registered: bool,
    last_button_state: ButtonState,
    press_started_at: Option<Instant>,
    last_short_release_at: Option<Instant>,
    last_led_state: Option<LedState>,
    led_pattern: LedPattern,
    led_on: bool,
    next_blink_at: Instant,
    #[cfg(not(target_os = "espidf"))]
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
            stack_bytes: Some(4096),
        }
    }
}

impl UiTask {
    const POLL_INTERVAL: Duration = Duration::from_millis(40);

    // Gesture tuning knobs. These are intentionally conservative defaults and
    // should be tweaked for the desired UX.
    const SHORT_PRESS_MAX: Duration = Duration::from_millis(650);
    const DOUBLE_TAP_WINDOW: Duration = Duration::from_millis(400);

    pub fn new(
        ui_device: UiDevice,
        ui_rx: UiCommandReceiver,
        sip_tx: SipCommandSender,
    ) -> Self {
        let initial_state = ui_device.read_button_state();
        let now = Instant::now();
        let initial_pattern = LedPattern::for_state(PhoneState::Idle, false);

        Self {
            ui_device,
            ui_rx,
            sip_tx,
            phone_state: PhoneState::Idle,
            registered: false,
            last_button_state: initial_state,
            press_started_at: None,
            last_short_release_at: None,
            last_led_state: None,
            led_pattern: initial_pattern,
            led_on: true,
            next_blink_at: now
                + initial_pattern
                    .blink_period
                    .unwrap_or_else(|| Duration::from_secs(3600)),
            #[cfg(not(target_os = "espidf"))]
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

            self.poll_button(now);
            self.poll_auto_answer(now);
            self.update_led(now);

            thread::sleep(Self::POLL_INTERVAL);
        }
    }

    fn handle_dialog_state_changed(&mut self, state: PhoneState) {
        #[cfg(not(target_os = "espidf"))]
        {
            let now = Instant::now();
            match state {
                PhoneState::Ringing => {
                    // Host-only: auto-answer is useful for testing without real button hardware.
                    // Only arm if not already armed.
                    if self.auto_answer_deadline.is_none() {
                        self.auto_answer_deadline = Some(now + Duration::from_secs(3));
                        log::info!("auto-answer armed for 3 seconds");
                    }
                }
                _ => {
                    // Any non-ringing state cancels the auto-answer.
                    if self.auto_answer_deadline.take().is_some() {
                        log::info!("auto-answer cancelled");
                    }
                }
            }
        }

        self.phone_state = state;
        self.led_pattern = LedPattern::for_state(self.phone_state, self.registered);
        // Force immediate update on next tick.
        self.last_led_state = None;
        self.led_on = true;
        self.next_blink_at = Instant::now()
            + self
                .led_pattern
                .blink_period
                .unwrap_or_else(|| Duration::from_secs(3600));
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
            UiCommand::RegistrationStateChanged(registered) => {
                self.registered = registered;
                self.led_pattern = LedPattern::for_state(self.phone_state, self.registered);
                self.last_led_state = None;
                self.led_on = true;
                self.next_blink_at = Instant::now()
                    + self
                        .led_pattern
                        .blink_period
                        .unwrap_or_else(|| Duration::from_secs(3600));
            }
        }
    }

    fn poll_button(&mut self, now: Instant) {
        let state = self.ui_device.read_button_state();

        // Expire old tap state so a subsequent press doesn't get paired as a double-tap.
        if let Some(prev) = self.last_short_release_at {
            if now.duration_since(prev) > Self::DOUBLE_TAP_WINDOW {
                self.last_short_release_at = None;
            }
        }

        if state != self.last_button_state {
            // State changed
            log::info!("ui_task: button state changed");
            let _ = self.sip_tx.send(
                SipCommand::Button(ButtonEvent::StateChanged(state))
            );
        }

        // Edge: button was just pressed.
        if matches!(self.last_button_state, ButtonState::Released)
            && matches!(state, ButtonState::Pressed)
        {
            self.press_started_at = Some(now);
        }

        // Edge: button was just released.
        //
        // We treat a "ShortPress" as a completed click (press+release) with
        // bounded duration. Holding longer than SHORT_PRESS_MAX cancels the
        // ShortPress, giving the user a "way out" if they change their mind.
        if matches!(self.last_button_state, ButtonState::Pressed)
            && matches!(state, ButtonState::Released)
        {
            if let Some(pressed_at) = self.press_started_at.take() {
                let held = now.duration_since(pressed_at);

                if held <= Self::SHORT_PRESS_MAX {
                    if self
                        .last_short_release_at
                        .is_some_and(|prev| now.duration_since(prev) <= Self::DOUBLE_TAP_WINDOW)
                    {
                        log::info!("ui_task: double-tap detected");
                        self.last_short_release_at = None;
                        let _ = self
                            .sip_tx
                            .send(SipCommand::Button(ButtonEvent::DoubleTap));
                    } else {
                        log::info!("ui_task: short press detected (held {:?})", held);
                        self.last_short_release_at = Some(now);
                        let _ = self
                            .sip_tx
                            .send(SipCommand::Button(ButtonEvent::ShortPress));
                    }
                } else {
                    log::info!(
                        "ui_task: press ignored/cancelled (held {:?}, short={:?})",
                        held,
                        Self::SHORT_PRESS_MAX
                    );
                }
            }
        }

        self.last_button_state = state;
    }

    #[cfg(not(target_os = "espidf"))]
    fn poll_auto_answer(&mut self, now: Instant) {
        // Host-only auto-answer for testing without a physical device.
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

    #[cfg(target_os = "espidf")]
    fn poll_auto_answer(&mut self, _now: Instant) {}

    fn update_led(&mut self, now: Instant) {
        let desired = LedPattern::for_state(self.phone_state, self.registered);

        if desired != self.led_pattern {
            self.led_pattern = desired;
            self.led_on = true;
            self.next_blink_at = now
                + desired
                    .blink_period
                    .unwrap_or_else(|| Duration::from_secs(3600));
            self.last_led_state = None;
        }

        if let Some(period) = self.led_pattern.blink_period {
            if now >= self.next_blink_at {
                self.led_on = !self.led_on;
                self.next_blink_at = now + period;
            }
        } else {
            self.led_on = true;
        }

        let target = if self.led_on {
            LedState::Color {
                red: self.led_pattern.color.0,
                green: self.led_pattern.color.1,
                blue: self.led_pattern.color.2,
            }
        } else {
            LedState::Off
        };

        if self.last_led_state != Some(target) {
            log::debug!(
                "ui_task: LED update state={:?} registered={} target={:?}",
                self.phone_state,
                self.registered,
                target
            );
            let _ = self.ui_device.set_led_state(target);
            self.last_led_state = Some(target);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LedPattern {
    color: (u8, u8, u8),
    blink_period: Option<Duration>,
}

impl LedPattern {
    fn for_state(phone: PhoneState, registered: bool) -> Self {
        match phone {
            PhoneState::Ringing => Self {
                color: (255, 255, 0),
                blink_period: Some(Duration::from_millis(300)),
            },
            PhoneState::Established => Self {
                color: (0, 0, 255),
                blink_period: None,
            },
            PhoneState::Idle => {
                if registered {
                    Self {
                        color: (0, 255, 0),
                        blink_period: None,
                    }
                } else {
                    Self {
                        color: (255, 0, 0),
                        blink_period: Some(Duration::from_millis(800)),
                    }
                }
            }
        }
    }
}

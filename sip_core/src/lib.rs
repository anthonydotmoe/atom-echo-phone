use log::{debug, info};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationState {
    Unregistered,
    Registering,
    Registered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallState {
    Idle,
    Ringing,
    InCall,
}

#[derive(Debug)]
pub struct SipStack {
    registration: RegistrationState,
    call_state: CallState,
}

#[derive(Debug, Error)]
pub enum SipError {
    #[error("unsupported message: {0}")]
    Unsupported(String),
    #[error("invalid state: {0}")]
    InvalidState(String),
}

pub type Result<T> = std::result::Result<T, SipError>;

impl SipStack {
    pub fn new() -> Self {
        Self {
            registration: RegistrationState::Unregistered,
            call_state: CallState::Idle,
        }
    }

    pub fn register(&mut self) -> Result<()> {
        info!("performing registration transaction");
        self.registration = RegistrationState::Registered;
        Ok(())
    }

    pub fn handle_incoming(&mut self, message: &str) -> Result<()> {
        debug!("handling incoming SIP message: {message}");
        let _ = message;
        Ok(())
    }

    pub fn invite(&mut self, target: &str) -> Result<()> {
        debug!("sending INVITE to {target}");
        self.call_state = CallState::Ringing;
        Ok(())
    }

    pub fn hangup(&mut self) -> Result<()> {
        debug!("terminating current dialog");
        self.call_state = CallState::Idle;
        Ok(())
    }

    pub fn registration_state(&self) -> RegistrationState {
        self.registration
    }

    pub fn call_state(&self) -> CallState {
        self.call_state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_and_hangs_up() {
        let mut sip = SipStack::new();
        assert_eq!(sip.registration_state(), RegistrationState::Unregistered);
        sip.register().unwrap();
        assert_eq!(sip.registration_state(), RegistrationState::Registered);
        sip.invite("sip:100@example.com").unwrap();
        assert_eq!(sip.call_state(), CallState::Ringing);
        sip.hangup().unwrap();
        assert_eq!(sip.call_state(), CallState::Idle);
    }
}

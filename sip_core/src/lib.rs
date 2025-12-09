//! Pure SIP core: message model, parsing/rendering,
//! registration and dialog state machines, and high-level events.

//#![forbid(unsafe_code)]

mod message;
mod auth;
mod registration;
mod dialog;
mod stack;

pub use crate::message::{
    header_value, parse_message, Header, HeaderList, Method, Message, Request,
    Response, Version, SmallString, MAX_BODY_LEN, MAX_CALL_ID_LEN,
    MAX_HEADER_NAME, MAX_HEADER_VALUE, MAX_REASON_LEN, MAX_TAG_LEN, MAX_URI_LEN,
};

pub use crate::auth::{
    authorization_header, compute_digest_response, parse_www_authenticate,
    DigestChallenge, DigestCredentials,
};

pub use crate::registration::{
    RegistrationResult, RegistrationState, RegistrationTransaction,
};

pub use crate::dialog::{Dialog, DialogRole, DialogState, SipDialogId};

pub use crate::stack::{
    CoreEvent, CoreRegistrationEvent, CoreDialogEvent,
    SipStack
};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SipError {
    #[error("invalid message: {0}")]
    Invalid(&'static str),

    #[error("buffer too small")]
    Capacity,

    #[error("invalid state: {0}")]
    InvalidState(&'static str),
}

pub type Result<T> = core::result::Result<T, SipError>;

// --- Stack size logging facility ---------------------------------------------
extern "C" {
    fn uxTaskGetStackHighWaterMark(handle: *mut core::ffi::c_void) -> u32;
}

pub fn log_stack_high_water(msg: &'static str) {
    unsafe {
        // NULL -> "current task"
        let words_left = uxTaskGetStackHighWaterMark(core::ptr::null_mut());
        let bytes_left = words_left as usize * core::mem::size_of::<usize>();
        log::info!("{}: min remaining stack: {} bytes", msg, bytes_left);
    }
}

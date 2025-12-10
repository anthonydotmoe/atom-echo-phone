//! Pure SIP core: message model, parsing/rendering,
//! registration and dialog state machines, and high-level events.

//#![forbid(unsafe_code)]

mod message;
mod auth;
mod registration;
mod dialog;
mod stack;
mod transaction;

pub use crate::message::{
    header_value, parse_message, Header, HeaderList, Method, Message, Request,
    Response, Version,
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
    InviteKind, SipStack,
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

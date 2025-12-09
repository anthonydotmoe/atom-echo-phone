use heapless::Vec;

use crate::{
    Result, auth::DigestChallenge, dialog::{Dialog, DialogState, SipDialogId}, log_stack_high_water, message::{Message, Method, Request, Response, header_value}, registration::{RegistrationResult, RegistrationState, RegistrationTransaction}
};

pub const MAX_EVENTS: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreRegistrationEvent {
    StateChanged(RegistrationState),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreDialogEvent {
    IncomingInvite {
        dialog_id: SipDialogId,
        request: Request,
    },
    DialogStateChanged(DialogState),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreEvent {
    Registration(CoreRegistrationEvent),
    Dialog(CoreDialogEvent),
}

/// High-level SIP stack that wires registration + dialog together,
/// and converts incoming messages into events for the application.
#[derive(Debug, Default)]
pub struct SipStack {
    pub registration: RegistrationTransaction,
    pub dialog: Dialog,
    last_reg_state: RegistrationState,
}

impl SipStack {
    /// Build a REGISTER request. Application is responsible for sending it.
    pub fn build_register(
        &mut self,
        registrar_uri: &str,
        contact_uri: &str,
        via_host: &str,
        via_port: u16,
        expires: u32,
        auth_header: Option<crate::message::Header>,
    ) -> Result<Request> {
        log_stack_high_water("SipStack::build_register");
        self.registration
            .build_register(registrar_uri, contact_uri, via_host, via_port, expires, auth_header)
    }

    /// Handle a REGISTER response and emit registration events.
    pub fn on_register_response(
        &mut self,
        resp: &Response,
        events: &mut Vec<CoreEvent, MAX_EVENTS>,
    ) -> RegistrationResult {
        let result = self.registration.handle_response(resp);
        let state = self.registration.state();

        if state != self.last_reg_state {
            self.last_reg_state = state;
            let _ = events.push(CoreEvent::Registration(
                CoreRegistrationEvent::StateChanged(state),
            ));
        }

        result
    }

    /// Handle any incoming message and emit high-level events.
    ///
    /// This does *not* perform any I/O. The caller is responsible for:
    /// - Parsing text into `Message` (via `parse_message`).
    /// - Sending any `Request`/`Response` objects the application chooses to build.
    pub fn on_message(
        &mut self,
        msg: Message,
    ) -> Vec<CoreEvent, MAX_EVENTS> {
        let mut events: Vec<CoreEvent, MAX_EVENTS> = Vec::new();

        match msg {
            Message::Response(resp) => {
                if is_register_response(&resp) {
                    let _ = self.on_register_response(&resp, &mut events);
                } else {
                    // Outgoing dialog responses could be handled here too.
                    self.handle_non_register_response(&resp, &mut events);
                }
            }
            Message::Request(req) => {
                if req.method == Method::Invite {
                    self.handle_incoming_invite(req, &mut events);
                } else {
                    // You could extend this with BYE/CANCEL/etc handling.
                }
            }
        }

        events
    }

    fn handle_non_register_response(
        &mut self,
        resp: &Response,
        events: &mut Vec<CoreEvent, MAX_EVENTS>,
    ) {
        // Very small outgoing-call support:
        if let Some(cseq) = header_value(&resp.headers, "CSeq") {
            if cseq.trim().ends_with("INVITE") {
                let _ack = self.dialog.handle_final_response(resp.status_code);
                // For now we don't emit an ACK request; the app can call
                // dialog.build_ack() or handle_final_response directly if desired.
                let _ = events.push(CoreEvent::Dialog(
                    CoreDialogEvent::DialogStateChanged(self.dialog.state),
                ));
            }
        }
    }

    fn handle_incoming_invite(
        &mut self,
        req: Request,
        events: &mut Vec<CoreEvent, MAX_EVENTS>,
    ) {
        // Classify as an incoming dialog; this will also set dialog state.
        if let Ok(dialog_id) = self.dialog.classify_incoming_invite(&req) {
            let _ = events.push(CoreEvent::Dialog(CoreDialogEvent::IncomingInvite {
                dialog_id,
                request: req,
            }));
            let _ = events.push(CoreEvent::Dialog(
                CoreDialogEvent::DialogStateChanged(self.dialog.state),
            ));
        }
    }

    pub fn registration_state(&self) -> RegistrationState {
        self.registration.state()
    }

    pub fn last_challenge(&self) -> Option<DigestChallenge> {
        self.registration.last_challenge()
    }

    /// Helper: get suggested refresh interval in seconds based on last Expires.
    pub fn registration_refresh_interval_secs(&self) -> u64 {
        self.registration.next_refresh_interval_secs()
    }
}

/// Heuristic: treat any response whose CSeq ends in "REGISTER" as a REGISTER response.
fn is_register_response(resp: &Response) -> bool {
    if let Some(cseq) = header_value(&resp.headers, "CSeq") {
        cseq.trim().ends_with("REGISTER")
    } else {
        false
    }
}

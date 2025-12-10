use crate::{
    Result, auth::DigestChallenge, dialog::{Dialog, DialogState, SipDialogId}, message::{Message, Method, Request, Response, header_value}, registration::{RegistrationResult, RegistrationState, RegistrationTransaction}
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreRegistrationEvent {
    Result(RegistrationResult),
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
    SendResponse(Response),
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
        self.registration
            .build_register(registrar_uri, contact_uri, via_host, via_port, expires, auth_header)
    }

    /// Handle a REGISTER response and emit registration events.
    pub fn on_register_response(
        &mut self,
        resp: &Response,
        events: &mut Vec<CoreEvent>,
    ) -> RegistrationResult {
        let result = self.registration.handle_response(resp);
        let state = self.registration.state();

        if state != self.last_reg_state {
            self.last_reg_state = state;
            let _ = events.push(CoreEvent::Registration(
                CoreRegistrationEvent::Result(result),
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
    ) -> Vec<CoreEvent> {
        let mut events: Vec<CoreEvent> = Vec::new();

        match msg {
            Message::Response(resp) => {
                if is_register_response(&resp) {
                    let res = self.registration.handle_response(&resp);

                    // Emit the result so SipTask can schedule timers, etc.
                    let _ = events.push(CoreEvent::Registration(
                        CoreRegistrationEvent::Result(res),
                    ));

                    let state = self.registration.state();
                    let _ = events.push(CoreEvent::Registration(
                        CoreRegistrationEvent::StateChanged(state),
                    ));

                    return events;
                } else {
                    // Non-REGISTER responses (e.g. INVITE/ACK/BYE flows) are
                    // not handled yet.
                    log::warn!("on_message: unhandled non-REGISTER response: {}", resp.status_code);
                }
            }
            Message::Request(req) => {
                match req.method {
                    Method::Invite => self.handle_incoming_invite(req, &mut events),
                    Method::Cancel => self.handle_incoming_cancel(req, &mut events),
                    Method::Ack    => self.handle_incoming_ack(req, &mut events),
                    Method::Bye    => self.handle_incoming_bye(req, &mut events),
                    m => { log::warn!("on_message: unhandled request: {}", m); },
                }
            }
        }

        events
    }

    fn handle_incoming_invite(
        &mut self,
        req: Request,
        events: &mut Vec<CoreEvent>,
    ) {
        // Classify as an incoming dialog; this will also set dialog state.
        if let Ok(dialog_id) = self.dialog.classify_incoming_invite(&req) {
            let _ = events.push(CoreEvent::Dialog(CoreDialogEvent::IncomingInvite {
                dialog_id,
                request: req,
            }));
            let _ = events.push(CoreEvent::Dialog(
                CoreDialogEvent::DialogStateChanged(self.dialog.state.clone()),
            ));
        }
    }

    fn handle_incoming_cancel(
        &mut self,
        req: Request,
        events: &mut Vec<CoreEvent>,
    ) {
        match self.dialog.handle_incoming_cancel(&req) {
            Ok(cancel_res) => {
                // Emit the responses we must send
                let _ = events.push(
                    CoreEvent::SendResponse(cancel_res.cancel_ok),
                );
                if let Some(resp_487) = cancel_res.maybe_invite_487 {
                    let _ = events.push(
                        CoreEvent::SendResponse(resp_487),
                    );
                }

                // And let the app know the dialog died
                let _ = events.push(CoreEvent::Dialog(
                    CoreDialogEvent::DialogStateChanged(self.dialog.state.clone()),
                ));
            }
            Err(_e) => {
                // log::warn!("handle_incoming_cancel: {:?}", e);
            }
        }
    }

    fn handle_incoming_ack(
        &mut self,
        req: Request,
        events: &mut Vec<CoreEvent>,
    ) {
        if let Err(e) = self.dialog.handle_incoming_ack(&req) {
            // log::warn!("handle_incoming_ack: {:?}", e);
            return;
        }

        let _ = events.push(CoreEvent::Dialog(
            CoreDialogEvent::DialogStateChanged(self.dialog.state.clone())
        ));
    }

    fn handle_incoming_bye (
        &mut self,
        req: Request,
        events: &mut Vec<CoreEvent>,
    ) {
        match self.dialog.handle_incoming_bye(&req) {
            Ok(resp) => {
                // send 200 OK for BYE
                let _ = events.push(CoreEvent::SendResponse(resp));
                // dialog is already moved to Terminated by the dialog helper
                let _ = events.push(CoreEvent::Dialog(
                    CoreDialogEvent::DialogStateChanged(self.dialog.state.clone())
                ));
            }
            Err(_e) => {
                // log::warn!("handle_incoming_bye: {:?}", e);
            }
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

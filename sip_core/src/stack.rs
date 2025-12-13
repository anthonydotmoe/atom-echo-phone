use crate::Result;
use crate::auth::DigestChallenge;
use crate::dialog::{Dialog, DialogState};
use crate::message::{Header, Message, Method, Request, Response, header_value};
use crate::registration::{RegistrationResult, RegistrationState, RegistrationTransaction};
use crate::transaction::InviteServerTransactionManager;
use std::net::SocketAddr;
use std::time::Instant;

const ALLOW_HEADER_VALUE: &str = "INVITE, ACK, CANCEL, BYE, OPTIONS";
const ACCEPT_HEADER_VALUE: &str = "application/sdp";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreRegistrationEvent {
    Result(RegistrationResult),
    StateChanged(RegistrationState),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InviteKind {
    Initial,
    Reinvite,
    InitialWhileBusy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreDialogEvent {
    IncomingInvite {
        kind: InviteKind,
        request: Request,
    },
    DialogStateChanged(DialogState),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreEvent {
    Registration(CoreRegistrationEvent),
    Dialog(CoreDialogEvent),
    SendResponse(Response),
    SendResponseTo {
        response: Response,
        target: SocketAddr,
    },
}

/// High-level SIP stack that wires registration + dialog together,
/// and converts incoming messages into events for the application.
#[derive(Debug, Default)]
pub struct SipStack {
    pub registration: RegistrationTransaction,
    pub dialog: Dialog,
    invite_transactions: InviteServerTransactionManager,
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
    pub fn on_message(&mut self, msg: Message, remote_addr: SocketAddr, now: Instant) -> Vec<CoreEvent> {
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
                    Method::Invite => self.handle_incoming_invite(req, remote_addr, &mut events),
                    Method::Cancel => self.handle_incoming_cancel(req, remote_addr, now, &mut events),
                    Method::Ack    => self.handle_incoming_ack(req, now, &mut events),
                    Method::Bye    => self.handle_incoming_bye(req, &mut events),
                    Method::Options => self.handle_incoming_options(req, &mut events),
                    m => { log::warn!("on_message: unhandled request: {}", m); },
                }
            }
        }

        events
    }

    pub fn poll_timers(&mut self, now: Instant) -> Vec<CoreEvent> {
        let mut events = Vec::new();
        for (resp, target) in self.invite_transactions.poll(now) {
            let _ = events.push(CoreEvent::SendResponseTo { response: resp, target });
        }
        events
    }

    /// Record an outgoing response so the stack can handle retransmissions.
    pub fn record_outgoing_response(&mut self, resp: &Response, target: SocketAddr, now: Instant) {
        self.invite_transactions.on_outgoing_response(resp, target, now);
    }

    fn handle_incoming_invite(
        &mut self,
        req: Request,
        remote_addr: SocketAddr,
        events: &mut Vec<CoreEvent>,
    ) {
        if let Some(resp) = self.invite_transactions.on_invite(&req, remote_addr) {
            let _ = events.push(CoreEvent::SendResponseTo { response: resp, target: remote_addr });
        }

        let dialog_events = self.dialog.handle_incoming_invite(req);
        events.extend(dialog_events);
    }

    fn handle_incoming_cancel(
        &mut self,
        req: Request,
        remote_addr: SocketAddr,
        now: Instant,
        events: &mut Vec<CoreEvent>,
    ) {
        match self.dialog.handle_incoming_cancel(&req) {
            Ok(cancel_res) => {
                // Emit the responses we must send
                self.invite_transactions.on_outgoing_response(&cancel_res.cancel_ok, remote_addr, now);
                let _ = events.push(
                    CoreEvent::SendResponse(cancel_res.cancel_ok),
                );
                if let Some(resp_487) = cancel_res.maybe_invite_487 {
                    self.invite_transactions.on_outgoing_response(&resp_487, remote_addr, now);
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
        now: Instant,
        events: &mut Vec<CoreEvent>,
    ) {
        self.invite_transactions.on_ack(&req, now);

        if let Err(_e) = self.dialog.handle_incoming_ack(&req) {
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

    fn handle_incoming_options(
        &mut self,
        req: Request,
        events: &mut Vec<CoreEvent>,
    ) {
        match self.dialog.build_response_for_request(&req, 200, "OK", None) {
            Ok(mut resp) => {
                if let Ok(allow) = Header::new("Allow", ALLOW_HEADER_VALUE) {
                    resp.add_header(allow);
                }
                if let Ok(accept) = Header::new("Accept", ACCEPT_HEADER_VALUE) {
                    resp.add_header(accept);
                }
                let _ = events.push(CoreEvent::SendResponse(resp));
            }
            Err(e) => {
                log::warn!("handle_incoming_options: {:?}", e);
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

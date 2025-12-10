use core::fmt::Write;
use core::mem;
use std::fmt::Display;

use crate::{
    CoreDialogEvent, CoreEvent, Result, SipError, header_value, message::{Header, Method, Request, Response}, stack::InviteKind
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogState {
    Idle,
    Inviting, // UAC side, INVITE sent, no confirmed dialog yet
    Ringing {
        role: DialogRole,
        id: SipDialogId,
        original_invite: Request,
    },
    Established {
        role: DialogRole,
        id: SipDialogId,
    },
    Terminated,
}

impl Default for DialogState {
    fn default() -> Self {
        DialogState::Idle
    }
}

impl Display for DialogState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            &DialogState::Idle => f.write_str("Idle"),
            &DialogState::Inviting => f.write_str("Inviting"),
            &DialogState::Ringing {..} => f.write_str("Ringing"),
            &DialogState::Established {..} => f.write_str("Established"),
            &DialogState::Terminated => f.write_str("Terminated"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogRole {
    Uac, // we initiated the call
    Uas, // remote initiated the call
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SipDialogId {
    pub call_id: String,
    pub local_tag: String,
    pub remote_tag: String,
}

pub struct CancelResult {
    pub cancel_ok: Response,
    pub maybe_invite_487: Option<Response>,
}

#[derive(Debug, Default)]
pub struct Dialog {
    pub state: DialogState,
    pub cseq: u32,
    next_tag_counter: u32,
}

impl Dialog {
    pub fn new() -> Self {
        Self {
            state: DialogState::Idle,
            cseq: 0,
            next_tag_counter: 1,
        }
    }

    fn allocate_tag(&mut self) -> String {
        let mut tag = String::new();
        let idx = self.next_tag_counter;
        self.next_tag_counter = self.next_tag_counter.wrapping_add(1);
        let _ = write!(tag, "dlg{:x}", idx);
        tag
    }

    /// Small helpers so the rest of the code doesn't have to pattern-match
    /// on `DialogState` over and over.
    fn id_mut(&mut self) -> Option<&mut SipDialogId> {
        match &mut self.state {
            DialogState::Ringing { id, .. } | DialogState::Established { id, .. } => Some(id),
            _ => None,
        }
    }

    fn id_ref(&self) -> Option<&SipDialogId> {
        match &self.state {
            DialogState::Ringing { id, .. } | DialogState::Established { id, .. } => Some(id),
            _ => None,
        }
    }

    /// Start an outgoing INVITE (UAC side).
    pub fn start_outgoing(&mut self, target: &str) -> Result<Request> {
        if self.state != DialogState::Idle && self.state != DialogState::Terminated {
            return Err(SipError::InvalidState("dialog busy"));
        }
        self.state = DialogState::Inviting;
        self.cseq = self.cseq.wrapping_add(1);

        let mut req = Request::new(Method::Invite, target)?;
        // Call-ID and tags should be set by the application (using headers),
        // but we keep cseq internally so we can build ACK/BYE later.
        let cseq_header = self.cseq_header("INVITE")?;
        req.add_header(cseq_header)?;
        Ok(req)
    }

    pub fn build_bye(&mut self, target: &str) -> Option<Request> {
        if !matches!(self.state, DialogState::Established { .. }) {
            return None;
        }
        self.cseq = self.cseq.wrapping_add(1);
        let mut req = Request::new(Method::Bye, target).ok()?;
        let cseq_header = self.cseq_header("BYE").ok()?;
        req.add_header(cseq_header).ok()?;
        self.state = DialogState::Terminated;
        Some(req)
    }

    fn build_ack(&mut self) -> Result<Request> {
        let mut req = Request::new(Method::Ack, "sip:remote")?;
        let cseq_header = self.cseq_header("ACK")?;
        req.add_header(cseq_header)?;
        Ok(req)
    }

    fn cseq_header(&self, method: &str) -> Result<Header> {
        let mut value = String::new();
        write!(value, "{} {}", self.cseq, method).map_err(|_| SipError::Capacity)?;
        Header::new("CSeq", &value)
    }

    /// Build a 180/100/486… response based on an incoming request.
    ///
    /// Tag handling:
    /// - If the request already has a To tag, we reuse it as the local tag.
    /// - Otherwise we allocate and append one.
    pub fn build_response_for_request(
        &mut self,
        req: &Request,
        status: u16,
        reason: &str,
        body: Option<(&str, &str)>, //TODO: (Content-Type, <data>) struct?
    ) -> Result<Response> {
        let mut resp = Response::new(status, reason)?;

        // SIP/2.0 fields already set in Response::new.

        // Via: copy as-is
        if let Some(via) = header_value(&req.headers, "Via") {
            resp.add_header(Header::new("Via", via)?);
        } else {
            return Err(SipError::Invalid("missing Via"));
        }

        // Call-ID
        let call_id = header_value(&req.headers, "Call-ID")
            .ok_or(SipError::Invalid("missing Call-ID"))?;
        resp.add_header(Header::new("Call-ID", call_id)?);

        // CSeq
        let cseq = header_value(&req.headers, "CSeq")
            .ok_or(SipError::Invalid("missing CSeq"))?;
        resp.add_header(Header::new("CSeq", cseq)?);

        // From
        let from = header_value(&req.headers, "From")
            .ok_or(SipError::Invalid("missing From"))?;
        resp.add_header(Header::new("From", from)?);

        // To: ensure it has a tag
        let mut to_value = String::new();
        let raw_to = header_value(&req.headers, "To")
            .ok_or(SipError::Invalid("missing To"))?;
        to_value.push_str(raw_to);

        // Decide whether we need to add a tag
        if !raw_to.to_ascii_lowercase().contains("tag=") {
            // We'll decide which tag to use then append once.
            let tag_to_use;

            let new_tag = self.allocate_tag();
            if let Some(id) = self.id_mut() {
                if id.call_id == call_id {
                    // This response belongs to our current dialog
                    if id.local_tag.is_empty() {
                        // First time we're adding a tag for this dialog
                        id.local_tag.clear();
                        id.local_tag.push_str(new_tag.as_str());
                    }

                    tag_to_use = Some(id.local_tag.clone());
                } else {
                    // Some other transaction; generate a one-off tag
                    tag_to_use = Some(self.allocate_tag());
                }
            } else {
                // No dialog state at all (e.g., standalone response)
                tag_to_use = Some(self.allocate_tag());
            }

            if let Some(tag) = tag_to_use {
                to_value.push_str(";tag=");
                to_value.push_str(tag.as_str());
            }
        }

        resp.add_header(Header::new("To", &to_value)?);

        // Content-Length / body
        if let Some(b) = body {
            resp.add_header(Header::new("Content-Type", b.0)?);
            resp.set_body(b.1);
            let len_str = b.1.len().to_string();
            resp.add_header(Header::new("Content-Length", &len_str)?);
        } else {
            resp.add_header(Header::new("Content-Length", "0")?);
        }

        Ok(resp)
    }

    pub fn handle_incoming_invite(&mut self, req: Request) -> Vec<CoreEvent> {
        let mut events = Vec::new();

        let call_id = match header_value(&req.headers, "Call-ID") {
            Some(v) => v,
            None => {
                log::debug!("handle_incoming_invite: missing Call-ID");
                // Maybe return 400
                return events;
            }
        };

        let from = match header_value(&req.headers, "From") {
            Some(v) => v,
            None => {
                log::debug!("handle_incoming_invite: missing From");
                return events;
            }
        };

        let to = match header_value(&req.headers, "To") {
            Some(v) => v,
            None => {
                log::debug!("handle_incoming_invite: missing To");
                return events;
            }
        };

        let from_tag = match parse_tag_param(from) {
            Some(tag) => tag,
            None => {
                // No From tag: this is weird for an in-dialog INVITE, treat as new
                log::debug!(
                    "handle_incoming_invite: no From tag, treating as initial. call_id={}",
                    call_id
                );
                // do NOT change state here; just tell the app it's an initial
                // attempt, and let it decide what to do.
                events.push(CoreEvent::Dialog(CoreDialogEvent::IncomingInvite {
                    request: req,
                    kind: InviteKind::Initial,
                }));
                return events;
            }
        };

        let to_tag = parse_tag_param(to);

        log::debug!(
            "handle_incoming_invite: call_id={} from_tag={:?} to_tag={:?} state={:?}",
            call_id,
            from_tag,
            to_tag,
            self.state,
        );

        // Decide if this matches the existing dialog
        let in_dialog = match &self.state {
            DialogState::Ringing { id, role, .. }
            | DialogState::Established { id, role , .. } if *role == DialogRole::Uas => {
                log::debug!(
                    "handle_incoming_invite: current dialog id: call_id={} local_tag={:?} remote_tag={:?}",
                    id.call_id,
                    id.local_tag,
                    id.remote_tag,
                );

                if id.call_id != call_id {
                    log::debug!("handle_incoming_invite: Call-ID mismatch");
                    false
                } else if id.remote_tag != from_tag {
                    log::debug!(
                        "handle_incoming_invite: remote_tag mismatch: expected={:?} got={:?}",
                        id.remote_tag,
                        from_tag
                    );
                    false
                } else if id.local_tag.is_empty() {
                    // Early state: we haven't committed a local tag yet,
                    // so we can't rely on To-tag matching.
                    // Treat as "same dialog" based on Call-ID + remote tag alone.
                    log::debug!(
                        "handle_incoming_invite: local_tag empty, accepting based on Call-ID+remote_tag"
                    );
                    true
                } else {
                    // Fully formed dialog: require To-tag match as well.
                    match to_tag {
                        Some(tag) if tag == id.local_tag => {
                            log::debug!("handle_incoming_invite: To-tag matches local_tag");
                            true
                        }
                        Some(tag) => {
                            log::debug!(
                                "handle_incoming_invite: To-tag mismatch: expected={:?} got={:?}",
                                id.local_tag,
                                tag
                            );
                            false
                        }
                        None => {
                            log::debug!(
                                "handle_incoming_invite: missing To-tag but dialog has local_tag={:?}",
                                id.local_tag
                            );
                            false
                        },
                    }
                }
            }
            other_state => {
                log::debug!(
                    "handle_incoming_invite: state not considered in-dialog\r\n{:?}",
                    other_state
                );
                false
            },
        };

        if in_dialog {
            // DO NOT reset state to Ringing here.
            // Just emit "incoming INVITE, in-dialog"
            log::debug!(
                "handle_incoming_invite: classified as RE-INVITE (in-dialog) for Call-ID={}",
                call_id
            );
            events.push(CoreEvent::Dialog(CoreDialogEvent::IncomingInvite {
                request: req,
                kind: InviteKind::Reinvite,
            }));

            return events;
        }
        
        // Not in-dialog: This is some kind of initial INVITE.
        // Decide whether we are free to accept a new dialog.
        let can_start_new_dialog = matches!(self.state, DialogState::Idle | DialogState::Terminated);

        if can_start_new_dialog {
            log::debug!(
                "handle_incoming_invite: classified INITIAL INVITE for Call-ID={}",
                call_id
            );

            self.handle_initial_invite(&req);
            events.push(CoreEvent::Dialog(CoreDialogEvent::IncomingInvite {
                request: req,
                kind: InviteKind::Initial,
            }));
        } else {
            log::debug!(
                "handle_incoming_invite: got NEW INVITE while busy in state {}, reporting InitialWhileBusy and not changing dialog state",
                self.state
            );

            events.push(CoreEvent::Dialog(CoreDialogEvent::IncomingInvite {
                request: req,
                kind: InviteKind::InitialWhileBusy,
            }));
        }
        
        events
    }

    fn handle_initial_invite(&mut self, req: &Request) {
        let call_id = match header_value(&req.headers, "Call-ID") {
            Some(v) => v,
            None => return,
        };

        let from = match header_value(&req.headers, "From") {
            Some(v) => v,
            None => return,
        };

        let from_tag = match parse_tag_param(from) {
            Some(tag) => tag,
            None => return,
        };


        self.state = DialogState::Ringing {
            role: DialogRole::Uas,
            id: SipDialogId {
                call_id: call_id.to_string(),
                local_tag: String::new(), // will be set when building 18x/200
                remote_tag: from_tag.to_string(),
            },
            original_invite: req.clone(),
        };
    }

    /// Checks current call status. If incoming CANCEL matches the current call,
    /// transision to Terminated
    pub fn handle_incoming_cancel(&mut self, cancel_req: &Request) -> Result<CancelResult> {
        // Take the current state out so we can move from it.
        let old_state = mem::replace(&mut self.state, DialogState::Idle);

        // Only meaningful if we are ringing as UAS
        let (role, id, original_invite) = match old_state {
            DialogState::Ringing { role, id, original_invite }  => (role, id, original_invite),
            other => {
                // Restore state
                self.state = other;
                return Err(SipError::InvalidState(
                    "received CANCEL while not ringing",
                ))
            }
        };

        if role != DialogRole::Uas {
            // Restore state if we somehow weren't UAS
            self.state = DialogState::Ringing { role, id, original_invite };
            return Err(SipError::InvalidState(
                "received CANCEL but we are not UAS",
            ));
        }

        let cancel_call_id = match header_value(&cancel_req.headers, "Call-ID") {
            Some(v) => v,
            None => {
                self.state = DialogState::Ringing { role, id, original_invite };
                return Err(SipError::Invalid("missing Call-ID"));
            }
        };
        let cancel_from = match header_value(&cancel_req.headers, "From") {
            Some(v) => v,
            None => {
                self.state = DialogState::Ringing { role, id, original_invite };
                return Err(SipError::Invalid("missing From"));
            }
        };
        // try to extract tag from From: ...;tag=foo
        let cancel_remote_tag = parse_tag_param(cancel_from).unwrap_or("remote");

        // Same Call-ID and same remote tag -> this CANCEL is for our dialog
        if cancel_call_id != id.call_id || cancel_remote_tag != id.remote_tag {
            self.state = DialogState::Ringing { role, id, original_invite };
            return Err(SipError::Invalid("CANCEL does not match current dialog"));
        }
        
        // 200 OK to the CANCEL itself
        let cancel_ok =
            self.build_response_for_request(cancel_req, 200, "OK", None)?;
        
        // 487 to the original INVITE
        let invite_487 =
            self.build_response_for_request(&original_invite, 487, "Request Terminated", None)?;
        
        self.state = DialogState::Terminated;

        Ok(CancelResult {
            cancel_ok,
            maybe_invite_487: Some(invite_487),
        })
    }

    pub fn handle_incoming_ack(&mut self, ack_req: &Request) -> Result<()> {
        // Only meaningful if we are UAS and currently ringing or already established
        let (role, id) = match &self.state {
            DialogState::Ringing { role, id, .. }
            | DialogState::Established { role, id, .. } => (role, id),
            _ => return Err(SipError::InvalidState("ACK in wrong state")),
        };

        if *role != DialogRole::Uas {
            return Err(SipError::InvalidState("ACK but we are not UAS"));
        }

        // Light matching: Call-ID and To tag
        let call_id = header_value(&ack_req.headers, "Call-ID")
            .ok_or(SipError::Invalid("missing Call-ID"))?;
        let to = header_value(&ack_req.headers, "To")
            .ok_or(SipError::Invalid("missing To"))?;

        let ack_to_tag = parse_tag_param(to).unwrap_or("");

        if call_id != id.call_id || ack_to_tag != id.local_tag {
            return Err(SipError::Invalid("ACK does not match current dialog"));
        }

        // Promote to Established if we were still Ringing
        if matches!(self.state, DialogState::Ringing { .. }) {
            self.state = DialogState::Established {
                role: *role,
                id: id.clone(),
            };
        }

        Ok(())
    }

    pub fn handle_incoming_bye(&mut self, bye_req: &Request) -> Result<Response> {
        let (role, id) = match &self.state {
            DialogState::Established { role, id } => (role, id),
            _ => return Err(SipError::InvalidState("BYE in wrong state")),
        };

        // For UAS: From is remote, To is local. For UAC it's reversed.
        let call_id = header_value(&bye_req.headers, "Call-ID")
            .ok_or(SipError::Invalid("missing Call-ID"))?;
        let from = header_value(&bye_req.headers, "From")
            .ok_or(SipError::Invalid("missing From"))?;
        let to = header_value(&bye_req.headers, "To")
            .ok_or(SipError::Invalid("missing To"))?;

        let from_tag = parse_tag_param(from).unwrap_or("");
        let to_tag = parse_tag_param(to).unwrap_or("");

        let matches = if *role == DialogRole::Uas {
            // remote = From, local = To
            call_id == id.call_id && from_tag == id.remote_tag && to_tag == id.local_tag
        } else {
            // UAC: remote = To, local = From
            call_id == id.call_id && to_tag == id.remote_tag && from_tag == id.local_tag
        };

        if !matches {
            return Err(SipError::Invalid("BYE does not match current dialog"));
        }

        // Move dialog to Terminated
        self.state = DialogState::Terminated;

        // 200 OK for BYE
        self.build_response_for_request(bye_req, 200, "OK", None)
    }

    pub fn terminate_local(&mut self) {
        self.state = DialogState::Terminated;
    }
}

fn parse_tag_param(input: &str) -> Option<&str> {
    // naive parse: search for "tag=" and take until next semicolon
    let lower = input.to_ascii_lowercase();
    let pos = lower.find("tag=")?;
    let rest = &input[pos + 4..];
    let end = rest.find(';').unwrap_or(rest.len());
    Some(&rest[..end])
}

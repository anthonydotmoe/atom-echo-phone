use core::fmt::Write;
use core::mem;
use std::fmt::Display;

use crate::{
    header_value,
    message::{Header, Method, Request, Response},
    Result, SipError,
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
        body: Option<&str>,
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
            resp.set_body(b);
            let len_str = b.len().to_string();
            resp.add_header(Header::new("Content-Length", &len_str)?);
        } else {
            resp.add_header(Header::new("Content-Length", "0")?);
        }

        Ok(resp)
    }

    /// Very small helper to interpret an incoming INVITE as a dialog start.
    /// You can use this to produce a high-level "IncomingInvite" event.
    pub fn classify_incoming_invite(&mut self, req: &Request) -> Result<SipDialogId> {
        let call_id = header_value(&req.headers, "Call-ID")
            .ok_or(SipError::Invalid("missing Call-ID"))?;
        let from = header_value(&req.headers, "From")
            .ok_or(SipError::Invalid("missing From"))?;

        // try to extract tag from From: ...;tag=foo
        let remote_tag = parse_tag_param(from).unwrap_or("remote");

        let mut cid = String::new();
        cid.push_str(call_id);

        let mut remote = String::new();
        remote
            .push_str(remote_tag);

        let id = SipDialogId {
            call_id: cid,
            local_tag: String::new(),
            remote_tag: remote,
        };

        self.state = DialogState::Ringing {
            id: id.clone(),
            role: DialogRole::Uas,
            original_invite: req.clone(),
        };

        Ok(id)
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

        let cancel_call_id = header_value(&cancel_req.headers, "Call-ID")
            .ok_or(SipError::Invalid("missing Call-ID"))?;
        let cancel_from = header_value(&cancel_req.headers, "From")
            .ok_or(SipError::Invalid("missing From"))?;
        // try to extract tag from From: ...;tag=foo
        let cancel_remote_tag = parse_tag_param(cancel_from).unwrap_or("remote");

        // Same Call-ID and same remote tag -> this CANCEL is for our dialog
        if cancel_call_id != id.call_id || cancel_remote_tag != id.remote_tag {
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

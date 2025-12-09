use core::fmt::Write;

use crate::{
    header_value,
    message::{Header, Method, Request, Response},
    Result, SipError,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum DialogState {
    #[default]
    Idle,
    Inviting,
    Ringing,
    Established,
    Terminated,
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

#[derive(Debug, Default)]
pub struct Dialog {
    pub state: DialogState,
    pub role: Option<DialogRole>,
    pub id: Option<SipDialogId>,
    pub cseq: u32,
    next_tag_counter: u32,
}

impl Dialog {
    pub fn new() -> Self {
        Self {
            state: DialogState::Idle,
            role: None,
            id: None,
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

    /// Start an outgoing INVITE (UAC side).
    pub fn start_outgoing(&mut self, target: &str) -> Result<Request> {
        if self.state != DialogState::Idle && self.state != DialogState::Terminated {
            return Err(SipError::InvalidState("dialog busy"));
        }
        self.state = DialogState::Inviting;
        self.role = Some(DialogRole::Uac);
        self.cseq = self.cseq.wrapping_add(1);

        let mut req = Request::new(Method::Invite, target)?;
        // Call-ID and tags should be set by the application (using headers),
        // but we keep cseq internally so we can build ACK/BYE later.
        let cseq_header = self.cseq_header("INVITE")?;
        req.add_header(cseq_header)?;
        Ok(req)
    }

    pub fn handle_final_response(&mut self, status: u16) -> Option<Request> {
        match status {
            180 | 183 => {
                self.state = DialogState::Ringing;
                None
            }
            200 => {
                self.state = DialogState::Established;
                self.build_ack().ok()
            }
            _ => {
                self.state = DialogState::Terminated;
                None
            }
        }
    }

    pub fn build_bye(&mut self, target: &str) -> Option<Request> {
        if self.state != DialogState::Established {
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

        if !raw_to.to_ascii_lowercase().contains("tag=") {
            let local_tag = self.allocate_tag();
            if !to_value.contains(";") {
                to_value.push_str(";tag=");
            } else {
                to_value.push_str(";tag=");
            }
            to_value
                .push_str(local_tag.as_str());

            // store dialog id if we haven't yet
            if self.id.is_none() {
                let mut cid = String::new();
                cid.push_str(call_id);
                let mut local = String::new();
                local
                    .push_str(local_tag.as_str());
                // remote tag comes from From header if present; for now we just leave it empty.
                let remote = String::new();
                self.id = Some(SipDialogId {
                    call_id: cid,
                    local_tag: local,
                    remote_tag: remote,
                });
                self.role = Some(DialogRole::Uas);
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
        let local_tag = "local"; // will be assigned when building responses

        let mut cid = String::new();
        cid.push_str(call_id);

        let mut local = String::new();
        local.push_str(local_tag);

        let mut remote = String::new();
        remote
            .push_str(remote_tag);

        let id = SipDialogId {
            call_id: cid,
            local_tag: local,
            remote_tag: remote,
        };

        self.id = Some(id.clone());
        self.role = Some(DialogRole::Uas);
        self.state = DialogState::Ringing;

        Ok(id)
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

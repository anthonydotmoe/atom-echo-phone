use core::fmt::Write;
use core::time::Duration;
use heapless::{String, Vec};
use thiserror::Error;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use md5::{Digest, Md5};

const MAX_URI_LEN: usize = 96;
const MAX_HEADER_NAME: usize = 32;
const MAX_HEADER_VALUE: usize = 256;
const MAX_REASON_LEN: usize = 64;
const MAX_BODY_LEN: usize = 256;
const MAX_HEADERS: usize = 16;
const MAX_TAG_LEN: usize = 32;
const MAX_BRANCH_LEN: usize = 64;
const MAX_CALL_ID_LEN: usize = 64;

pub type SmallString<const N: usize> = String<N>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Register,
    Invite,
    Ack,
    Bye,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub major: u8,
    pub minor: u8,
}

impl Version {
    pub const SIP_2_0: Version = Version { major: 2, minor: 0 };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub name: SmallString<MAX_HEADER_NAME>,
    pub value: SmallString<MAX_HEADER_VALUE>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SipTransactionId {
    pub branch: SmallString<MAX_BRANCH_LEN>,
    pub call_id: SmallString<MAX_CALL_ID_LEN>,
    pub cseq: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SipDialogId {
    pub call_id: SmallString<MAX_CALL_ID_LEN>,
    pub local_tag: SmallString<MAX_TAG_LEN>,
    pub remote_tag: Option<SmallString<MAX_TAG_LEN>>,
}

pub type HeaderList = Vec<Header, MAX_HEADERS>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    pub uri: SmallString<MAX_URI_LEN>,
    pub version: Version,
    pub headers: HeaderList,
    pub body: SmallString<MAX_BODY_LEN>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub version: Version,
    pub status_code: u16,
    pub reason: SmallString<MAX_REASON_LEN>,
    pub headers: HeaderList,
    pub body: SmallString<MAX_BODY_LEN>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Request(Request),
    Response(Response),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RegistrationState {
    #[default]
    Unregistered,
    Registering,
    Registered,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DialogState {
    #[default]
    Idle,
    Inviting,
    Ringing,
    Established,
    Terminated,
}

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

impl Header {
    pub fn new(name: &str, value: &str) -> Result<Self> {
        let mut name_buf: SmallString<MAX_HEADER_NAME> = SmallString::new();
        name_buf
            .push_str(name)
            .map_err(|_| SipError::Capacity)?;
        let mut value_buf: SmallString<MAX_HEADER_VALUE> = SmallString::new();
        value_buf
            .push_str(value)
            .map_err(|_| SipError::Capacity)?;
        Ok(Header {
            name: name_buf,
            value: value_buf,
        })
    }
}

impl Request {
    pub fn new(method: Method, uri: &str) -> Result<Self> {
        let mut uri_buf: SmallString<MAX_URI_LEN> = SmallString::new();
        uri_buf
            .push_str(uri)
            .map_err(|_| SipError::Capacity)?;

        Ok(Self {
            method,
            uri: uri_buf,
            version: Version::SIP_2_0,
            headers: HeaderList::new(),
            body: SmallString::new(),
        })
    }

    pub fn add_header(&mut self, header: Header) -> Result<()> {
        self.headers.push(header).map_err(|_| SipError::Capacity)
    }

    pub fn set_body(&mut self, body: &str) -> Result<()> {
        self.body = SmallString::new();
        self.body.push_str(body).map_err(|_| SipError::Capacity)
    }

    pub fn render(&self) -> Result<SmallString<512>> {
        let mut out: SmallString<512> = SmallString::new();
        write!(out, "{} {} SIP/{}.{}\r\n", self.method, self.uri, self.version.major, self.version.minor)
            .map_err(|_| SipError::Capacity)?;
        for header in &self.headers {
            write!(out, "{}: {}\r\n", header.name, header.value).map_err(|_| SipError::Capacity)?;
        }
        write!(out, "\r\n{}", self.body).map_err(|_| SipError::Capacity)?;
        Ok(out)
    }
}

impl Response {
    pub fn new(status_code: u16, reason: &str) -> Result<Self> {
        let mut reason_buf: SmallString<MAX_REASON_LEN> = SmallString::new();
        reason_buf
            .push_str(reason)
            .map_err(|_| SipError::Capacity)?;

        Ok(Self {
            version: Version::SIP_2_0,
            status_code,
            reason: reason_buf,
            headers: HeaderList::new(),
            body: SmallString::new(),
        })
    }

    pub fn add_header(&mut self, header: Header) -> Result<()> {
        self.headers.push(header).map_err(|_| SipError::Capacity)
    }

    pub fn render(&self) -> Result<SmallString<512>> {
        let mut out: SmallString<512> = SmallString::new();
        write!(
            out,
            "SIP/{}.{} {} {}\r\n",
            self.version.major, self.version.minor, self.status_code, self.reason
        )
        .map_err(|_| SipError::Capacity)?;

        for header in &self.headers {
            write!(out, "{}: {}\r\n", header.name, header.value).map_err(|_| SipError::Capacity)?;
        }
        write!(out, "\r\n{}", self.body).map_err(|_| SipError::Capacity)?;
        Ok(out)
    }
}

impl core::fmt::Display for Method {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Method::Register => write!(f, "REGISTER"),
            Method::Invite => write!(f, "INVITE"),
            Method::Ack => write!(f, "ACK"),
            Method::Bye => write!(f, "BYE"),
        }
    }
}

pub fn parse_message(input: &str) -> Result<Message> {
    let mut lines = input.split("\r\n");
    let first = lines.next().ok_or(SipError::Invalid("empty message"))?;

    if first.starts_with("SIP/") {
        parse_response(first, &mut lines)
    } else {
        parse_request(first, &mut lines)
    }
}

fn parse_request<'a, I>(start_line: &str, lines: &mut I) -> Result<Message>
where
    I: Iterator<Item = &'a str>,
{
    let mut parts = start_line.split_whitespace();
    let method = parts.next().ok_or(SipError::Invalid("missing method"))?;
    let uri = parts.next().ok_or(SipError::Invalid("missing uri"))?;
    let _version = parts.next().ok_or(SipError::Invalid("missing version"))?;

    let mut req = Request::new(parse_method(method)?, uri)?;
    parse_headers_and_body(lines, &mut req.headers, &mut req.body)?;
    Ok(Message::Request(req))
}

fn parse_response<'a, I>(start_line: &str, lines: &mut I) -> Result<Message>
where
    I: Iterator<Item = &'a str>,
{
    let mut parts = start_line.split_whitespace();
    let version = parts.next().ok_or(SipError::Invalid("missing version"))?;
    if !version.starts_with("SIP/2.0") {
        return Err(SipError::Invalid("unsupported version"));
    }
    let status: u16 = parts
        .next()
        .ok_or(SipError::Invalid("missing status"))?
        .parse()
        .map_err(|_| SipError::Invalid("status parse"))?;
    let mut reason: SmallString<MAX_REASON_LEN> = SmallString::new();
    for part in parts {
        if !reason.is_empty() {
            reason.push(' ').map_err(|_| SipError::Capacity)?;
        }
        reason.push_str(part).map_err(|_| SipError::Capacity)?;
    }
    let mut resp = Response::new(status, &reason)?;
    parse_headers_and_body(lines, &mut resp.headers, &mut resp.body)?;
    Ok(Message::Response(resp))
}

fn parse_headers_and_body<'a, I>(
    lines: &mut I,
    headers: &mut HeaderList,
    body: &mut SmallString<MAX_BODY_LEN>,
) -> Result<()>
where
    I: Iterator<Item = &'a str>,
{
    for line in lines.by_ref() {
        if line.is_empty() {
            break;
        }
        let mut parts = line.splitn(2, ':');
        let name = parts.next().ok_or(SipError::Invalid("header name"))?;
        let value = parts
            .next()
            .ok_or(SipError::Invalid("header value"))?
            .trim();
        headers.push(Header::new(name, value)?).map_err(|_| SipError::Capacity)?;
    }

    body.clear();
    let remaining: Vec<&str, 8> = lines.collect();
    for (idx, line) in remaining.iter().enumerate() {
        if idx > 0 {
            body.push_str("\r\n").map_err(|_| SipError::Capacity)?;
        }
        body.push_str(line).map_err(|_| SipError::Capacity)?;
    }
    Ok(())
}

fn parse_method(input: &str) -> Result<Method> {
    match input {
        "REGISTER" => Ok(Method::Register),
        "INVITE" => Ok(Method::Invite),
        "ACK" => Ok(Method::Ack),
        "BYE" => Ok(Method::Bye),
        _ => Err(SipError::Invalid("unknown method")),
    }
}

pub fn header_value<'a>(headers: &'a HeaderList, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestChallenge {
    pub realm: SmallString<MAX_HEADER_VALUE>,
    pub nonce: SmallString<MAX_HEADER_VALUE>,
    pub algorithm: SmallString<MAX_HEADER_VALUE>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestCredentials<'a> {
    pub username: &'a str,
    pub password: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationResult {
    Sent,
    Registered(u32),
    AuthRequired,
    Failed(u16),
}

#[derive(Debug)]
pub struct RegistrationTransaction {
    state: RegistrationState,
    cseq: u32,
    call_id: SmallString<MAX_CALL_ID_LEN>,
    from_tag: SmallString<MAX_TAG_LEN>,
    to_tag: SmallString<MAX_TAG_LEN>,
    branch_counter: u32,
    last_expires: u32,
    last_challenge: Option<DigestChallenge>,
}

impl Default for RegistrationTransaction {
    fn default() -> Self {
        Self {
            state: RegistrationState::Unregistered,
            cseq: 0,
            call_id: unique_token::<MAX_CALL_ID_LEN>("reg"),
            from_tag: unique_token::<MAX_TAG_LEN>("from"),
            to_tag: unique_token::<MAX_TAG_LEN>("to"),
            branch_counter: 1,
            last_expires: 3600,
            last_challenge: None,
        }
    }
}

impl RegistrationTransaction {
    pub fn build_register(
        &mut self,
        registrar_uri: &str,
        contact_uri: &str,
        via_host: &str,
        via_port: u16,
        expires: u32,
        auth_header: Option<Header>,
    ) -> Result<Request> {
        if self.state == RegistrationState::Registering {
            return Err(SipError::InvalidState("already registering"));
        }

        self.cseq = self.cseq.wrapping_add(1);
        self.state = RegistrationState::Registering;

        let mut req = Request::new(Method::Register, registrar_uri)?;
        let via = build_via(via_host, via_port, self.next_branch())?;
        let from = build_from(contact_uri, &self.from_tag)?;
        let to = build_to(contact_uri, &self.to_tag)?;

        req.add_header(via)?;
        req.add_header(Header::new("Max-Forwards", "70")?)?;
        req.add_header(from)?;
        req.add_header(to)?;
        req.add_header(Header::new("Call-ID", &self.call_id)?)?;
        req.add_header(Header::new(
            "CSeq",
            &format_cseq(self.cseq, "REGISTER")?,
        )?)?;
        req.add_header(Header::new("Contact", contact_uri)?)?;
        req.add_header(Header::new("Expires", &expires.to_string())?)?;
        if let Some(auth) = auth_header {
            req.add_header(auth)?;
        }
        req.add_header(Header::new("Content-Length", "0")?)?;

        Ok(req)
    }

    pub fn handle_response(&mut self, resp: &Response) -> RegistrationResult {
        match resp.status_code {
            200 => {
                self.state = RegistrationState::Registered;
                let expires = header_value(&resp.headers, "Expires")
                    .and_then(|v| v.parse::<u32>().ok())
                    .unwrap_or(self.last_expires);
                self.last_expires = expires;
                RegistrationResult::Registered(expires)
            }
            401 | 407 => {
                if let Some(chal) = resp
                    .headers
                    .iter()
                    .find(|h| h.name.eq_ignore_ascii_case("WWW-Authenticate"))
                    .and_then(|h| parse_www_authenticate(&h.value).ok())
                {
                    self.last_challenge = Some(chal);
                }
                self.state = RegistrationState::Unregistered;
                RegistrationResult::AuthRequired
            }
            code => {
                self.state = RegistrationState::Error;
                RegistrationResult::Failed(code)
            }
        }
    }

    pub fn state(&self) -> RegistrationState {
        self.state
    }

    pub fn last_expires(&self) -> u32 {
        self.last_expires
    }

    pub fn last_challenge(&self) -> Option<DigestChallenge> {
        self.last_challenge.clone()
    }

    fn next_branch(&mut self) -> SmallString<MAX_BRANCH_LEN> {
        let mut branch = SmallString::<MAX_BRANCH_LEN>::new();
        let counter = self.branch_counter;
        self.branch_counter = self.branch_counter.wrapping_add(1);
        let _ = write!(branch, "z9hG4bK{:08x}", counter);
        branch
    }
}

#[derive(Debug, Default)]
pub struct Dialog {
    state: DialogState,
    call_id: Option<SmallString<MAX_CALL_ID_LEN>>,
    cseq: u32,
}

impl Dialog {
    pub fn start_outgoing(&mut self, target: &str) -> Result<Request> {
        if self.state != DialogState::Idle {
            return Err(SipError::InvalidState("dialog busy"));
        }
        self.state = DialogState::Inviting;
        self.cseq += 1;

        let mut req = Request::new(Method::Invite, target)?;
        let call_id = self.ensure_call_id()?;
        req.add_header(Header::new("Call-ID", &call_id)?)?;
        req.add_header(Header::new("CSeq", &format_cseq(self.cseq, "INVITE")?)?)?;
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
                Some(self.ack_request().ok()?)
            }
            _ => {
                self.state = DialogState::Terminated;
                None
            }
        }
    }

    pub fn bye(&mut self, target: &str) -> Option<Request> {
        if self.state != DialogState::Established {
            return None;
        }
        self.cseq += 1;
        let mut req = Request::new(Method::Bye, target).ok()?;
        let call_id = self.ensure_call_id().ok()?;
        let call_id_header = Header::new("Call-ID", &call_id).ok()?;
        req.add_header(call_id_header).ok()?;
        let cseq_header = Header::new("CSeq", &format_cseq(self.cseq, "BYE").ok()?).ok()?;
        req.add_header(cseq_header).ok()?;
        self.state = DialogState::Terminated;
        Some(req)
    }

    pub fn state(&self) -> DialogState {
        self.state
    }

    fn ack_request(&mut self) -> Result<Request> {
        let mut req = Request::new(Method::Ack, "sip:remote")?;
        let call_id = self.ensure_call_id()?;
        req.add_header(Header::new("Call-ID", &call_id)?)?;
        req.add_header(Header::new("CSeq", &format_cseq(self.cseq, "ACK")?)?)?;
        Ok(req)
    }

    fn ensure_call_id(&mut self) -> Result<SmallString<MAX_HEADER_VALUE>> {
        if let Some(id) = &self.call_id {
            let mut out: SmallString<MAX_HEADER_VALUE> = SmallString::new();
            out.push_str(id).map_err(|_| SipError::Capacity)?;
            return Ok(out);
        }
        let mut id: SmallString<MAX_CALL_ID_LEN> = SmallString::new();
        id.push_str("call-1").map_err(|_| SipError::Capacity)?;
        self.call_id = Some(id.clone());
        let mut out: SmallString<MAX_HEADER_VALUE> = SmallString::new();
        out.push_str(&id).map_err(|_| SipError::Capacity)?;
        Ok(out)
    }
}

#[derive(Debug, Default)]
pub struct SipStack {
    pub registration: RegistrationTransaction,
    pub dialog: Dialog,
}

impl SipStack {
    pub fn register(
        &mut self,
        registrar_uri: &str,
        contact_uri: &str,
        via_host: &str,
        via_port: u16,
        expires: u32,
        auth_header: Option<Header>,
    ) -> Result<Request> {
        self.registration
            .build_register(registrar_uri, contact_uri, via_host, via_port, expires, auth_header)
    }

    pub fn on_register_response(&mut self, resp: &Response) -> RegistrationResult {
        self.registration.handle_response(resp)
    }

    pub fn invite(&mut self, target: &str) -> Result<Request> {
        self.dialog.start_outgoing(target)
    }

    pub fn on_invite_response(&mut self, status: u16) -> Option<Request> {
        self.dialog.handle_final_response(status)
    }

    pub fn bye(&mut self, target: &str) -> Option<Request> {
        self.dialog.bye(target)
    }
}

fn format_cseq(seq: u32, method: &str) -> Result<SmallString<MAX_HEADER_VALUE>> {
    let mut buf: SmallString<MAX_HEADER_VALUE> = SmallString::new();
    write!(buf, "{} {}", seq, method).map_err(|_| SipError::Capacity)?;
    Ok(buf)
}

static GLOBAL_COUNTER: AtomicU32 = AtomicU32::new(1);

fn unique_token<const N: usize>(prefix: &str) -> SmallString<N> {
    let mut token = SmallString::<N>::new();
    let counter = GLOBAL_COUNTER.fetch_add(1, Ordering::Relaxed);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0));
    let _ = write!(token, "{}-{:x}-{:x}", prefix, now.as_secs(), counter);
    token
}

fn build_via(host: &str, port: u16, branch: SmallString<MAX_BRANCH_LEN>) -> Result<Header> {
    let mut value: SmallString<MAX_HEADER_VALUE> = SmallString::new();
    write!(value, "SIP/2.0/UDP {}:{};branch={};rport", host, port, branch)
        .map_err(|_| SipError::Capacity)?;
    Header::new("Via", &value)
}

fn build_from(uri: &str, tag: &str) -> Result<Header> {
    let mut value: SmallString<MAX_HEADER_VALUE> = SmallString::new();
    write!(value, "{};tag={}", uri, tag).map_err(|_| SipError::Capacity)?;
    Header::new("From", &value)
}

fn build_to(uri: &str, tag: &str) -> Result<Header> {
    let mut value: SmallString<MAX_HEADER_VALUE> = SmallString::new();
    write!(value, "{};tag={}", uri, tag).map_err(|_| SipError::Capacity)?;
    Header::new("To", &value)
}

pub fn parse_www_authenticate(input: &str) -> Result<DigestChallenge> {
    let mut parts = input.trim().splitn(2, ' ');
    let scheme = parts.next().ok_or(SipError::Invalid("auth scheme"))?;
    if !scheme.eq_ignore_ascii_case("digest") {
        return Err(SipError::Invalid("auth scheme"));
    }
    let params = parts
        .next()
        .ok_or(SipError::Invalid("auth params"))?;

    let mut realm: Option<SmallString<MAX_HEADER_VALUE>> = None;
    let mut nonce: Option<SmallString<MAX_HEADER_VALUE>> = None;
    let mut algorithm: SmallString<MAX_HEADER_VALUE> = SmallString::new();
    algorithm
        .push_str("MD5")
        .map_err(|_| SipError::Capacity)?;

    for param in params.split(',') {
        let mut kv = param.trim().splitn(2, '=');
        let key = kv
            .next()
            .ok_or(SipError::Invalid("auth key"))?
            .trim();
        let raw_val = kv
            .next()
            .ok_or(SipError::Invalid("auth value"))?
            .trim()
            .trim_matches('"');
        match key.to_ascii_lowercase().as_str() {
            "realm" => {
                let mut v: SmallString<MAX_HEADER_VALUE> = SmallString::new();
                v.push_str(raw_val).map_err(|_| SipError::Capacity)?;
                realm = Some(v);
            }
            "nonce" => {
                let mut v: SmallString<MAX_HEADER_VALUE> = SmallString::new();
                v.push_str(raw_val).map_err(|_| SipError::Capacity)?;
                nonce = Some(v);
            }
            "algorithm" => {
                algorithm.clear();
                algorithm.push_str(raw_val).map_err(|_| SipError::Capacity)?;
            }
            _ => {}
        }
    }

    Ok(DigestChallenge {
        realm: realm.ok_or(SipError::Invalid("realm"))?,
        nonce: nonce.ok_or(SipError::Invalid("nonce"))?,
        algorithm,
    })
}

pub fn authorization_header(
    challenge: &DigestChallenge,
    creds: &DigestCredentials<'_>,
    method: &str,
    uri: &str,
) -> Result<Header> {
    let response = compute_digest_response(challenge, creds, method, uri)?;
    let mut value: SmallString<MAX_HEADER_VALUE> = SmallString::new();
    write!(
        value,
        "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", response=\"{}\", algorithm={}",
        creds.username, challenge.realm, challenge.nonce, uri, response, challenge.algorithm
    )
    .map_err(|_| SipError::Capacity)?;
    Header::new("Authorization", &value)
}

fn compute_digest_response(
    challenge: &DigestChallenge,
    creds: &DigestCredentials<'_>,
    method: &str,
    uri: &str,
) -> Result<SmallString<MAX_HEADER_VALUE>> {
    let mut a1 = SmallString::<MAX_HEADER_VALUE>::new();
    write!(a1, "{}:{}:{}", creds.username, challenge.realm, creds.password)
        .map_err(|_| SipError::Capacity)?;
    let mut a2 = SmallString::<MAX_HEADER_VALUE>::new();
    write!(a2, "{}:{}", method, uri).map_err(|_| SipError::Capacity)?;

    let ha1 = md5_hex(a1.as_bytes());
    let ha2 = md5_hex(a2.as_bytes());

    let mut combo = SmallString::<MAX_HEADER_VALUE>::new();
    write!(combo, "{}:{}:{}", ha1, challenge.nonce, ha2).map_err(|_| SipError::Capacity)?;
    Ok(md5_hex(combo.as_bytes()))
}

fn md5_hex(data: &[u8]) -> SmallString<MAX_HEADER_VALUE> {
    let digest = Md5::digest(data);
    let mut out = SmallString::<MAX_HEADER_VALUE>::new();
    for b in &digest {
        let _ = write!(out, "{:02x}", b);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_request_and_response() {
        let mut req = Request::new(Method::Invite, "sip:100@example.com").unwrap();
        req.add_header(Header::new("Via", "SIP/2.0/UDP 192.0.2.1").unwrap())
            .unwrap();
        let rendered = req.render().unwrap();
        assert!(rendered.starts_with("INVITE sip:100@example.com SIP/2.0"));

        let mut resp = Response::new(200, "OK").unwrap();
        resp.add_header(Header::new("Content-Length", "0").unwrap())
            .unwrap();
        let rendered_resp = resp.render().unwrap();
        assert!(rendered_resp.starts_with("SIP/2.0 200 OK"));
    }

    #[test]
    fn parses_request() {
        let raw = "INVITE sip:100@example.com SIP/2.0\r\nVia: SIP/2.0/UDP host\r\n\r\n";
        let message = parse_message(raw).unwrap();
        match message {
            Message::Request(r) => assert_eq!(r.method, Method::Invite),
            _ => panic!("expected request"),
        }
    }

    #[test]
    fn registration_flow() {
        let mut reg = RegistrationTransaction::default();
        let req = reg
            .build_register(
                "sip:user@example.com",
                "sip:user@example.com",
                "192.0.2.1",
                5060,
                120,
                None,
            )
            .unwrap();
        assert_eq!(req.method, Method::Register);
        let mut resp = Response::new(200, "OK").unwrap();
        resp.add_header(Header::new("Expires", "120").unwrap())
            .unwrap();
        reg.handle_response(&resp);
        assert_eq!(reg.state(), RegistrationState::Registered);
    }

    #[test]
    fn dialog_flow() {
        let mut dialog = Dialog::default();
        let invite = dialog.start_outgoing("sip:200@example.com").unwrap();
        assert_eq!(invite.method, Method::Invite);
        let ack = dialog.handle_final_response(200).unwrap();
        assert_eq!(dialog.state(), DialogState::Established);
        assert_eq!(ack.method, Method::Ack);
        let bye = dialog.bye("sip:200@example.com").unwrap();
        assert_eq!(bye.method, Method::Bye);
    }

    #[test]
    fn register_builder_adds_headers() {
        let mut reg = RegistrationTransaction::default();
        let req = reg
            .build_register(
                "sip:registrar@example.com",
                "sip:user@192.0.2.1:5060",
                "192.0.2.1",
                5060,
                300,
                None,
            )
            .unwrap();
        let rendered = req.render().unwrap();
        assert!(rendered.contains("Via: SIP/2.0/UDP 192.0.2.1:5060;branch=z9hG4bK"));
        assert!(rendered.contains("From: sip:user@192.0.2.1:5060;tag="));
        assert!(rendered.contains("To: sip:user@192.0.2.1:5060;tag="));
        assert!(rendered.contains("Contact: sip:user@192.0.2.1:5060"));
        assert!(rendered.contains("CSeq: 1 REGISTER"));
        assert!(rendered.contains("Expires: 300"));
    }

    #[test]
    fn digest_auth_header_matches_reference() {
        let challenge = parse_www_authenticate(
            r#"Digest realm="testrealm@host.com", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", algorithm=MD5"#,
        )
        .unwrap();
        let creds = DigestCredentials {
            username: "Mufasa",
            password: "Circle Of Life",
        };
        let header = authorization_header(&challenge, &creds, "GET", "/dir/index.html").unwrap();
        assert!(
            header
                .value
                .contains("response=\"670fd8c2df070c60b045671b8b24ff02\""),
            "unexpected header: {}",
            header.value
        );
    }

    #[test]
    fn md5_round_trip_reference() {
        let digest = md5_hex(b"abc");
        assert_eq!(digest.as_str(), "900150983cd24fb0d6963f7d28e17f72");
    }
}

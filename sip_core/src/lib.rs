use core::fmt::Write;
use heapless::{String, Vec};
use thiserror::Error;

const MAX_URI_LEN: usize = 96;
const MAX_HEADER_NAME: usize = 32;
const MAX_HEADER_VALUE: usize = 160;
const MAX_REASON_LEN: usize = 64;
const MAX_BODY_LEN: usize = 256;
const MAX_HEADERS: usize = 16;

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

#[derive(Debug, Default)]
pub struct RegistrationTransaction {
    state: RegistrationState,
    cseq: u32,
}

impl RegistrationTransaction {
    pub fn start(&mut self, uri: &str, contact: &str) -> Result<Request> {
        if self.state == RegistrationState::Registering {
            return Err(SipError::InvalidState("already registering"));
        }
        self.state = RegistrationState::Registering;
        self.cseq += 1;

        let mut req = Request::new(Method::Register, uri)?;
        req.add_header(Header::new("To", uri)?)?;
        req.add_header(Header::new("From", contact)?)?;
        req.add_header(Header::new("Call-ID", "1")?)?;
        req.add_header(Header::new("CSeq", &format_cseq(self.cseq, "REGISTER")?)?)?;
        Ok(req)
    }

    pub fn handle_response(&mut self, status: u16) {
        if status == 200 {
            self.state = RegistrationState::Registered;
        } else {
            self.state = RegistrationState::Unregistered;
        }
    }

    pub fn state(&self) -> RegistrationState {
        self.state
    }
}

#[derive(Debug, Default)]
pub struct Dialog {
    state: DialogState,
    call_id: Option<SmallString<MAX_HEADER_VALUE>>,
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
            return Ok(id.clone());
        }
        let mut id: SmallString<MAX_HEADER_VALUE> = SmallString::new();
        id.push_str("call-1").map_err(|_| SipError::Capacity)?;
        self.call_id = Some(id.clone());
        Ok(id)
    }
}

#[derive(Debug, Default)]
pub struct SipStack {
    pub registration: RegistrationTransaction,
    pub dialog: Dialog,
}

impl SipStack {
    pub fn register(&mut self, uri: &str, contact: &str) -> Result<Request> {
        self.registration.start(uri, contact)
    }

    pub fn on_register_response(&mut self, status: u16) {
        self.registration.handle_response(status);
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
        let req = reg.start("sip:user@example.com", "sip:user@example.com").unwrap();
        assert_eq!(req.method, Method::Register);
        reg.handle_response(200);
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
}

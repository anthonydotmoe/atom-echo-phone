use core::fmt::Write;
use heapless::{String, Vec};

use crate::{Result, SipError};

pub const MAX_URI_LEN: usize = 32;
pub const MAX_HEADER_NAME: usize = 32;
pub const MAX_HEADER_VALUE: usize = 128;
pub const MAX_REASON_LEN: usize = 64;
pub const MAX_BODY_LEN: usize = 1024;
pub const MAX_HEADERS: usize = 16;
pub const MAX_TAG_LEN: usize = 32;
pub const MAX_CALL_ID_LEN: usize = 64;

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

impl Header {
    pub fn new(name: &str, value: &str) -> Result<Self> {
        let mut name_buf: SmallString<MAX_HEADER_NAME> = SmallString::new();
        name_buf.push_str(name).map_err(|_| SipError::Capacity)?;
        let mut value_buf: SmallString<MAX_HEADER_VALUE> = SmallString::new();
        value_buf.push_str(value).map_err(|_| SipError::Capacity)?;
        Ok(Header {
            name: name_buf,
            value: value_buf,
        })
    }
}

impl Request {
    pub fn new(method: Method, uri: &str) -> Result<Self> {
        let mut uri_buf: SmallString<MAX_URI_LEN> = SmallString::new();
        uri_buf.push_str(uri).map_err(|_| SipError::Capacity)?;

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
        self.body.clear();
        self.body.push_str(body).map_err(|_| SipError::Capacity)
    }

    pub fn render<const N: usize>(&self) -> Result<SmallString<N>> {
        let mut out: SmallString<N> = SmallString::new();
        write!(
            out,
            "{} {} SIP/{}.{}\r\n",
            self.method, self.uri, self.version.major, self.version.minor
        )
        .map_err(|_| SipError::Capacity)?;
        for header in &self.headers {
            write!(out, "{}: {}\r\n", header.name, header.value)
                .map_err(|_| SipError::Capacity)?;
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

    pub fn set_body(&mut self, body: &str) -> Result<()> {
        self.body.clear();
        self.body.push_str(body).map_err(|_| SipError::Capacity)
    }

    pub fn render<const N: usize>(&self) -> Result<SmallString<N>> {
        let mut out: SmallString<N> = SmallString::new();
        write!(
            out,
            "SIP/{}.{} {} {}\r\n",
            self.version.major, self.version.minor, self.status_code, self.reason
        )
        .map_err(|_| SipError::Capacity)?;

        for header in &self.headers {
            write!(out, "{}: {}\r\n", header.name, header.value)
                .map_err(|_| SipError::Capacity)?;
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

// Basic parser: decide request vs response by first line.
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
    // Headers
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
        headers
            .push(Header::new(name, value)?)
            .map_err(|_| SipError::Capacity)?;
    }

    // Body
    body.clear();
    let mut first = true;
    for line in lines {
        if !first {
            body.push_str("\r\n").map_err(|_| SipError::Capacity)?;
        }
        first = false;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_request_and_response() {
        let mut req = Request::new(Method::Invite, "sip:100@example.com").unwrap();
        req.add_header(Header::new("Via", "SIP/2.0/UDP 192.0.2.1").unwrap())
            .unwrap();
        let rendered: SmallString<MAX_BODY_LEN> = req.render().unwrap();
        assert!(rendered.starts_with("INVITE sip:100@example.com SIP/2.0"));

        let mut resp = Response::new(200, "OK").unwrap();
        resp.add_header(Header::new("Content-Length", "0").unwrap())
            .unwrap();
        let rendered_resp: SmallString<MAX_BODY_LEN> = resp.render().unwrap();
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
}
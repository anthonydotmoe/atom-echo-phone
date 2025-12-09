use core::fmt::Write;

use crate::{
    Result, SipError, auth::DigestChallenge, header_value, message::{Header, Request}
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RegistrationState {
    #[default]
    Unregistered,
    Registering,
    Registered,
    Error,
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
    call_id: String,
    from_tag: String,
    to_tag: String,
    branch_counter: u32,
    last_expires: u32,
    last_challenge: Option<DigestChallenge>,
}

impl Default for RegistrationTransaction {
    fn default() -> Self {
        Self {
            state: RegistrationState::Unregistered,
            cseq: 0,
            call_id: simple_token("reg", 1),
            from_tag: simple_token("from", 1),
            to_tag: simple_token("to", 1),
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

        let mut req = Request::new(crate::message::Method::Register, registrar_uri)?;
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

    pub fn handle_response(&mut self, resp: &crate::message::Response) -> RegistrationResult {
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
                    .and_then(|h| crate::auth::parse_www_authenticate(&h.value).ok())
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

    pub fn next_branch(&mut self) -> String {
        let mut branch = String::new();
        let counter = self.branch_counter;
        self.branch_counter = self.branch_counter.wrapping_add(1);
        let _ = write!(branch, "z9hG4bK{:08x}", counter);
        branch
    }

    pub fn next_refresh_interval_secs(&self) -> u64 {
        let expires = self.last_expires.max(5);
        (expires as u64 * 8) / 10
    }
}

fn simple_token(prefix: &str, counter: u32) -> String {
    let mut token = String::new();
    let _ = write!(token, "{}-{:x}", prefix, counter);
    token
}

fn build_via(
    host: &str,
    port: u16,
    branch: String,
) -> Result<Header> {
    let mut value = String::new();
    write!(value, "SIP/2.0/UDP {}:{};branch={};rport", host, port, branch)
        .map_err(|_| SipError::Capacity)?;
    Header::new("Via", &value)
}

fn build_from(uri: &str, tag: &str) -> Result<Header> {
    let mut value = String::new();
    write!(value, "{};tag={}", uri, tag).map_err(|_| SipError::Capacity)?;
    Header::new("From", &value)
}

fn build_to(uri: &str, tag: &str) -> Result<Header> {
    let mut value = String::new();
    write!(value, "{};tag={}", uri, tag).map_err(|_| SipError::Capacity)?;
    Header::new("To", &value)
}

fn format_cseq(seq: u32, method: &str) -> Result<String> {
    let mut buf = String::new();
    write!(buf, "{} {}", seq, method).map_err(|_| SipError::Capacity)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Method, Response
    };

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
        resp.add_header(Header::new("Expires", "120").unwrap());
        reg.handle_response(&resp);
        assert_eq!(reg.state(), RegistrationState::Registered);
    }
}

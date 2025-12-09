use core::fmt::Write;

use md5::Digest;

use crate::{
    Header, Result, SipError,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestChallenge {
    pub realm: String,
    pub nonce: String,
    pub algorithm: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestCredentials<'a> {
    pub username: &'a str,
    pub password: &'a str,
}

pub fn parse_www_authenticate(input: &str) -> Result<DigestChallenge> {
    let mut parts = input.trim().splitn(2, ' ');
    let scheme = parts.next().ok_or(SipError::Invalid("auth scheme"))?;
    if !scheme.eq_ignore_ascii_case("digest") {
        return Err(SipError::Invalid("auth scheme"));
    }
    let params = parts.next().ok_or(SipError::Invalid("auth params"))?;

    let mut realm: Option<String> = None;
    let mut nonce: Option<String> = None;
    let mut algorithm = String::new();
    algorithm.push_str("MD5");

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
                let mut v = String::new();
                v.push_str(raw_val);
                realm = Some(v);
            }
            "nonce" => {
                let mut v = String::new();
                v.push_str(raw_val);
                nonce = Some(v);
            }
            "algorithm" => {
                algorithm.clear();
                algorithm.push_str(raw_val);
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
    let mut value = String::new();
    write!(
        value,
        "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", response=\"{}\", algorithm=\"{}\"",
        creds.username, challenge.realm, challenge.nonce, uri, response, challenge.algorithm
    )
        .map_err(|_| SipError::Capacity)?;

    Header::new("Authorization", &value)
}

pub fn compute_digest_response(
    challenge: &DigestChallenge,
    creds: &DigestCredentials<'_>,
    method: &str,
    uri: &str,
) -> Result<String> {
    let mut a1 = String::new();
    write!(a1, "{}:{}:{}", creds.username, challenge.realm, creds.password)
        .map_err(|_| SipError::Capacity)?;
    let mut a2 = String::new();
    write!(a2, "{}:{}", method, uri)
        .map_err(|_| SipError::Capacity)?;

    let ha1 = md5_hex(a1.as_bytes());
    let ha2 = md5_hex(a2.as_bytes());

    let mut combo = String::new();
    write!(combo, "{}:{}:{}", ha1, challenge.nonce, ha2)
        .map_err(|_| SipError::Capacity)?;

    Ok(md5_hex(combo.as_bytes()))
}

fn md5_hex(data: &[u8]) -> String {
    let digest = md5::Md5::digest(data);
    let mut out = String::new();
    for b in &digest {
        let _ = write!(out, "{:02x}", b);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
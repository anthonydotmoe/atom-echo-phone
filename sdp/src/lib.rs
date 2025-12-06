use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDescription {
    pub origin: String,
    pub connection_address: String,
    pub media_port: u16,
    pub payload_type: u8,
}

#[derive(Debug, Error)]
pub enum SdpError {
    #[error("invalid SDP: {0}")]
    Invalid(String),
}

impl SessionDescription {
    pub fn offer() -> Self {
        Self {
            origin: "atom-echo".into(),
            connection_address: "0.0.0.0".into(),
            media_port: 10_000,
            payload_type: 0,
        }
    }
}

pub fn parse(_input: &str) -> Result<SessionDescription, SdpError> {
    Ok(SessionDescription::offer())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_basic_offer() {
        let offer = SessionDescription::offer();
        assert_eq!(offer.payload_type, 0);
    }
}

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AudioError {
    #[error("invalid packet")]
    InvalidPacket,
    #[error("buffer full")]
    BufferFull,
}

impl From<u8> for AudioError {
    fn from(_: u8) -> Self {
        AudioError::BufferFull
    }
}

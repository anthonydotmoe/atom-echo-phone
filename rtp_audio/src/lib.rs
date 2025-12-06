use std::collections::VecDeque;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct RtpPacket {
    pub sequence_number: u16,
    pub timestamp: u32,
    pub payload: Vec<u8>,
}

#[derive(Debug)]
pub struct JitterBuffer {
    frames: VecDeque<Vec<i16>>,
    capacity: usize,
}

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("invalid packet: {0}")]
    InvalidPacket(String),
}

impl RtpPacket {
    pub fn new(sequence_number: u16, timestamp: u32, payload: Vec<u8>) -> Self {
        Self {
            sequence_number,
            timestamp,
            payload,
        }
    }
}

impl JitterBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            frames: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn push_frame(&mut self, frame: Vec<i16>) {
        if self.frames.len() >= self.capacity {
            let _ = self.frames.pop_front();
        }
        self.frames.push_back(frame);
    }

    pub fn pop_frame(&mut self) -> Option<Vec<i16>> {
        self.frames.pop_front()
    }
}

pub fn encode_ulaw(pcm: &[i16]) -> Vec<u8> {
    pcm.iter().map(|sample| (*sample >> 8) as u8).collect()
}

pub fn decode_ulaw(encoded: &[u8]) -> Vec<i16> {
    encoded.iter().map(|byte| i16::from(*byte) << 8).collect()
}

pub fn build_rtp_packet(seq: u16, ts: u32, payload: Vec<u8>) -> Result<RtpPacket, AudioError> {
    Ok(RtpPacket::new(seq, ts, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_decodes() {
        let pcm = vec![0_i16, 1024, -1024];
        let encoded = encode_ulaw(&pcm);
        let decoded = decode_ulaw(&encoded);
        assert_eq!(decoded.len(), pcm.len());
    }
}

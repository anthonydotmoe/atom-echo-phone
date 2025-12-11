use heapless::Vec;
use thiserror::Error;

const ULAW_BIAS: i32 = 0x84;
const ULAW_CLIP: i32 = 32635;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpHeader {
    pub version: u8,
    pub padding: bool,
    pub extension: bool,
    pub csrc_count: u8,
    pub marker: bool,
    pub payload_type: u8,
    pub sequence_number: u16,
    pub timestamp: u32,
    pub ssrc: u32,
}

impl Default for RtpHeader {
    fn default() -> Self {
        Self {
            version: 2,
            padding: false,
            extension: false,
            csrc_count: 0,
            marker: false,
            payload_type: 0,
            sequence_number: 0,
            timestamp: 0,
            ssrc: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtpPacket<const N: usize> {
    pub header: RtpHeader,
    pub payload: Vec<u8, N>,
}

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

impl<const N: usize> RtpPacket<N> {
    pub fn new(header: RtpHeader, payload: Vec<u8, N>) -> Self {
        Self { header, payload }
    }

    pub fn pack(&self) -> Result<Vec<u8, 524>, AudioError> {
        let mut out: Vec<u8, 524> = Vec::new();
        let b0 = (self.header.version & 0b11) << 6
            | ((self.header.padding as u8) << 5)
            | ((self.header.extension as u8) << 4)
            | (self.header.csrc_count & 0x0f);
        let b1 = ((self.header.marker as u8) << 7)
            | (self.header.payload_type & 0x7f);

        let header_bytes = [
            b0,
            b1,
            (self.header.sequence_number >> 8) as u8,
            (self.header.sequence_number & 0xff) as u8,
            (self.header.timestamp >> 24) as u8,
            ((self.header.timestamp >> 16) & 0xff) as u8,
            ((self.header.timestamp >> 8) & 0xff) as u8,
            (self.header.timestamp & 0xff) as u8,
            (self.header.ssrc >> 24) as u8,
            ((self.header.ssrc >> 16) & 0xff) as u8,
            ((self.header.ssrc >> 8) & 0xff) as u8,
            (self.header.ssrc & 0xff) as u8,
        ];

        for &b in &header_bytes {
            out.push(b)?;
        }

        for &b in &self.payload {
            out.push(b)?;
        }

        Ok(out)
    }

    pub fn unpack(bytes: &[u8]) -> Result<Self, AudioError> {
        if bytes.len() < 12 {
            return Err(AudioError::InvalidPacket);
        }
        let b0 = bytes[0];
        let b1 = bytes[1];
        let header = RtpHeader {
            version: b0 >> 6,
            padding: (b0 & 0x20) != 0,
            extension: (b0 & 0x10) != 0,
            csrc_count: b0 & 0x0f,
            marker: (b1 & 0x80) != 0,
            payload_type: b1 & 0x7f,
            sequence_number: u16::from_be_bytes([bytes[2], bytes[3]]),
            timestamp: u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            ssrc: u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
        };
        let mut payload: Vec<u8, N> = Vec::new();
        for b in &bytes[12..] {
            payload.push(*b)?;
        }
        Ok(Self { header, payload })
    }
}

pub fn encode_ulaw(samples: &[i16]) -> Vec<u8, 512> {
    let mut out: Vec<u8, 512> = Vec::new();
    for &s in samples {
        let clamped = s.clamp(-ULAW_CLIP as i16, ULAW_CLIP as i16);
        let sign = ((clamped >> 8) & 0x80) as u8;
        let magnitude = if clamped < 0 {
            (!clamped as i32) + ULAW_BIAS
        } else {
            (clamped as i32) + ULAW_BIAS
        };

        let mut exponent = 7;
        for exp in 0..7 {
            if magnitude <= (0x1F << (exp + 3)) {
                exponent = exp;
                break;
            }
        }

        let mantissa = (magnitude >> (exponent + 3)) & 0x0F;
        let ulaw_byte = !(sign | ((exponent as u8) << 4) | (mantissa as u8));
        let _ = out.push(ulaw_byte);
    }
    out
}

#[cfg(feature = "table_decode")]
const ULAW_DECODE_TABLE: [i16; 256] = {
    let mut t = [0i16; 256];
    let mut i = 0;
    while i < 256 {
        let b = i as u8;
        let byte = !b;
        let sign = (byte & 0x80) != 0;
        let exponent = (byte >> 4) & 0x07;
        let mantissa = byte & 0x0F;

        let mut magnitude = ((mantissa as i32) << 3) + ULAW_BIAS;
        magnitude <<= exponent as i32;
        magnitude -= ULAW_BIAS;
        let sample = if sign { -magnitude } else { magnitude } as i16;

        t[i] = sample;
        i += 1;
    }
    t
};

#[cfg(feature = "table_decode")]
pub fn decode_ulaw(bytes: &[u8]) -> Vec<i16, 512> {
    let mut out: Vec<i16, 512> = Vec::new();
    for &b in bytes {
        let sample = ULAW_DECODE_TABLE[b as usize];
        let _ = out.push(sample);
    }
    out
}

#[cfg(not(feature = "table_decode"))]
pub fn compute_decode_ulaw(bytes: &[u8]) -> Vec<i16, 512> {
    let mut out: Vec<i16, 512> = Vec::new();
    for &b in bytes {
        let byte = !b as u8;
        let sign = (byte & 0x80) != 0;
        let exponent = (byte >> 4) & 0x07;
        let mantissa = byte & 0x0F;

        let mut magnitude = ((mantissa as i32) << 3) + ULAW_BIAS;
        magnitude <<= exponent as i32;
        magnitude -= ULAW_BIAS;
        let sample = if sign {
            -magnitude
        } else {
            magnitude
        } as i16;
        let _ = out.push(sample);
    }
    out
}

#[derive(Debug, Clone)]
pub struct JitterFrame<const FRAME: usize> {
    pub seq: u16,
    pub samples: Vec<i16, FRAME>,
}

#[derive(Debug)]
pub struct JitterBuffer<const CAP: usize, const FRAME: usize> {
    next_seq: Option<u16>,
    frames: Vec<JitterFrame<FRAME>, CAP>,
}

impl<const CAP: usize, const FRAME: usize> JitterBuffer<CAP, FRAME> {
    pub fn new() -> Self {
        Self {
            next_seq: None,
            frames: Vec::new(),
        }
    }

    pub fn push_frame(&mut self, seq: u16, samples: &[i16]) {
        if self.frames.is_full() {
            let _ = self.frames.remove(0);
        }

        if self.frames.iter().any(|f| f.seq == seq) {
            return;
        }

        let mut buf: Vec<i16, FRAME> = Vec::new();
        for s in samples.iter().copied().take(FRAME) {
            let _ = buf.push(s);
        }
        while buf.len() < FRAME {
            let _ = buf.push(0);
        }

        let _ = self.frames.push(JitterFrame { seq, samples: buf });
    }

    pub fn pop_frame(&mut self) -> (Vec<i16, FRAME>, bool) {
        if self.next_seq.is_none() {
            if let Some(min_seq) = self.frames.iter().map(|f| f.seq).min() {
                self.next_seq = Some(min_seq);
            }
        }

        let expected = match self.next_seq {
            Some(s) => s,
            None => return (silence_frame::<FRAME>(), false),
        };

        if let Some(pos) = self.frames.iter().position(|f| f.seq == expected) {
            let frame = self.frames.remove(pos);
            self.next_seq = Some(expected.wrapping_add(1));
            return (frame.samples, true);
        }

        self.next_seq = Some(expected.wrapping_add(1));
        (silence_frame::<FRAME>(), false)
    }
}

fn silence_frame<const FRAME: usize>() -> Vec<i16, FRAME> {
    let mut buf: Vec<i16, FRAME> = Vec::new();
    for _ in 0..FRAME {
        let _ = buf.push(0);
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulaw_round_trip_stable() {
        let samples = [0_i16, 1000, -1000, 12345, -20000];
        let encoded = encode_ulaw(&samples);
        let decoded = decode_ulaw(&encoded);
        let reencoded = encode_ulaw(&decoded);
        assert_eq!(encoded, reencoded);
    }

    #[test]
    fn header_pack_unpack() {
        let header = RtpHeader {
            version: 2,
            padding: false,
            extension: false,
            csrc_count: 0,
            marker: true,
            payload_type: 0,
            sequence_number: 42,
            timestamp: 160,
            ssrc: 0x11223344,
        };
        let packet: RtpPacket<4> = RtpPacket {
            header,
            payload: Vec::from_slice(&[1, 2, 3, 4]).unwrap(),
        };
        let bytes = packet.pack().unwrap();
        let unpacked: RtpPacket<4> = RtpPacket::unpack(&bytes).unwrap();
        assert_eq!(unpacked.header, header);
        assert_eq!(unpacked.payload, packet.payload);
    }

    #[test]
    fn jitter_buffer_reordering() {
        let mut jb: JitterBuffer<4, 4> = JitterBuffer::new();
        jb.push_frame(2, &[20, 21, 22, 23]);
        jb.push_frame(1, &[10, 11, 12, 13]);

        let (f1, ok1) = jb.pop_frame();
        assert!(ok1);
        assert_eq!(f1[..], [10, 11, 12, 13]);

        let (f2, ok2) = jb.pop_frame();
        assert!(ok2);
        assert_eq!(f2[..], [20, 21, 22, 23]);

        let (f3, ok3) = jb.pop_frame();
        assert!(!ok3);
        assert_eq!(f3, silence_frame::<4>());
    }

    #[test]
    fn jitter_buffer_drops_and_underflow() {
        let mut jb: JitterBuffer<3, 3> = JitterBuffer::new();
        jb.push_frame(5, &[1, 2, 3]);

        let (f1, ok1) = jb.pop_frame();
        assert!(ok1);
        assert_eq!(f1[..], [1, 2, 3]);

        let (f2, ok2) = jb.pop_frame();
        assert!(!ok2);
        assert_eq!(f2, silence_frame::<3>());

        let (f3, ok3) = jb.pop_frame();
        assert!(!ok3);
        assert_eq!(f3, silence_frame::<3>());
    }
}

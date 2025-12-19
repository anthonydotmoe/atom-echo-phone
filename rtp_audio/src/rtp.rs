use heapless::Vec;

use crate::error::AudioError;

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
        let b1 = ((self.header.marker as u8) << 7) | (self.header.payload_type & 0x7f);

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
        for &b in &bytes[12..] {
            payload.push(b)?;
        }

        Ok(Self { header, payload })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

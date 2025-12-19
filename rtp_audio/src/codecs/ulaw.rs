use heapless::Vec;

const ULAW_BIAS: i32 = 0x84;
const ULAW_CLIP: i32 = 32635;

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

        let mut exponent: u8 = 0;
        let mut tmp = (magnitude >> 7) as i32;
        while tmp > 1 && exponent < 7 {
            tmp >>= 1;
            exponent += 1;
        }

        let mantissa = (magnitude >> (exponent + 3)) & 0x0F;
        let ulaw_byte = !(sign | ((exponent as u8) << 4) | (mantissa as u8));
        let _ = out.push(ulaw_byte);
    }

    out
}

pub fn decode_ulaw(bytes: &[u8]) -> Vec<i16, 512> {
    #[cfg(feature = "table_decode")]
    {
        decode_ulaw_table(bytes)
    }
    #[cfg(not(feature = "table_decode"))]
    {
        compute_decode_ulaw(bytes)
    }
}

#[cfg(feature = "table_decode")]
fn decode_ulaw_table(bytes: &[u8]) -> Vec<i16, 512> {
    let mut out: Vec<i16, 512> = Vec::new();
    for &b in bytes {
        let sample = ULAW_DECODE_TABLE[b as usize];
        let _ = out.push(sample);
    }
    out
}

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

        let sample = if sign { -magnitude } else { magnitude } as i16;
        let _ = out.push(sample);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulaw_all_codes_round_trip_preserves_pcm() {
        for b in 0u16..=255 {
            let b = b as u8;

            let pcm1 = decode_ulaw(&[b])[0];
            let b2 = encode_ulaw(&[pcm1])[0];
            let pcm2 = decode_ulaw(&[b2])[0];

            assert_eq!(pcm2, pcm1, "byte 0x{b:02x} changed PCM");
        }
    }

    #[test]
    fn ulaw_special_values() {
        assert_eq!(decode_ulaw(&[0xFF])[0], 0);
        assert_eq!(decode_ulaw(&[0x7F])[0], 0); // the other zero code
        assert_eq!(encode_ulaw(&[0])[0], 0xFF); // encode canonicalizes to 0xFF
    }

    #[test]
    fn ulaw_table_and_compute_decode_match_for_all_codes() {
        for b in 0u16..=255 {
            let b = b as u8;
            let a = compute_decode_ulaw(&[b])[0];

            #[cfg(feature = "table_decode")]
            {
                let t = decode_ulaw(&[b])[0];
                assert_eq!(a, t, "mismatch at byte 0x{b:02x}");
            }

            #[cfg(not(feature = "table_decode"))]
            {
                // decode_ulaw calls compute path anyway, but keep symmetry in the test.
                let t = decode_ulaw(&[b])[0];
                assert_eq!(a, t, "mismatch at byte 0x{b:02x}");
            }
        }
    }
}

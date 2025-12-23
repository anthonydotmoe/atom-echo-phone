include!(concat!(env!("OUT_DIR"), "/polyphase_h.rs"));

const FRAME_SAMPLES_48K: usize = 960;

pub struct Up6Polyphase {
    hist: [i16; TAPS_PER_PHASE],
}

impl Up6Polyphase {
    pub fn new() -> Self { Self { hist: [0; TAPS_PER_PHASE] } }

    #[inline]
    pub fn push_sample(&mut self, x: i16) {
        // shift history
        // TODO: Maybe replace with a ring buffer?
        self.hist.copy_within(0..TAPS_PER_PHASE-1, 1);
        self.hist[0] = x;
    }

    pub fn process_frame(&mut self, in8k: &[i16; 160], out48k: &mut [i16; FRAME_SAMPLES_48K]) {
        // Push all new input into a larger working buffer:
        // simplist approach: push one sample, immediately generate its 6 outputs.

        let mut out_i = 0;
        for &x in in8k.iter() {
            self.push_sample(x);

            for phase in 0..UPSAMPLE {
                // dot = sum hist[t] * H[phase][t]
                let mut acc: i32 = 0;
                for t in 0..TAPS_PER_PHASE {
                    acc += (self.hist[t] as i32) * (H[phase][t] as i32);
                }
                // Q15 -> i16
                out48k[out_i] = (acc >> 15).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                out_i += 1;
            }
        }
        debug_assert_eq!(out_i , FRAME_SAMPLES_48K);
    }
}

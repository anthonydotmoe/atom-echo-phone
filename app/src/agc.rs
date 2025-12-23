pub struct Agc {
    // Gain in Q12 (1.0 = 4096)
    gain_q12: i32,

    // Config
    target_rms: i32,        // e.g. 9000..12000
    noise_gate_rms: i32,    // below this, do not increase gain
    max_gain_q12: i32,      // e.g. 4.0x = 16384
    min_gain_q12: i32,      // e.g. 0.5x = 2048

    // Smoothing (0..255). Higher = faster response.
    // attack: how fast we decrease gain (loud suddenly)
    // release: how fast we increase gain (quiet)
    attack: u8,
    release: u8,

    // Optional limiter threshold in i16 domain (absolute)
    limiter_thresh: i16,    // e.g. 28000
}

const START_GAIN: i32 = f32_to_q12(3.0);
const MAX_GAIN: i32   = f32_to_q12(32.0);
const MIN_GAIN: i32   = f32_to_q12(0.5);

impl Agc {
    pub fn new() -> Self {
        Self {
            gain_q12: START_GAIN,
            target_rms: 16000,
            noise_gate_rms: 150,
            max_gain_q12: MAX_GAIN,
            min_gain_q12: MIN_GAIN,
            attack: 96,
            release: 16,
            limiter_thresh: 28500,
        }
    }

    pub fn set_target_rms(&mut self, target: i32) { self.target_rms = target; }
    pub fn set_noise_gate_rms(&mut self, gate: i32) { self.noise_gate_rms = gate; }
    pub fn set_max_gain(&mut self, max_gain_q12: i32) { self.max_gain_q12 = max_gain_q12; }
    pub fn set_attack_release(&mut self, attack: u8, release: u8) {
        self.attack = attack;
        self.release = release;
    }

    /// Process a frame in-place.
    /// Returns the applied gain_q12 and measured rms for logging/tuning.
    pub fn process_frame(&mut self, frame: &mut [i16]) -> (i32, i32) {
        let rms = frame_rms_i16(frame);

        // Compute desired gain in Q12.
        // desired_gain = target_rms / rms
        // => desired_gain_q12 = (target_rms << 12) / rms
        let rms_i64 = rms as i64;
        let mut desired_gain_q12 = if rms_i64 > 0 {
            (((self.target_rms as i64) << 12) / rms_i64) as i32
        } else {
            self.max_gain_q12
        };

        // Clamp desired gain
        desired_gain_q12 = desired_gain_q12
            .clamp(self.min_gain_q12, self.max_gain_q12);

        // Noise gate: do not increase gain when rms is below gate.
        if rms < self.noise_gate_rms && desired_gain_q12 > self.gain_q12 {
            desired_gain_q12 = self.gain_q12;
        }

        // Smooth gain (EMA-like in Q12, using 0..255 alpha)
        let alpha = if desired_gain_q12 < self.gain_q12 {
            self.attack
        } else {
            self.release
        } as i32;

        // gain += alpha/256 * (desired - gain)
        let delta = desired_gain_q12 - self.gain_q12;
        self.gain_q12 += (delta * alpha) >> 8;

        // Apply gain + limiter
        apply_gain_q12_with_limiter(frame, self.gain_q12, self.limiter_thresh);

        (self.gain_q12, rms)
    }
}

#[inline]
fn frame_rms_i16(frame: &[i16]) -> i32 {
    // RMS = sqrt(mean(x^2))
    // Use 64-bit accumulator to avoid overflow: sum(x*x) up to ~160 * (32767^2) ~ 1.7e11
    let mut sum: i64 = 0;
    for &s in frame {
        let x = s as i32;
        sum += (x as i64) * (x as i64);
    }

    let mean = (sum / frame.len() as i64) as u32;
    isqrt_u32(mean) as i32
}

#[inline]
fn apply_gain_q12_with_limiter(frame: &mut [i16], gain_q12: i32, thresh: i16) {
    let thresh_i32 = thresh as i32;
    let neg_thresh_i32 = -(thresh_i32);

    for s in frame {
        let x = *s as i32;

        // Apply gain in Q12
        let mut y = (x * gain_q12) >> 12;

        // Simple soft-ish limiter: if above threshold, compress the excess by 4:1.
        // This avoids brutal hard clipping on yells.
        if y > thresh_i32 {
            let excess = y - thresh_i32;
            y = thresh_i32 + (excess >> 2);
        } else if y < neg_thresh_i32 {
            let excess = y - neg_thresh_i32; // negative
            y = neg_thresh_i32 + (excess >> 2);
        }

        // Final clamp to i16
        *s = y.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    }
}

#[inline]
fn isqrt_u32(mut n: u32) -> u32 {
    // Integer sqrt via binary method
    let mut x: u32 = 0;
    let mut bit: u32 = 1 << 30; // The second-to-top bit
    while bit > n { bit >>= 2; }

    while bit != 0 {
        if n >= x + bit {
            n -= x + bit;
            x = (x >> 1) + bit;
        } else {
            x >>= 1;
        }
        bit >>= 2;
    }
    x
}

const fn f32_to_q12(n: f32) -> i32 {
    (n * 4096.0) as i32
}

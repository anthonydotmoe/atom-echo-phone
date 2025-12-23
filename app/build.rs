use std::{env, fs, path::PathBuf};

fn sinc(x: f64) -> f64 {
    if x == 0.0 { 1.0 } else { (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x) }
}

fn blackman(n: usize, taps: usize) -> f64 {
    let a0 = 0.42;
    let a1 = 0.5;
    let a2 = 0.08;
    let nn = n as f64;
    let m = (taps - 1) as f64;
    let w = 2.0 * std::f64::consts::PI * nn / m;
    a0 - a1 * w.cos() + a2 * (2.0 * w).cos()
}

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("espidf") {
        embuild::espidf::sysenv::output();
    }

    const UPSAMPLE: usize = 6;
    const TAPS_PER_PHASE: usize = 16;
    const TAPS: usize = UPSAMPLE * TAPS_PER_PHASE;

    const FS_HZ: f64 = 48_000.0;
    const FC_HZ: f64 = 3_400.0;

    let fc = FC_HZ / FS_HZ; // cycles/sample at 48k
    let mid = (TAPS as f64 - 1.0) * 0.5;

    // 1) design float taps
    let mut h = [0.0f64; TAPS];
    for i in 0..TAPS {
        let n = i as f64 - mid;
        let ideal = 2.0 * fc * sinc(2.0 * fc * n);
        let w = blackman(i, TAPS);
        h[i] = ideal * w;
    }

    // 2) normalize DC gain
    let sum: f64 = h.iter().sum();
    for v in &mut h { *v /= sum; }

    // 3) quantize to Q15
    let mut q15 = [0i16; TAPS];
    for i in 0..TAPS {
        let v = (h[i] * 32768.0).round();
        q15[i] = v.clamp(-32768.0, 32767.0) as i16;
    }

    // 4) polyphase split
    let mut phases = [[0i16; TAPS_PER_PHASE]; UPSAMPLE];
    for p in 0..UPSAMPLE {
        for t in 0..TAPS_PER_PHASE {
            phases[p][t] = q15[p + t * UPSAMPLE];
        }
    }

    // 5) emit Rust source
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let dest = out_dir.join("polyphase_h.rs");

    let mut s = String::new();
    s.push_str(&format!(
        "pub const UPSAMPLE: usize = {UPSAMPLE};\n\
         pub const TAPS_PER_PHASE: usize = {TAPS_PER_PHASE};\n\
         pub const H: [[i16; TAPS_PER_PHASE]; UPSAMPLE] = [\n"
    ));
    for p in 0..UPSAMPLE {
        s.push_str("    [");
        for t in 0..TAPS_PER_PHASE {
            s.push_str(&format!("{},", phases[p][t]));
        }
        s.push_str("],\n");
    }
    s.push_str("];\n");

    fs::write(&dest, s).unwrap();

    println!("cargo:rerun-if-changed=build.rs");
}

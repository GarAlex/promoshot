//! The beat fixtures, synthesized — a kick on every beat (a noise click and
//! a 180 Hz thump) and a 55 Hz bass note on every downbeat, no
//! subdivisions (no octave to mistake), 4/4, the first downbeat at 0.25 s.
//! Every sample is `libm` arithmetic, so the fixture is the same bits on
//! every machine, and so is its analysis.

#![allow(dead_code)]

pub const OFFSET: f64 = 0.25;

/// `seconds` of the track at `bpm`, mono at `rate`; bars from `loud_from`
/// (1-based) on play three times louder. With the true beat times.
pub fn click_track(
    bpm: f64,
    seconds: f64,
    rate: f64,
    loud_from: Option<u32>,
) -> (Vec<f32>, Vec<f64>) {
    let n = (seconds * rate) as usize;
    let mut out = vec![0.0f64; n];
    let period = 60.0 / bpm;
    let mut beats = Vec::new();
    let mut state: u64 = 0x5EED_BEA7;
    let mut noise = move || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    };
    let mut k = 0usize;
    loop {
        let t = OFFSET + k as f64 * period;
        if t > seconds - 0.3 {
            break;
        }
        beats.push(t);
        let bar = (k / 4) as u32 + 1;
        let gain = if loud_from.is_some_and(|b| bar >= b) {
            0.9
        } else {
            0.3
        };
        let start = (t * rate).round() as usize;
        let tau = std::f64::consts::TAU;
        for i in 0..(0.25 * rate) as usize {
            let at = start + i;
            if at >= n {
                break;
            }
            let s = i as f64 / rate;
            let mut v = 0.0;
            if s < 0.012 {
                v += 0.8 * libm::exp(-s / 0.003) * noise();
            }
            v += 0.4 * libm::exp(-s / 0.04) * libm::sin(tau * 180.0 * s);
            out[at] += gain * v;
        }
        if k.is_multiple_of(4) {
            for i in 0..(0.6 * rate) as usize {
                let at = start + i;
                if at >= n {
                    break;
                }
                let s = i as f64 / rate;
                let attack = (s / 0.003).min(1.0);
                out[at] += gain * 0.7 * attack * libm::exp(-s / 0.15) * libm::sin(tau * 55.0 * s);
            }
        }
        k += 1;
    }
    (out.into_iter().map(|v| v as f32).collect(), beats)
}

/// A 32-bit float WAV of interleaved `samples`.
pub fn wav(samples: &[f32], channels: u16, rate: u32) -> Vec<u8> {
    let data = (samples.len() * 4) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * u32::from(channels) * 4).to_le_bytes());
    out.extend_from_slice(&(channels * 4).to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

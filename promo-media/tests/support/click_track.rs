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

/// A 16-bit PCM WAV of interleaved `samples`, each rounded to the nearest
/// step and held inside the range — the shape most songs are delivered in.
pub fn wav16(samples: &[f32], channels: u16, rate: u32) -> Vec<u8> {
    let data = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
    out.extend_from_slice(&(channels * 2).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        let v = libm::round(f64::from(*s) * 32768.0).clamp(-32768.0, 32767.0) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Deterministic noise in [-1, 1] (splitmix64), seeded.
pub fn noise(seed: u64) -> impl FnMut() -> f64 {
    let mut state = seed;
    move || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

/// A kick (a 60 Hz thump and a click) at `at` seconds, `gain` loud.
pub fn kick(out: &mut [f64], at: f64, rate: f64, gain: f64, rng: &mut impl FnMut() -> f64) {
    let start = (at * rate).round() as usize;
    for i in 0..(0.25 * rate) as usize {
        let n = start + i;
        if n >= out.len() {
            break;
        }
        let s = i as f64 / rate;
        let mut v = 0.9 * libm::exp(-s / 0.06) * libm::sin(std::f64::consts::TAU * 60.0 * s);
        if s < 0.008 {
            v += 0.5 * libm::exp(-s / 0.002) * rng();
        }
        out[n] += gain * v;
    }
}

/// A short tone (an arpeggio note) at `at`.
pub fn blip(out: &mut [f64], at: f64, rate: f64, hz: f64, gain: f64) {
    let start = (at * rate).round() as usize;
    for i in 0..(0.08 * rate) as usize {
        let n = start + i;
        if n >= out.len() {
            break;
        }
        let s = i as f64 / rate;
        let attack = (s / 0.002).min(1.0);
        out[n] += gain * attack * libm::exp(-s / 0.03) * libm::sin(std::f64::consts::TAU * hz * s);
    }
}

/// A closed hi-hat at `at`.
pub fn hat(out: &mut [f64], at: f64, rate: f64, gain: f64, rng: &mut impl FnMut() -> f64) {
    let start = (at * rate).round() as usize;
    let mut previous = 0.0;
    for i in 0..(0.03 * rate) as usize {
        let n = start + i;
        if n >= out.len() {
            break;
        }
        let s = i as f64 / rate;
        let white = rng();
        // A first difference: the hiss without its low end.
        out[n] += gain * libm::exp(-s / 0.008) * (white - previous) * 0.5;
        previous = white;
    }
}

/// A chiptune-like loop at `bpm`: a kick on every beat, an arpeggio note on
/// every eighth and a hi-hat on every sixteenth — the busy off-beats that
/// pulled the old tracker off the pulse. The first beat at `OFFSET`.
pub fn busy_loop(bpm: f64, seconds: f64, rate: f64) -> (Vec<f32>, Vec<f64>) {
    let n = (seconds * rate) as usize;
    let mut out = vec![0.0f64; n];
    let period = 60.0 / bpm;
    let mut rng = noise(0xC41F_7E57);
    let notes = [523.25, 659.25, 783.99, 1046.5];
    let mut beats = Vec::new();
    let mut k = 0usize;
    loop {
        let t = OFFSET + k as f64 * period;
        if t > seconds - 0.3 {
            break;
        }
        beats.push(t);
        kick(&mut out, t, rate, 0.6, &mut rng);
        for e in 0..2 {
            blip(
                &mut out,
                t + e as f64 * period / 2.0,
                rate,
                notes[(2 * k + e) % 4],
                0.3,
            );
        }
        for s in 0..4 {
            hat(&mut out, t + s as f64 * period / 4.0, rate, 0.25, &mut rng);
        }
        k += 1;
    }
    (out.into_iter().map(|v| v as f32).collect(), beats)
}

/// Kicks whose tempo glides from `from` to `to` BPM across the track — a
/// live player speeding up. With the true beat times.
pub fn gliding_kicks(from: f64, to: f64, seconds: f64, rate: f64) -> (Vec<f32>, Vec<f64>) {
    let n = (seconds * rate) as usize;
    let mut out = vec![0.0f64; n];
    let mut rng = noise(0x0061_1D35);
    let mut beats = Vec::new();
    let mut t = OFFSET;
    while t < seconds - 0.3 {
        beats.push(t);
        kick(&mut out, t, rate, 0.8, &mut rng);
        let bpm = from + (to - from) * (t / seconds);
        t += 60.0 / bpm;
    }
    (out.into_iter().map(|v| v as f32).collect(), beats)
}

/// Clicks on every eighth note of `bpm`, all alike, and a quiet bass note
/// on each beat: heard as `bpm` with eighths, but nothing in the clicks
/// themselves says which pulse is the beat.
pub fn even_eighths(bpm: f64, seconds: f64, rate: f64) -> Vec<f32> {
    let n = (seconds * rate) as usize;
    let mut out = vec![0.0f64; n];
    let mut rng = noise(0x0E16_7475);
    let period = 60.0 / bpm;
    let mut k = 0usize;
    loop {
        let t = OFFSET + k as f64 * period / 2.0;
        if t > seconds - 0.3 {
            break;
        }
        kick(&mut out, t, rate, 0.6, &mut rng);
        k += 1;
    }
    out.into_iter().map(|v| v as f32).collect::<Vec<f32>>()
}

/// Sixteen bars at `bpm`: a kick on every beat throughout, over a pad of
/// two tones (400 and 1600 Hz) that from bar 9 becomes one tone at 1000 Hz
/// — the same loudness (every bar scaled to one), the same spectral
/// centroid, the same bass and the same onsets: only the timbre changes.
pub fn arrangement_change(bpm: f64, rate: f64) -> Vec<f32> {
    let period = 60.0 / bpm;
    let seconds = OFFSET + 16.0 * 4.0 * period + 0.5;
    let n = (seconds * rate) as usize;
    let mut out = vec![0.0f64; n];
    let mut rng = noise(0x00A7_7A9E);
    let change = ((OFFSET + 32.0 * period) * rate) as usize;
    let tau = std::f64::consts::TAU;
    for (i, v) in out.iter_mut().enumerate() {
        let s = i as f64 / rate;
        *v += if i < change {
            0.07 * (libm::sin(tau * 400.0 * s) + libm::sin(tau * 1600.0 * s))
        } else {
            0.1 * libm::sin(tau * 1000.0 * s)
        };
    }
    for k in 0..64 {
        let t = OFFSET + k as f64 * period;
        // The bar's first beat the heavier: where its downbeat is.
        kick(
            &mut out,
            t,
            rate,
            if k % 4 == 0 { 1.0 } else { 0.5 },
            &mut rng,
        );
    }
    // One loudness per bar.
    for bar in 0..16 {
        let a = ((OFFSET + bar as f64 * 4.0 * period) * rate) as usize;
        let b = (((OFFSET + (bar + 1) as f64 * 4.0 * period) * rate) as usize).min(n);
        let mut square = 0.0;
        for v in &out[a..b] {
            square += v * v;
        }
        let rms = (square / (b - a) as f64).sqrt();
        if rms > 0.0 {
            for v in &mut out[a..b] {
                *v *= 0.2 / rms;
            }
        }
    }
    out.into_iter().map(|v| v as f32).collect()
}

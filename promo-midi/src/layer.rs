//! A generated layer's signal work (MIDI.md §7, the experiment's
//! findings): its kind (percussive or tonal, from ACE-Step's
//! `track_name`), the gate that keeps or refuses it — onsets against the
//! sixteenth grid for a percussive layer, chord fit and bar energy for a
//! tonal one — its loudness, and the high-pass a tonal layer gets in the
//! mix so its low register does not stack on the MIDI bass. Everything
//! runs in `f64` in one fixed order with `libm`, so every host agrees.

use promo_model::midi::MidiBlock;
use serde_json::{json, Value};

use crate::render::{Stereo, SAMPLE_RATE};
use crate::score::Score;

/// ACE-Step's lego track names.
pub const TRACK_NAMES: [&str; 12] = [
    "vocals",
    "backing_vocals",
    "drums",
    "bass",
    "guitar",
    "keyboard",
    "percussion",
    "strings",
    "synth",
    "fx",
    "brass",
    "woodwinds",
];

/// Percussive layers: median strong-onset error to the nearest sixteenth
/// above this is refused (ms). Observed on real lego takes: 6–23 ms.
pub const ONSET_LIMIT_MS: f64 = 30.0;
/// Percussive layers: the 95th percentile of the kick band's onset error
/// above this is refused (ms). Observed 31–40 ms; random onsets give 59.
pub const KICK_P95_LIMIT_MS: f64 = 50.0;
/// Tonal layers: the median per-bar chroma correlation with the MIDI's
/// notes below this is refused. Good 32 s takes score 0.8–0.96.
pub const CHORD_FIT_MIN: f64 = 0.6;
/// Tonal layers: a bar more than this far under the median bar (dB) is a
/// layer that sat out — refused.
pub const QUIET_BAR_DB: f64 = 30.0;
/// The high-pass on a tonal layer in the mix (Hz), unless the track says
/// `highPass: false` (or a number of its own).
pub const HIGH_PASS_HZ: f64 = 90.0;
/// What a stored layer is normalised to (LUFS) before it is mixed under
/// the track's volume: sustained beds sit lower than a lead part.
pub const BED_LUFS: f64 = -22.0;
pub const PART_LUFS: f64 = -20.0;

/// ACE-Step's `track_name` for a layer: what the caption names, else
/// `synth` (a pad).
pub fn track_name_for(caption: &str, track: &str) -> &'static str {
    let text = format!("{caption} {track}").to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| text.contains(w));
    if has(&["drum", "kick", "snare", "beat"]) {
        "drums"
    } else if has(&[
        "shaker",
        "hat",
        "percussion",
        "conga",
        "bongo",
        "tambourine",
        "clap",
    ]) {
        "percussion"
    } else if has(&["string", "violin", "viola", "cello", "orchestra"]) {
        "strings"
    } else if has(&[
        "rhodes",
        "piano",
        "keys",
        "organ",
        "electric piano",
        "wurli",
        "clav",
    ]) {
        "keyboard"
    } else if has(&["brass", "horn", "trumpet", "trombone"]) {
        "brass"
    } else if has(&["flute", "sax", "clarinet", "oboe", "woodwind"]) {
        "woodwinds"
    } else if has(&["guitar"]) {
        "guitar"
    } else if has(&["bass"]) {
        "bass"
    } else {
        "synth"
    }
}

/// Whether a track name plays rhythm (the onset gate) rather than
/// harmony (the chord gate).
pub fn is_percussive(track_name: &str) -> bool {
    matches!(track_name, "drums" | "percussion")
}

/// The loudness a stored layer is normalised to.
pub fn target_lufs(track_name: &str) -> f64 {
    match track_name {
        "synth" | "keyboard" => BED_LUFS,
        _ => PART_LUFS,
    }
}

/// The high-pass a layer gets in the mix: `generated.highPass` false is
/// none, a number is that corner, absent is [`HIGH_PASS_HZ`] for a tonal
/// layer (not drums, percussion or bass).
pub fn high_pass_hz(generated: &Value) -> Option<f64> {
    match generated.get("highPass") {
        Some(Value::Bool(false)) => None,
        Some(Value::Number(n)) => n.as_f64().filter(|f| f.is_finite() && *f > 0.0),
        Some(Value::Bool(true)) => Some(HIGH_PASS_HZ),
        _ => {
            let name = generated["trackName"].as_str().unwrap_or("synth");
            (!is_percussive(name) && name != "bass").then_some(HIGH_PASS_HZ)
        }
    }
}

/// A second-order Butterworth high-pass at `hz`, run over the layer. A
/// loop's filter is primed with its own last second, so the seam stays
/// seamless.
pub fn high_pass(audio: &mut Stereo, hz: f64, looped: bool) {
    let rate = f64::from(SAMPLE_RATE);
    let k = libm::tan(std::f64::consts::PI * (hz / rate).min(0.49));
    let q = std::f64::consts::FRAC_1_SQRT_2;
    let norm = 1.0 / (1.0 + k / q + k * k);
    let (b0, b1, b2) = (norm, -2.0 * norm, norm);
    let (a1, a2) = (2.0 * (k * k - 1.0) * norm, (1.0 - k / q + k * k) * norm);
    for channel in [&mut audio.left, &mut audio.right] {
        let n = channel.len();
        let prime = if looped {
            n.min(SAMPLE_RATE as usize)
        } else {
            0
        };
        let (mut x1, mut x2, mut y1, mut y2) = (0.0f64, 0.0, 0.0, 0.0);
        let mut run = |x: f64| {
            let y = b0 * x + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
            x2 = x1;
            x1 = x;
            y2 = y1;
            y1 = y;
            y
        };
        for v in &channel[n - prime..] {
            run(f64::from(*v));
        }
        for v in channel.iter_mut() {
            *v = run(f64::from(*v)) as f32;
        }
    }
}

/// The layer at `lufs`, one static gain held under −1 dBTP. The gain
/// applied, dB (0 for silence).
pub fn normalise(audio: &mut Stereo, lufs: f64) -> f64 {
    let Some(measured) =
        promo_media::loudness::integrated_lufs(&audio.interleaved(), 2, f64::from(SAMPLE_RATE))
    else {
        return 0.0;
    };
    let mut gain = lufs - measured;
    let peak = crate::render::true_peak(&audio.left).max(crate::render::true_peak(&audio.right));
    if peak > 0.0 {
        gain = gain.min(crate::render::TRUE_PEAK_CEILING_DB - 20.0 * f64::from(peak).log10());
    }
    gain = gain.clamp(-40.0, 40.0);
    let scale = 10f64.powf(gain / 20.0) as f32;
    for x in audio.left.iter_mut().chain(audio.right.iter_mut()) {
        *x *= scale;
    }
    gain
}

/// What the gate measured, and whether it keeps the layer.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Gate {
    /// `percussive` or `tonal`.
    pub kind: String,
    pub track_name: String,
    pub measured: bool,
    /// Percussive: the strong onsets' signed median error to the nearest
    /// sixteenth (late positive), ms — the layer's drift.
    pub drift_ms: Option<f64>,
    /// Percussive: the kick band's 95th-percentile |error|, ms.
    pub kick_p95_ms: Option<f64>,
    pub onsets: usize,
    /// Tonal: the median per-bar chroma correlation with the MIDI's notes.
    pub chord_fit: Option<f64>,
    /// Tonal: how far the quietest bar sits under the median bar, dB.
    pub quietest_bar_db: Option<f64>,
    pub bars: usize,
    /// Why it is refused, in words; None keeps it.
    pub refused: Option<String>,
    pub note: Option<String>,
}

impl Gate {
    pub fn to_json(&self) -> Value {
        let r1 = |x: f64| (x * 10.0).round() / 10.0;
        let mut v = json!({ "kind": self.kind, "trackName": self.track_name,
            "measured": self.measured, "onsets": self.onsets, "bars": self.bars,
            "kept": self.refused.is_none() });
        if let Some(d) = self.drift_ms {
            v["driftMs"] = json!(r1(d));
            v["limitMs"] = json!(ONSET_LIMIT_MS);
        }
        if let Some(p) = self.kick_p95_ms {
            v["kickP95Ms"] = json!(r1(p));
        }
        if let Some(c) = self.chord_fit {
            v["chordFit"] = json!((c * 1000.0).round() / 1000.0);
            v["chordFitMin"] = json!(CHORD_FIT_MIN);
        }
        if let Some(q) = self.quietest_bar_db {
            v["quietestBarDb"] = json!(r1(q));
        }
        if let Some(n) = &self.note {
            v["note"] = json!(n);
        }
        if let Some(r) = &self.refused {
            v["refused"] = json!(r);
        }
        v
    }

    /// One line for a report.
    pub fn summary(&self) -> String {
        if !self.measured {
            return format!(
                "not measured ({})",
                self.note.as_deref().unwrap_or("nothing to measure")
            );
        }
        match (self.drift_ms, self.chord_fit) {
            (Some(d), _) => format!(
                "on the grid: drift {d:+.1} ms over {} onsets, kick p95 {}",
                self.onsets,
                self.kick_p95_ms
                    .map_or("—".into(), |p| format!("{p:.0} ms"))
            ),
            (_, Some(c)) => format!(
                "chord fit {c:.2} over {} bars, quietest bar {:.0} dB under the median",
                self.bars,
                self.quietest_bar_db.unwrap_or(0.0)
            ),
            _ => "measured".into(),
        }
    }
}

/// Seconds of bar `bar` (0-based from the take's start) and the next.
fn bar_span(score: &Score, bar: usize) -> (f64, f64) {
    let beat = |b: usize| score.start_beat + b as f64 * score.bar_beats;
    (
        score.map.seconds(beat(bar)) - score.start_seconds,
        score.map.seconds(beat(bar + 1)) - score.start_seconds,
    )
}

/// The gate for a layer one take long, over the take seconds `span`.
/// `rhythm` is the audio the percussive check counts onsets in — for a
/// loop, every period but the last (the grid repeats with it, and one
/// period holds too few kicks for a 95th percentile); `rhythm_span` its
/// seconds to count.
pub fn gate(
    layer: &Stereo,
    rhythm: &Stereo,
    rhythm_span: (f64, f64),
    score: &Score,
    block: &MidiBlock,
    track_name: &str,
    span: (f64, f64),
) -> Gate {
    let mut out = Gate {
        kind: if is_percussive(track_name) {
            "percussive"
        } else {
            "tonal"
        }
        .into(),
        track_name: track_name.into(),
        ..Gate::default()
    };
    let mono: Vec<f64> = layer
        .left
        .iter()
        .zip(&layer.right)
        .map(|(a, b)| (f64::from(*a) + f64::from(*b)) / 2.0)
        .collect();
    let rate = f64::from(SAMPLE_RATE);
    if is_percussive(track_name) {
        let mono: Vec<f64> = rhythm
            .left
            .iter()
            .zip(&rhythm.right)
            .map(|(a, b)| (f64::from(*a) + f64::from(*b)) / 2.0)
            .collect();
        let in_span = |t: f64| t >= rhythm_span.0 - 1e-9 && t < rhythm_span.1;
        let sixteenth = 60.0 / score.bpm * f64::from(score.time_signature[1]) / 16.0;
        let error = |t: f64| (t - (t / sixteenth).round() * sixteenth) * 1000.0;
        let all = onsets(&mono, false);
        let strong = strongest_half(&all);
        let errors: Vec<f64> = strong
            .iter()
            .filter(|o| in_span(o.0))
            .map(|o| error(o.0))
            .collect();
        out.onsets = errors.len();
        if errors.len() < 4 {
            out.note = Some(format!("{} clear onset(s)", errors.len()));
            out.refused = Some(format!(
                "the generated layer plays almost nothing ({} clear onsets) — try another seed",
                errors.len()
            ));
            return out;
        }
        out.measured = true;
        let drift = median(&errors);
        out.drift_ms = Some(drift);
        let kicks: Vec<f64> = strongest_half(&onsets(&mono, true))
            .iter()
            .filter(|o| in_span(o.0))
            .map(|o| error(o.0).abs())
            .collect();
        if kicks.len() >= 4 {
            out.kick_p95_ms = Some(percentile(&kicks, 0.95));
        }
        if drift.abs() > ONSET_LIMIT_MS {
            out.refused = Some(format!(
                "the generated layer is off the MIDI grid: its onsets sit {drift:+.0} ms from the \
                 sixteenths (median of {}) and the limit is {ONSET_LIMIT_MS} ms — not kept. Try \
                 another seed",
                errors.len()
            ));
        } else if let Some(p) = out.kick_p95_ms.filter(|p| *p > KICK_P95_LIMIT_MS) {
            out.refused = Some(format!(
                "the generated layer's kicks wander off the grid (95% within {p:.0} ms, the limit \
                 is {KICK_P95_LIMIT_MS} ms) — not kept. Try another seed"
            ));
        }
        return out;
    }
    // Tonal: each bar's chroma against the notes the MIDI plays in it.
    let bars = (0..4096)
        .map(|b| bar_span(score, b))
        .take_while(|(a, _)| *a < score.length - 1e-6)
        .collect::<Vec<_>>();
    let mut fits = Vec::new();
    let mut levels = Vec::new();
    for (index, (a, b)) in bars.iter().enumerate() {
        let (a, b) = (a.max(span.0), b.min(span.1));
        if b - a < 0.25 {
            continue;
        }
        let (i, j) = ((a * rate) as usize, ((b * rate) as usize).min(mono.len()));
        if j <= i {
            continue;
        }
        let slice = &mono[i..j];
        // The chroma skips the bar's edges, where chords change late.
        let (ci, cj) = (
            (i + (0.1 * rate) as usize).min(j),
            j.saturating_sub((0.05 * rate) as usize),
        );
        let rms = (slice.iter().map(|x| x * x).sum::<f64>() / slice.len() as f64).sqrt();
        levels.push(20.0 * libm::log10(rms.max(1e-12)));
        let expected = bar_notes(block, score, index);
        if expected.iter().all(|w| *w <= 0.0) {
            continue;
        }
        fits.push(correlation(&chroma(&mono[ci..cj.max(ci)]), &expected));
    }
    out.bars = levels.len();
    if levels.is_empty() {
        out.note = Some("no whole bar in the range".into());
        return out;
    }
    out.measured = true;
    let typical = median(&levels);
    let quietest = levels.iter().copied().fold(f64::INFINITY, f64::min);
    out.quietest_bar_db = Some(typical - quietest);
    if !fits.is_empty() {
        out.chord_fit = Some(median(&fits));
    } else {
        out.note = Some("the MIDI plays no pitched notes to fit".into());
    }
    if typical - quietest > QUIET_BAR_DB {
        out.refused = Some(format!(
            "the generated layer sits out part of the take: one bar is {:.0} dB under the others \
             (the limit is {QUIET_BAR_DB} dB) — not kept. Try another seed",
            typical - quietest
        ));
    } else if let Some(fit) = out.chord_fit.filter(|f| *f < CHORD_FIT_MIN) {
        out.refused = Some(format!(
            "the generated layer does not follow the MIDI's chords: chord fit {fit:.2}, at least \
             {CHORD_FIT_MIN} is kept — not kept. Try another seed or caption"
        ));
    }
    out
}

fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let n = s.len();
    if n == 0 {
        0.0
    } else if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    }
}

fn percentile(v: &[f64], p: f64) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let at = ((s.len() as f64 - 1.0) * p).round() as usize;
    s[at.min(s.len() - 1)]
}

/// Onsets as (seconds, strength): the positive rise of a 5 ms energy
/// envelope, peaks over a local window and a floor. `kick` looks at the
/// band under 150 Hz only.
fn onsets(mono: &[f64], kick: bool) -> Vec<(f64, f64)> {
    const HOP: usize = 128;
    let rate = f64::from(SAMPLE_RATE);
    let signal: Vec<f64> = if kick {
        // The kick band: a fourth-order Butterworth low-pass at 150 Hz.
        let once = low_pass(mono, 150.0);
        low_pass(&once, 150.0)
    } else {
        mono.to_vec()
    };
    // 512-sample energies every 128 samples; the flux is the rise over
    // one window, so a hop dates the onset to ~3 ms.
    const WIN: usize = 512;
    const LAG: usize = WIN / HOP;
    let frames = signal.len().saturating_sub(WIN) / HOP + 1;
    let env: Vec<f64> = (0..frames)
        .map(|f| {
            let c = &signal[f * HOP..(f * HOP + WIN).min(signal.len())];
            libm::log10(c.iter().map(|x| x * x).sum::<f64>() / c.len().max(1) as f64 + 1e-10)
        })
        .collect();
    let flux: Vec<f64> = (0..env.len())
        .map(|i| {
            if i < LAG {
                0.0
            } else {
                (env[i] - env[i - LAG]).max(0.0)
            }
        })
        .collect();
    let top = flux.iter().copied().fold(0.0, f64::max);
    if top <= 0.0 {
        return Vec::new();
    }
    let floor = top * 0.2;
    let window = (0.05 * rate) as usize / HOP; // ±50 ms
    let mut out = Vec::new();
    for i in 1..flux.len() {
        let lo = i.saturating_sub(window);
        let hi = (i + window + 1).min(flux.len());
        if flux[i] >= floor && flux[lo..hi].iter().all(|f| *f <= flux[i]) {
            // The rise peaks when the new window's start meets the attack.
            out.push((
                (i * HOP + WIN / 2) as f64 / rate - (LAG * HOP) as f64 / 2.0 / rate,
                flux[i],
            ));
        }
    }
    out
}

/// A second-order Butterworth low-pass at `hz` (48 kHz).
fn low_pass(x: &[f64], hz: f64) -> Vec<f64> {
    let k = libm::tan(std::f64::consts::PI * hz / f64::from(SAMPLE_RATE));
    let q = std::f64::consts::FRAC_1_SQRT_2;
    let norm = 1.0 / (1.0 + k / q + k * k);
    let (b0, b1, b2) = (k * k * norm, 2.0 * k * k * norm, k * k * norm);
    let (a1, a2) = (2.0 * (k * k - 1.0) * norm, (1.0 - k / q + k * k) * norm);
    let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
    x.iter()
        .map(|&v| {
            let y = b0 * v + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
            x2 = x1;
            x1 = v;
            y2 = y1;
            y1 = y;
            y
        })
        .collect()
}

fn strongest_half(all: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let strengths: Vec<f64> = all.iter().map(|o| o.1).collect();
    let cut = median(&strengths);
    all.iter().filter(|o| o.1 >= cut).copied().collect()
}

/// The pitch classes the MIDI's pitched tracks hold in bar `bar` of the
/// take: 1 for each that sounds a quarter of a beat or more, the chord as
/// a template.
fn bar_notes(block: &MidiBlock, score: &Score, bar: usize) -> [f64; 12] {
    let mut w = [0.0f64; 12];
    let from = score.start_beat + bar as f64 * score.bar_beats;
    let to = from + score.bar_beats;
    for track in block.tracks.iter().flatten() {
        if track.is_drums() || track.is_generated() || track.mute == Some(true) {
            continue;
        }
        for n in track.notes.iter().flatten() {
            let overlap = (n.t + n.d).min(to) - n.t.max(from);
            if overlap > 0.0 {
                w[usize::from(n.n % 12)] += overlap;
            }
        }
    }
    w.map(|x| if x >= 0.25 { 1.0 } else { 0.0 })
}

/// Twelve pitch-class energies of `x` (48 kHz): Hann-windowed 4096-point
/// spectra every 2048 samples, 60 Hz – 5 kHz folded onto the classes,
/// each frame scaled to its loudest class (as a chromagram is) and summed.
fn chroma(x: &[f64]) -> [f64; 12] {
    const N: usize = 4096;
    let fft = promo_media::dsp::Fft::new(N);
    let rate = f64::from(SAMPLE_RATE);
    let window: Vec<f64> = (0..N)
        .map(|i| 0.5 - 0.5 * libm::cos(2.0 * std::f64::consts::PI * i as f64 / N as f64))
        .collect();
    let classes: Vec<Option<usize>> = (0..N / 2)
        .map(|k| {
            let f = k as f64 * rate / N as f64;
            (60.0..=5000.0).contains(&f).then(|| {
                let midi = 69.0 + 12.0 * libm::log2(f / 440.0);
                (midi.round() as i64).rem_euclid(12) as usize
            })
        })
        .collect();
    let mut out = [0.0f64; 12];
    let mut at = 0;
    while at + N <= x.len().max(N) {
        let mut re: Vec<f64> = (0..N)
            .map(|i| x.get(at + i).copied().unwrap_or(0.0) * window[i])
            .collect();
        let mut im = vec![0.0; N];
        fft.run(&mut re, &mut im);
        let mut frame = [0.0f64; 12];
        for (k, class) in classes.iter().enumerate() {
            if let Some(c) = class {
                frame[*c] += (re[k] * re[k] + im[k] * im[k]).sqrt();
            }
        }
        let top = frame.iter().copied().fold(0.0, f64::max);
        if top > 1e-9 {
            for c in 0..12 {
                out[c] += frame[c] / top;
            }
        }
        at += N / 2;
        if x.len() < N {
            break;
        }
    }
    out
}

fn correlation(a: &[f64; 12], b: &[f64; 12]) -> f64 {
    let ma = a.iter().sum::<f64>() / 12.0;
    let mb = b.iter().sum::<f64>() / 12.0;
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        num += (a[i] - ma) * (b[i] - mb);
        da += (a[i] - ma) * (a[i] - ma);
        db += (b[i] - mb) * (b[i] - mb);
    }
    if da <= 0.0 || db <= 0.0 {
        0.0
    } else {
        num / (da * db).sqrt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_names_follow_the_caption() {
        assert_eq!(track_name_for("lush string section", "Pad"), "strings");
        assert_eq!(track_name_for("warm pad", "Layer 1"), "synth");
        assert_eq!(track_name_for("shaker sixteenths", "x"), "percussion");
        assert_eq!(track_name_for("warm Rhodes comping", "Keys"), "keyboard");
        assert!(is_percussive("drums") && !is_percussive("strings"));
        assert_eq!(high_pass_hz(&json!({ "trackName": "strings" })), Some(90.0));
        assert_eq!(high_pass_hz(&json!({ "trackName": "drums" })), None);
        assert_eq!(
            high_pass_hz(&json!({ "trackName": "strings", "highPass": false })),
            None
        );
        assert_eq!(high_pass_hz(&json!({ "highPass": 120 })), Some(120.0));
    }

    /// The high-pass takes the low register out and leaves the mids.
    #[test]
    fn the_high_pass_cuts_the_bass() {
        let tone = |hz: f64| {
            let x: Vec<f32> = (0..48_000)
                .map(|i| (2.0 * std::f64::consts::PI * hz * i as f64 / 48_000.0).sin() as f32)
                .collect();
            Stereo {
                left: x.clone(),
                right: x,
            }
        };
        let rms =
            |s: &Stereo| (s.left[24_000..].iter().map(|x| x * x).sum::<f32>() / 24_000.0).sqrt();
        let (mut low, mut mid) = (tone(40.0), tone(440.0));
        high_pass(&mut low, 90.0, false);
        high_pass(&mut mid, 90.0, true);
        assert!(rms(&low) < 0.2, "{}", rms(&low));
        assert!(rms(&mid) > 0.69, "{}", rms(&mid));
    }

    #[test]
    fn chroma_finds_the_notes() {
        let x: Vec<f64> = (0..48_000)
            .map(|i| {
                let t = i as f64 / 48_000.0;
                [261.63, 311.13, 392.0]
                    .iter()
                    .map(|f| (2.0 * std::f64::consts::PI * f * t).sin())
                    .sum::<f64>()
            })
            .collect();
        let mut minor = [0.0; 12];
        for c in [0, 3, 7] {
            minor[c] = 1.0;
        }
        let mut major = [0.0; 12];
        for c in [0, 4, 7] {
            major[c] = 1.0;
        }
        let c = chroma(&x);
        assert!(correlation(&c, &minor) > 0.8, "{c:?}");
        assert!(correlation(&c, &minor) > correlation(&c, &major));
    }
}

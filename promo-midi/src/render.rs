//! The score through rustysynth: one `Synthesizer` per track (so a
//! track's gain and pan are its own, and any number of drum tracks each
//! get the percussion channel), 48 kHz stereo f32 summed, a loop's tail
//! folded onto its head and cut to the exact length, a one-shot run until
//! its tail falls below −80 dBFS — then 24-bit WAV.
//!
//! Reverb and chorus are OFF: rustysynth's built-in Freeverb and chorus
//! are not the bank's, a per-track synth would run one reverb per track,
//! and room belongs to the resource's own `audioEffects` (reverb is one),
//! where every audio file gets the same one. The take is the dry
//! instrument, the same bytes on every host.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};

use crate::score::{Msg, Score};

pub const SAMPLE_RATE: u32 = 48_000;
/// Events land on these block boundaries (0.67 ms at 48 kHz).
const BLOCK: usize = 32;
/// −80 dBFS: below this a tail is silent.
const SILENT: f32 = 1e-4;
/// A one-shot's tail is cut off after this much quiet.
const QUIET_RUN: usize = SAMPLE_RATE as usize / 4;
/// Longest tail rendered after the last event, seconds.
const TAIL_CAP: f64 = 12.0;
/// Longest take, seconds.
pub const LENGTH_CAP: f64 = 20.0 * 60.0;

/// A SoundFont bank and its identity.
pub struct Bank {
    pub path: PathBuf,
    /// The display name the receipt's block records.
    pub name: String,
    /// SHA-256 of the `.sf2`, lowercase hex.
    pub sha256: String,
    pub(crate) font: Arc<SoundFont>,
}

impl std::fmt::Debug for Bank {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bank")
            .field("path", &self.path)
            .field("name", &self.name)
            .field("sha256", &self.sha256)
            .finish()
    }
}

type Cached = (u64, Option<std::time::SystemTime>, Arc<Bank>);

fn cache() -> &'static Mutex<Vec<(PathBuf, Cached)>> {
    static CACHE: OnceLock<Mutex<Vec<(PathBuf, Cached)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

impl Bank {
    /// Loads (once per file version) the bank at `path`: its SHA-256 and
    /// its parsed SoundFont, kept for the next call.
    pub fn open(path: &Path) -> Result<Arc<Bank>, String> {
        let meta =
            std::fs::metadata(path).map_err(|e| format!("sound bank {}: {e}", path.display()))?;
        let stamp = (meta.len(), meta.modified().ok());
        {
            let held = cache().lock().unwrap_or_else(|e| e.into_inner());
            if let Some((_, (len, modified, bank))) = held.iter().find(|(p, _)| p == path) {
                if (*len, *modified) == stamp {
                    return Ok(bank.clone());
                }
            }
        }
        let bytes =
            std::fs::read(path).map_err(|e| format!("sound bank {}: {e}", path.display()))?;
        let sha256 = sha_hex(&bytes);
        let font = SoundFont::new(&mut std::io::Cursor::new(&bytes))
            .map_err(|e| format!("sound bank {}: not a SoundFont: {e}", path.display()))?;
        let name = match crate::sounds::known(&sha256) {
            Some(known) => known.title.to_string(),
            None => {
                let own = font.get_info().get_bank_name().trim().to_string();
                if own.is_empty() {
                    path.file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "SoundFont".into())
                } else {
                    own
                }
            }
        };
        let bank = Arc::new(Bank {
            path: path.to_path_buf(),
            name,
            sha256,
            font: Arc::new(font),
        });
        let mut held = cache().lock().unwrap_or_else(|e| e.into_inner());
        held.retain(|(p, _)| p != path);
        held.push((path.to_path_buf(), (stamp.0, stamp.1, bank.clone())));
        Ok(bank)
    }
}

pub(crate) fn sha_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A rendered take.
#[derive(Debug, Clone)]
pub struct Take {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    /// Peak after any lowering, dBFS.
    pub peak_dbfs: f64,
    pub notes: Vec<String>,
}

impl Take {
    pub fn frames(&self) -> usize {
        self.left.len()
    }

    pub fn seconds(&self) -> f64 {
        self.left.len() as f64 / f64::from(SAMPLE_RATE)
    }
}

fn samples(seconds: f64) -> usize {
    (seconds.max(0.0) * f64::from(SAMPLE_RATE)).round() as usize
}

/// One track on its own synthesizer, until its tail is quiet (or `cap`).
fn track(
    bank: &Bank,
    track: &crate::score::Track,
    cap: usize,
) -> Result<(Vec<f32>, Vec<f32>), String> {
    let mut settings = SynthesizerSettings::new(SAMPLE_RATE as i32);
    settings.block_size = BLOCK;
    settings.maximum_polyphony = 128;
    settings.enable_reverb_and_chorus = false;
    let mut synth = Synthesizer::new(&bank.font, &settings).map_err(|e| format!("synth: {e}"))?;
    let mut events: Vec<(usize, u8, usize, Msg)> = track
        .events
        .iter()
        .enumerate()
        .map(|(i, e)| (samples(e.time), e.msg.rank(), i, e.msg))
        .collect();
    events.sort_by_key(|(at, rank, i, _)| (*at, *rank, *i));
    let last = events.last().map(|e| e.0).unwrap_or(0);
    let channel = i32::from(track.channel);
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let (mut l, mut r) = ([0f32; BLOCK], [0f32; BLOCK]);
    let mut next = 0usize;
    let mut pos = 0usize;
    let mut quiet = 0usize;
    while pos < cap {
        while next < events.len() && events[next].0 < pos + BLOCK {
            match events[next].3 {
                Msg::On { key, vel } => synth.note_on(channel, i32::from(key), i32::from(vel)),
                Msg::Off { key } => synth.note_off(channel, i32::from(key)),
                Msg::Program(p) => synth.process_midi_message(channel, 0xC0, i32::from(p), 0),
                Msg::Control(cc, v) => {
                    synth.process_midi_message(channel, 0xB0, i32::from(cc), i32::from(v))
                }
                Msg::Bend(v) => synth.process_midi_message(
                    channel,
                    0xE0,
                    i32::from(v & 0x7F),
                    i32::from((v >> 7) & 0x7F),
                ),
            }
            next += 1;
        }
        synth.render(&mut l, &mut r);
        let mut loud = false;
        for i in 0..BLOCK {
            let (a, b) = (l[i] * track.gain, r[i] * track.gain);
            loud |= a.abs() >= SILENT || b.abs() >= SILENT;
            left.push(a);
            right.push(b);
        }
        pos += BLOCK;
        if pos > last {
            quiet = if loud { 0 } else { quiet + BLOCK };
            if quiet >= QUIET_RUN {
                break;
            }
        }
    }
    Ok((left, right))
}

/// Renders the score with the bank: deterministic, so the same score and
/// bank give the same samples.
pub fn render(score: &Score, bank: &Bank) -> Result<Take, String> {
    if score.length > LENGTH_CAP {
        return Err(format!(
            "midi: {:.0} s is longer than a take may be ({:.0} s)",
            score.length, LENGTH_CAP
        ));
    }
    let length = samples(score.length);
    if score.looped && length == 0 {
        return Err("midi: a loop needs a length".into());
    }
    let cap = length + samples(TAIL_CAP);
    let mut notes = Vec::new();
    let (mut left, mut right): (Vec<f32>, Vec<f32>) = (Vec::new(), Vec::new());
    for t in score.tracks.iter().filter(|t| !t.muted) {
        let (l, r) = track(bank, t, cap)?;
        if l.len() > left.len() {
            left.resize(l.len(), 0.0);
            right.resize(r.len(), 0.0);
        }
        for (i, (a, b)) in l.iter().zip(&r).enumerate() {
            left[i] += a;
            right[i] += b;
        }
    }
    let (mut left, mut right) = if score.looped {
        // The tail goes round onto the head: every sample past the end
        // lands where the loop has come back to.
        let (mut ol, mut or) = (vec![0f32; length], vec![0f32; length]);
        for (i, (a, b)) in left.iter().zip(&right).enumerate() {
            ol[i % length] += a;
            or[i % length] += b;
        }
        (ol, or)
    } else {
        let last_loud = left
            .iter()
            .zip(&right)
            .rposition(|(a, b)| a.abs() >= SILENT || b.abs() >= SILENT)
            .map_or(0, |i| i + 1);
        let end = last_loud.max(length.min(left.len())).max(1);
        left.resize(end, 0.0);
        right.resize(end, 0.0);
        // rustysynth drops a voice once its envelope is 60 dB down, so a
        // tail ends on a small step: the last 10 ms past the last event
        // fade out instead.
        let fade = (SAMPLE_RATE as usize / 100).min(end.saturating_sub(length));
        for k in 0..fade {
            let g = k as f32 / fade as f32;
            left[end - 1 - k] *= g;
            right[end - 1 - k] *= g;
        }
        (left, right)
    };
    let peak = left.iter().chain(&right).fold(0f32, |m, x| m.max(x.abs()));
    let ceiling = 0.999f32;
    let mut peak_out = peak;
    if peak > ceiling {
        let scale = ceiling / peak;
        for x in left.iter_mut().chain(right.iter_mut()) {
            *x *= scale;
        }
        notes.push(format!(
            "lowered {:.1} dB so the mix does not clip",
            -20.0 * f64::from(scale).log10()
        ));
        peak_out = ceiling;
    }
    if peak == 0.0 {
        notes.push("the take is silent".into());
    }
    let peak_dbfs = if peak_out > 0.0 {
        20.0 * f64::from(peak_out).log10()
    } else {
        f64::NEG_INFINITY
    };
    Ok(Take {
        left,
        right,
        peak_dbfs,
        notes,
    })
}

/// The take as a 48 kHz stereo 24-bit PCM WAV file.
pub fn wav(take: &Take) -> Vec<u8> {
    let frames = take.frames();
    let data = frames * 2 * 3;
    let mut out = Vec::with_capacity(44 + data);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 6).to_le_bytes());
    out.extend_from_slice(&6u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data as u32).to_le_bytes());
    for i in 0..frames {
        for x in [take.left[i], take.right[i]] {
            let v = (f64::from(x.clamp(-1.0, 1.0)) * 8_388_607.0).round() as i32;
            out.extend_from_slice(&v.to_le_bytes()[..3]);
        }
    }
    out
}

//! The `local` provider: narration synthesized on this machine by
//! [local-tts](https://github.com/GarAlex/local-tts), with no key, no
//! account and no network once a voice package is installed. The audio
//! belongs to the person: the shipped voices are Apache-2.0 weights, so a
//! film narrated here can be published — which a system voice (macOS
//! AVSpeechSynthesizer) cannot, its licence being personal use only.
//!
//! A voice id is `<package>/<speaker>`, e.g. `kokoro-en-q8/af_heart` or
//! `kitten-nano-en/Luna`. Packages live in [`voices_dir`]; they are
//! installed with `promoshot-mcp voices install <package>`, which fetches
//! every file from its pinned upstream URL and checks its SHA-256 before
//! the package appears. Output is mono 16-bit WAV, so a local take is
//! named `.wav`, not `.mp3`.
//!
//! Words the English dictionary does not know (product names, mostly)
//! fail rather than being guessed: a project spells them once in
//! `Resources/pronunciations.dict`, CMU format — `PROMOSHOT  P R OW1 M OW0
//! SH AA2 T` — and every local take in that project reads them.

use local_tts_core::{
    compact_english::{CompactEnglish, CompactKind},
    kokoro::Kokoro,
    Language, SynthesisRequest, Synthesizer, TimedSynthesizer,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub use local_tts_core::catalog;

/// The provider id as a project spells it.
pub const PROVIDER: &str = "local";

/// The voice a local narration gets when its script names none.
pub const DEFAULT_VOICE: &str = "kokoro-en-q8/af_heart";

/// The project file a local take reads its extra pronunciations from.
pub const LEXICON: &str = "pronunciations.dict";

/// Where voice packages are installed: `PROMOSHOT_VOICES` when set (the
/// Docker image points it at its `/voices` volume), else the platform's
/// per-user data folder — `~/Library/Application Support/PromoShot/Voices`
/// on macOS, `$XDG_DATA_HOME/promoshot/voices` (or
/// `~/.local/share/promoshot/voices`) on Linux, `%APPDATA%\PromoShot\Voices`
/// on Windows.
pub fn voices_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("PROMOSHOT_VOICES").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let home = || {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
    };
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/PromoShot/Voices")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(home)
            .join("PromoShot")
            .join("Voices")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/share"))
            .join("promoshot")
            .join("voices")
    }
}

/// Kitten's speakers by the names its card gives them.
const KITTEN: [(&str, &str); 8] = [
    ("Bella", "female"),
    ("Jasper", "male"),
    ("Luna", "female"),
    ("Bruno", "male"),
    ("Rosie", "female"),
    ("Hugo", "male"),
    ("Kiki", "female"),
    ("Leo", "male"),
];

/// Kokoro's upstream grades, best first — the roster's order.
const GRADES: [&str; 13] = [
    "A", "A-", "B+", "B", "B-", "C+", "C", "C-", "D+", "D", "D-", "F+", "F",
];

/// The speakers a package offers, each with a line of detail; Kokoro's
/// best-graded first.
fn speakers(package: &str) -> Vec<(String, String)> {
    match package {
        "kokoro-en-q8" => {
            let mut voices = local_tts_core::kokoro::voices();
            voices.sort_by_key(|v| {
                (
                    GRADES
                        .iter()
                        .position(|g| *g == v.grade)
                        .unwrap_or(GRADES.len()),
                    v.name.clone(),
                )
            });
            voices
                .into_iter()
                .map(|v| {
                    let accent = if v.accent == "gb" {
                        "British (read with American pronunciation)"
                    } else {
                        "American"
                    };
                    (v.name, format!("{} {accent}, grade {}", v.gender, v.grade))
                })
                .collect()
        }
        "kitten-nano-en" => KITTEN
            .iter()
            .map(|(name, detail)| (name.to_string(), detail.to_string()))
            .collect(),
        "inflect-nano-en" => vec![("default".into(), "male, small and fast".into())],
        _ => Vec::new(),
    }
}

/// Is this speaker's file in the installed package? Only Kokoro's voices
/// are separate files; a package installed before they were pinned has
/// af_heart alone until it is installed again.
fn speaker_installed(package: &str, speaker: &str) -> bool {
    package != "kokoro-en-q8"
        || voices_dir()
            .join(package)
            .join(format!("{speaker}.bin"))
            .is_file()
}

/// `package/speaker` from a voice id, the package's speaker defaulted.
pub fn split_voice(voice: &str) -> Result<(String, String), String> {
    let (package, speaker) = match voice.split_once('/') {
        Some((package, speaker)) => (package.to_string(), speaker.to_string()),
        None => (voice.to_string(), String::new()),
    };
    let known = speakers(&package);
    if known.is_empty() {
        return Err(format!(
            "local voice `{voice}`: no package `{package}` — voices are <package>/<speaker>, \
             e.g. {DEFAULT_VOICE}; `promoshot-mcp voices list` shows them"
        ));
    }
    let speaker = if speaker.is_empty() {
        if package == "kokoro-en-q8" {
            local_tts_core::kokoro::DEFAULT_VOICE.to_string()
        } else {
            known[0].0.clone()
        }
    } else if known.iter().any(|(name, _)| *name == speaker) {
        speaker
    } else {
        let names: Vec<&str> = known.iter().map(|(n, _)| n.as_str()).collect();
        return Err(format!(
            "local voice `{voice}`: {package} has {}",
            names.join(", ")
        ));
    };
    Ok((package, speaker))
}

/// Why `voice` cannot be spoken here yet — its package not installed —
/// or None when it can.
pub fn unavailable(voice: &str) -> Option<String> {
    let (package, speaker) = match split_voice(voice) {
        Ok(parts) => parts,
        Err(e) => return Some(e),
    };
    if !catalog::installed(&voices_dir()).contains(&package.as_str()) {
        Some(not_installed(&package))
    } else if !speaker_installed(&package, &speaker) {
        Some(needs_update(&package, &speaker))
    } else {
        None
    }
}

fn needs_update(package: &str, speaker: &str) -> String {
    format!(
        "local voice `{package}/{speaker}` is newer than the installed {package} — the person \
         runs `promoshot-mcp voices install {package}` once to add it (only the new voices are \
         downloaded)"
    )
}

fn not_installed(package: &str) -> String {
    format!(
        "local voice package `{package}` is not installed in {} — the person installs it once \
         with `promoshot-mcp voices install {package}` (a pinned, checksummed download; no key)",
        voices_dir().display()
    )
}

/// Every local voice: the installed packages' speakers, and one line per
/// package still to install.
pub fn roster() -> Vec<crate::Voice> {
    let installed = catalog::installed(&voices_dir());
    catalog::catalog()
        .iter()
        .flat_map(|package| {
            let present = installed.contains(&package.id);
            speakers(package.id).into_iter().map(move |(speaker, detail)| {
                let ready = present && speaker_installed(package.id, &speaker);
                let mut attributes = std::collections::BTreeMap::new();
                attributes.insert("package".into(), package.id.to_string());
                attributes.insert("license".into(), package.license.to_string());
                attributes.insert(
                    "installed".into(),
                    if ready { "yes" } else { "no" }.to_string(),
                );
                crate::Voice {
                    id: format!("{}/{speaker}", package.id),
                    name: speaker.to_string(),
                    language: Some("en".into()),
                    detail: Some(if ready {
                        detail.to_string()
                    } else if present {
                        format!(
                            "{detail} — not in this install (`promoshot-mcp voices install {}` adds it)",
                            package.id
                        )
                    } else {
                        format!(
                            "{detail} — not installed ({:.0} MB: `promoshot-mcp voices install {}`)",
                            package.download_bytes() as f64 / 1e6,
                            package.id
                        )
                    }),
                    attributes,
                }
            })
        })
        .collect()
}

enum Engine {
    Kokoro(Kokoro),
    Compact(CompactEnglish),
}

/// Loaded engines, by package and lexicon: a model loads once per server.
type Cache = HashMap<(String, Option<(PathBuf, std::time::SystemTime)>), Engine>;

/// (word as written, ARPAbet, how it was guessed), each word once.
type Guesses = Vec<crate::LocalGuess>;

/// Words the dictionary lacked that local takes guessed since the last
/// [`take_guesses`]: (word as written, ARPAbet, how it was guessed).
fn guessed() -> &'static Mutex<Guesses> {
    static GUESSED: OnceLock<Mutex<Guesses>> = OnceLock::new();
    GUESSED.get_or_init(|| Mutex::new(Vec::new()))
}

/// The guesses local takes made since the last call, each word once.
pub fn take_guesses() -> Guesses {
    std::mem::take(&mut *guessed().lock().unwrap_or_else(|e| e.into_inner()))
}

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// One local take as WAV bytes. `lexicon`, when the file exists, adds its
/// pronunciations for this take's engine.
pub fn synthesize(voice: &str, text: &str, lexicon: Option<&Path>) -> Result<Vec<u8>, String> {
    run(voice, text, lexicon, false).map(|(wav, _)| wav)
}

/// [`synthesize`], and each spoken word's time from the voice's own
/// duration predictor (local-tts 0.7): Kokoro and Kitten report them, the
/// same request, the same audio; a voice that cannot (Inflect) answers its
/// audio with no words, and the take is aligned instead. The words are the
/// engine's normalized ones ("forty", "nine", "dollars").
pub fn synthesize_timed(
    voice: &str,
    text: &str,
    lexicon: Option<&Path>,
) -> Result<(Vec<u8>, Option<Vec<local_tts_core::SpokenWord>>), String> {
    run(voice, text, lexicon, true)
}

fn run(
    voice: &str,
    text: &str,
    lexicon: Option<&Path>,
    timed: bool,
) -> Result<(Vec<u8>, Option<Vec<local_tts_core::SpokenWord>>), String> {
    let (package, speaker) = split_voice(voice)?;
    let root = voices_dir().join(&package);
    if !catalog::installed(&voices_dir()).contains(&package.as_str()) {
        return Err(not_installed(&package));
    }
    let lexicon = lexicon.filter(|path| path.is_file()).map(|path| {
        let modified = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        (path.to_path_buf(), modified)
    });
    let mut engines = cache()
        .lock()
        .map_err(|_| "local voices: engine cache poisoned")?;
    // A changed lexicon is a different engine; drop the stale ones for
    // this package so a long session does not hold every edit in memory.
    engines.retain(|(p, l), _| p != &package || l == &lexicon);
    let key = (package.clone(), lexicon.clone());
    if !engines.contains_key(&key) {
        let mut engine = match package.as_str() {
            "kokoro-en-q8" => Engine::Kokoro(Kokoro::load(&root).map_err(|e| e.to_string())?),
            "kitten-nano-en" => Engine::Compact(
                CompactEnglish::load(&root, CompactKind::Kitten).map_err(|e| e.to_string())?,
            ),
            _ => Engine::Compact(
                CompactEnglish::load(&root, CompactKind::Inflect).map_err(|e| e.to_string())?,
            ),
        };
        // A name the dictionary lacks is guessed, not refused; every guess
        // is reported (take_guesses) so it can be checked and pinned.
        match &mut engine {
            Engine::Kokoro(e) => e.set_fallback(true),
            Engine::Compact(e) => e.set_fallback(true),
        }
        .map_err(|e| e.to_string())?;
        if let Some((path, _)) = &lexicon {
            match &mut engine {
                Engine::Kokoro(e) => e.add_lexicon(path),
                Engine::Compact(e) => e.add_lexicon(path),
            }
            .map_err(|e| e.to_string())?;
        }
        engines.insert(key.clone(), engine);
    }
    let request = SynthesisRequest {
        text,
        language: Language::En,
        speaker: &speaker,
        duration_scale: 1.0,
    };
    let engine = engines.get_mut(&key).expect("inserted above");
    let timed = if timed {
        match engine {
            Engine::Kokoro(e) => e.synthesize_timed(&request).ok(),
            Engine::Compact(e) => e.synthesize_timed(&request).ok(),
        }
    } else {
        None
    };
    let (audio, words) = match timed {
        Some(t) => {
            let words = snap_to_voice(&t.audio.samples, t.audio.sample_rate, t.words);
            (t.audio, Some(words))
        }
        None => (
            match engine {
                Engine::Kokoro(e) => e.synthesize(&request),
                Engine::Compact(e) => e.synthesize(&request),
            }
            .map_err(|e| explain(&e.to_string()))?,
            None,
        ),
    };
    let guesses = match engine {
        Engine::Kokoro(e) => e.take_guesses(),
        Engine::Compact(e) => e.take_guesses(),
    };
    let mut all = guessed().lock().unwrap_or_else(|e| e.into_inner());
    for guess in guesses {
        if !all.iter().any(|g| g.word == guess.word) {
            all.push(crate::LocalGuess {
                unsure: guess.unsure(),
                word: guess.word,
                arpabet: guess.arpabet,
                method: guess.method,
            });
        }
    }
    Ok((wav(&audio.samples, audio.sample_rate)?, words))
}

/// A voice's predicted word edges put on its own audio where they border
/// silence. The predictor's frames are the model's alignment, not what is
/// heard: a sentence's last word holds on in silence (Kokoro's final
/// lengthening ends up to 0.3 s after the voice does) and a word after a
/// pause is predicted a little after its onset. So an edge with a pause on
/// its outer side moves to where the voice really starts or stops — the
/// first (last) 10 ms window within 28 dB of the take's loudest, after
/// (before) at least 30 ms under it — searching no further than the
/// neighbouring word. Edges inside connected speech keep the prediction.
/// Plain arithmetic on the samples, the same on every machine.
pub(crate) fn snap_to_voice(
    samples: &[f32],
    rate: u32,
    words: Vec<local_tts_core::SpokenWord>,
) -> Vec<local_tts_core::SpokenWord> {
    if rate == 0 || samples.is_empty() || words.is_empty() {
        return words;
    }
    let rate = f64::from(rate);
    let hop = (rate * 0.002) as usize; // 2 ms
    let half = (rate * 0.005) as usize; // a 10 ms window
    let levels: Vec<f64> = (0..samples.len() / hop.max(1))
        .map(|k| {
            let c = k * hop;
            let (a, b) = (c.saturating_sub(half), (c + half).min(samples.len()));
            let mut e = 0.0f64;
            for v in &samples[a..b] {
                e += f64::from(*v) * f64::from(*v);
            }
            10.0 * (e / (b - a).max(1) as f64 + 1e-12).log10()
        })
        .collect();
    let peak = levels.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let loud = |k: usize| levels.get(k).is_some_and(|l| *l >= peak - 28.0);
    let at = |t: f64| ((t * rate) as usize / hop.max(1)).min(levels.len().saturating_sub(1));
    let quiet_run = 15; // 30 ms of 2 ms steps
    let mut out = words.clone();
    for i in 0..words.len() {
        let floor = if i == 0 {
            0.0
        } else {
            words[i - 1].end.max(out[i - 1].end)
        };
        let ceiling = words
            .get(i + 1)
            .map_or(samples.len() as f64 / rate, |w| w.start);
        // Start: the first loud window from the previous word's end on,
        // when quiet comes before it.
        let (lo, hi) = (at(floor.max(words[i].start - 0.15)), at(words[i].end));
        if let Some(k) = (lo..=hi).find(|&k| loud(k)) {
            let quiet_before = k >= quiet_run && (k - quiet_run..k).all(|j| !loud(j));
            if quiet_before && (k as f64 * hop as f64 / rate) < words[i].end {
                out[i].start = k as f64 * hop as f64 / rate;
            }
        }
        // End: the last loud window before the next word, when quiet
        // follows it.
        let (lo, hi) = (at(out[i].start), at(ceiling.min(words[i].end + 0.05)));
        if let Some(k) = (lo..=hi).rev().find(|&k| loud(k)) {
            let quiet_after = (k + 1..k + 1 + quiet_run).all(|j| !loud(j) && j < levels.len());
            let end = (k + 1) as f64 * hop as f64 / rate;
            if quiet_after && end > out[i].start {
                out[i].end = end;
            }
        }
    }
    out
}

/// The engine's refusal, with where the fix goes in a project.
fn explain(error: &str) -> String {
    if error.starts_with("Unknown word") {
        format!(
            "{error} For local voices, spell it once in Resources/{LEXICON} \
             (CMU format, e.g. `PROMOSHOT  P R OW1 M OW0 SH AA2 T`) or write it as words."
        )
    } else {
        error.to_string()
    }
}

/// Mono 16-bit PCM WAV, the format `tts-speak` writes.
fn wav(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>, String> {
    if sample_rate == 0 || samples.iter().any(|s| !s.is_finite()) {
        return Err("local voice produced invalid audio".into());
    }
    let data = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&((sample.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_ids_name_a_package_and_a_speaker() {
        assert_eq!(
            split_voice("kokoro-en-q8").unwrap(),
            ("kokoro-en-q8".into(), "af_heart".into())
        );
        assert_eq!(
            split_voice("kitten-nano-en/Luna").unwrap(),
            ("kitten-nano-en".into(), "Luna".into())
        );
        assert!(split_voice("kitten-nano-en/Nobody").is_err());
        assert!(split_voice("alloy").is_err());
    }

    /// Edges that border silence move onto the voice; edges inside
    /// connected speech keep the prediction.
    #[test]
    fn predicted_edges_snap_to_the_voice_where_they_border_silence() {
        let rate = 24_000u32;
        // Voice from 0.30 to 0.90 s and from 1.30 to 2.00 s, silence else.
        let samples: Vec<f32> = (0..rate as usize * 3)
            .map(|i| {
                let t = i as f64 / f64::from(rate);
                let on = (0.30..0.90).contains(&t) || (1.30..2.00).contains(&t);
                if on {
                    (0.3 * (2.0 * std::f64::consts::PI * 180.0 * t).sin()) as f32
                } else {
                    0.0
                }
            })
            .collect();
        let word = |text: &str, start: f64, end: f64| local_tts_core::SpokenWord {
            text: text.into(),
            start,
            end,
        };
        let snapped = snap_to_voice(
            &samples,
            rate,
            vec![
                word("meet", 0.375, 0.50),  // starts late after silence
                word("euclid", 0.55, 1.10), // connected start; ends in silence
                word("geometry", 1.35, 2.30),
            ],
        );
        assert!((snapped[0].start - 0.30).abs() < 0.008, "{snapped:?}");
        assert_eq!(
            snapped[0].end, 0.50,
            "a connected edge keeps the prediction"
        );
        assert_eq!(snapped[1].start, 0.55, "voice runs on before it: kept");
        assert!((snapped[1].end - 0.90).abs() < 0.008, "{snapped:?}");
        assert!((snapped[2].start - 1.30).abs() < 0.008, "{snapped:?}");
        assert!((snapped[2].end - 2.00).abs() < 0.008, "{snapped:?}");
    }

    #[test]
    fn the_wav_header_matches_its_payload() {
        let bytes = wav(&[0.0, 0.5, -0.5, 1.0], 24000).unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(bytes.len(), 44 + 8);
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 24000);
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 8);
        assert!(wav(&[f32::NAN], 24000).is_err());
    }

    #[test]
    fn the_roster_lists_every_package_and_says_what_is_missing() {
        let roster = roster();
        assert!(roster.iter().any(|v| v.id == DEFAULT_VOICE));
        assert_eq!(
            roster
                .iter()
                .filter(|v| v.id.starts_with("kitten-nano-en/"))
                .count(),
            8
        );
        let kokoro: Vec<&str> = roster
            .iter()
            .filter(|v| v.id.starts_with("kokoro-en-q8/"))
            .map(|v| v.name.as_str())
            .collect();
        assert_eq!(kokoro.len(), 28);
        assert_eq!(&kokoro[..2], ["af_heart", "af_bella"], "best grades first");
        assert!(split_voice("kokoro-en-q8/bm_george").is_ok());
    }
}

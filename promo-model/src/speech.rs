//! Word timing (docs/SPEECH-SYNC.md §1): one record for every take,
//! whatever timed it — a provider's own timing, a local voice's duration
//! predictor, offline alignment, a transcription or a syllable estimate —
//! and the one tokenizer every source, the resolver and the reveal mapping
//! cut a script into words and sentences with.
//!
//! Here, beside the wire types, so the timeline and the author tools read
//! the record without the media stack. The record lives in the project's
//! `Resources/.words/` cache (`crate::word_cache`), never in
//! `metadata.json`; the only timing data a document carries is a
//! resource's `speech.wordFixes`, the hand corrections.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::media_cache::MediaSource;

/// The record's format. A reader takes any version it knows; a newer one
/// is still a record (unknown keys are kept in `extra`).
pub const TIMING_VERSION: u32 = 1;

/// The offline aligner's version (`promo_media::words`): bumped whenever
/// the alignment of the same samples could change, so a record from an
/// older aligner is aligned again (§1.3). Here, beside the record, so a
/// reader that never aligns (the apps' cache check) knows it too.
/// 2: pauses belong at the script's punctuation (issue #10).
pub const ALIGNER_VERSION: u32 = 2;

/// The version of the syllable estimate (`source: "estimate"`), written as
/// an estimate record's `alignerVersion`: a record from an older estimate
/// is estimated again, as an older aligner's is re-aligned.
pub const ESTIMATE_VERSION: u32 = 1;

/// Where a take's word times came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimingSource {
    /// The provider's own timing: `provider:elevenlabs`, `provider:google`.
    Provider(String),
    /// A local voice's duration predictor: `local:<package>/<speaker>`.
    Local(String),
    /// Offline forced alignment.
    Aligned,
    /// An opt-in provider transcription: `transcribed:openai`.
    Transcribed(String),
    /// The syllable estimate (placeholders, takes not timed yet).
    Estimate,
    /// A source this build does not know, kept as written.
    Other(String),
}

impl TimingSource {
    pub fn as_text(&self) -> String {
        match self {
            TimingSource::Provider(p) => format!("provider:{p}"),
            TimingSource::Local(v) => format!("local:{v}"),
            TimingSource::Aligned => "aligned".into(),
            TimingSource::Transcribed(p) => format!("transcribed:{p}"),
            TimingSource::Estimate => "estimate".into(),
            TimingSource::Other(s) => s.clone(),
        }
    }

    pub fn parse(text: &str) -> TimingSource {
        if let Some(p) = text.strip_prefix("provider:") {
            return TimingSource::Provider(p.into());
        }
        if let Some(v) = text.strip_prefix("local:") {
            return TimingSource::Local(v.into());
        }
        if let Some(p) = text.strip_prefix("transcribed:") {
            return TimingSource::Transcribed(p.into());
        }
        match text {
            "aligned" => TimingSource::Aligned,
            "estimate" => TimingSource::Estimate,
            other => TimingSource::Other(other.into()),
        }
    }

    /// A source whose times cannot be recomputed and are never replaced
    /// except by a new take (§1.3, "Authority").
    pub fn is_authoritative(&self) -> bool {
        matches!(
            self,
            TimingSource::Provider(_) | TimingSource::Local(_) | TimingSource::Transcribed(_)
        )
    }
}

impl Serialize for TimingSource {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.as_text())
    }
}

impl<'de> Deserialize<'de> for TimingSource {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(TimingSource::parse(&String::deserialize(d)?))
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One script word's place in the take.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Word {
    /// The script's token with its outer punctuation removed.
    pub text: String,
    /// Source seconds of the take, rounded to 1 µs.
    pub start: f64,
    pub end: f64,
    /// `[start, end)` in Unicode scalar indices into the record's `script`.
    pub chars: [u32; 2],
    /// Written only where it differs from the take's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// The source never voiced this word on its own; its times are shared
    /// out within its neighbours' gap.
    #[serde(default, skip_serializing_if = "is_false")]
    pub interpolated: bool,
    /// 0–1, from the audio (S8): a cue for generators, never a timing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prominence: Option<f64>,
}

/// One sentence of the take.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sentence {
    pub start: f64,
    pub end: f64,
    /// `[first, last]` word indices, inclusive, 0-based.
    pub words: [u32; 2],
    /// `[start, end)` scalar indices into `script`, punctuation included.
    pub chars: [u32; 2],
}

/// A take's word timing — the cache file, CLI `--json` and MCP alike.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeTiming {
    pub timing_version: u32,
    /// The audio file the times are of (filled in when stored).
    pub audio: MediaSource,
    /// The script exactly as written (`speech.text`, else `transcript`).
    pub script: String,
    /// 8 hex digits of the SHA-256 of `script` ([`script_hash`]).
    pub script_hash: String,
    /// The take's length, seconds.
    pub duration: f64,
    pub source: TimingSource,
    /// True only for `source: "estimate"` — so a reader never parses the
    /// source to learn whether it may trust the times.
    pub estimated: bool,
    /// 0–1 for the take as a whole.
    pub confidence: f64,
    pub words: Vec<Word>,
    pub sentences: Vec<Sentence>,
    /// The aligner (or estimate) that made an `aligned` or `estimate`
    /// record: an older one is made again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aligner_version: Option<u32>,
    /// Keys a newer writer added, kept.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// What a source hands back before it is mapped onto the script's words
/// (§2.5): a word as the source said it, its times, and — when the source
/// reports characters — the span of the text it was given.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpokenWord {
    pub text: String,
    pub start: f64,
    /// None when the source names only where a word starts (a Google
    /// mark): the end is the next word's start, or the take's end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chars: Option<[u32; 2]>,
}

/// A hand correction of one word's time, over whatever record the take
/// has: `speech.wordFixes` on the resource.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WordFix {
    /// 1-based, as every word index on the wire.
    pub word: u32,
    /// Source seconds of the take.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<f64>,
    /// The script the fix was made for ([`script_hash`]); a fix for
    /// another script is ignored, and said so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_hash: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// 8 hex digits of the SHA-256 of `script` exactly as written — the
/// script half of a record's cache key.
pub fn script_hash(script: &str) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(script.as_bytes());
    digest[..4].iter().map(|b| format!("{b:02x}")).collect()
}

/// A time as the record writes it: rounded to 1 µs.
pub fn round_us(t: f64) -> f64 {
    (t * 1e6).round() / 1e6
}

// ---------------------------------------------------------------------------
// The tokenizer (§1.2).

/// One word of a script.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptWord {
    /// The token with its outer punctuation removed (SSML entities
    /// decoded).
    pub text: String,
    /// `[start, end)` scalar indices of the word in the script.
    pub chars: [u32; 2],
    /// The word with its punctuation margins — what a sentence spans.
    pub token: [u32; 2],
    /// The whitespace-separated run it is in: the caption reveal's word
    /// unit (`promo-text`, `RevealBy::Word`), an index into
    /// [`Script::units`].
    pub unit: u32,
    /// In an SSML script, the scalar index of the `<` of the outermost
    /// `<say-as>`, `<sub>` or `<phoneme>` the word sits in — a provider
    /// mark may not go inside one.
    pub atomic: Option<u32>,
}

/// One sentence of a script.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptSentence {
    /// `[first, last]` word indices, inclusive.
    pub words: [u32; 2],
    /// `[start, end)` scalar indices, punctuation included.
    pub chars: [u32; 2],
}

/// A script cut into reveal units, words and sentences.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Script {
    /// Every maximal run of non-whitespace (outside tags, in SSML) —
    /// `[start, end)` scalar indices. A caption reveals by these.
    pub units: Vec<[u32; 2]>,
    pub words: Vec<ScriptWord>,
    pub sentences: Vec<ScriptSentence>,
    /// The script is SSML (it begins with `<speak`).
    pub ssml: bool,
}

/// Punctuation a word's margins lose.
const MARGIN: &str = "\"“”‘’«»()[]{}.,;:!?…—–-";

/// Ends a sentence when whitespace (or the end) follows.
const TERMINATORS: &str = ".!?…";

/// Ends a sentence wherever it stands: scripts written without spaces.
const CJK_TERMINATORS: &str = "。！？";

/// Closing marks that may stand between a terminator and the whitespace.
const CLOSERS: &str = "\"”’»)]}'";

/// Words whose final period ends no sentence.
const ABBREVIATIONS: &[&str] = &["dr.", "mr.", "mrs.", "ms.", "e.g.", "i.e.", "vs.", "etc."];

fn is_margin(c: char) -> bool {
    MARGIN.contains(c)
}

/// A character written without spaces between words, which v1 makes a
/// word of its own: Han and kana.
fn is_cluster_script(c: char) -> bool {
    let u = c as u32;
    matches!(u,
        0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F
        | 0x3040..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F)
}

fn is_thai(c: char) -> bool {
    (0x0E00..=0x0E7F).contains(&(c as u32))
}

/// A mark that belongs to the character before it.
fn is_combining(c: char) -> bool {
    let u = c as u32;
    matches!(u,
        0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F
        | 0x3099..=0x309A | 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E)
}

/// One visible character of the script: the character, the raw scalar span
/// it came from (an SSML entity is one character over several), and the
/// atomic SSML element it sits in. `None` as the character is a boundary
/// a tag left — it separates words as whitespace does.
#[derive(Clone, Copy)]
struct Visible {
    c: Option<char>,
    start: u32,
    end: u32,
    atomic: Option<u32>,
}

fn visible(script: &str, ssml: bool) -> Vec<Visible> {
    let raw: Vec<char> = script.chars().collect();
    if !ssml {
        return raw
            .iter()
            .enumerate()
            .map(|(i, &c)| Visible {
                c: Some(c),
                start: i as u32,
                end: i as u32 + 1,
                atomic: None,
            })
            .collect();
    }
    let mut out = Vec::with_capacity(raw.len());
    // Open elements: (name, the `<` of their start tag).
    let mut stack: Vec<(String, u32)> = Vec::new();
    let atomic_of = |stack: &[(String, u32)]| {
        stack
            .iter()
            .find(|(name, _)| matches!(name.as_str(), "say-as" | "sub" | "phoneme"))
            .map(|(_, at)| *at)
    };
    let mut i = 0usize;
    while i < raw.len() {
        let c = raw[i];
        if c == '<' {
            let close = raw[i..].iter().position(|&c| c == '>').map(|p| i + p);
            let end = close.unwrap_or(raw.len() - 1);
            let inner: String = raw[i + 1..end.max(i + 1)].iter().collect();
            let comment = inner.starts_with('!') || inner.starts_with('?');
            if !comment {
                let closing = inner.starts_with('/');
                let body = inner.trim_start_matches('/');
                let name: String = body
                    .chars()
                    .take_while(|c| !c.is_whitespace() && *c != '/' && *c != '>')
                    .collect::<String>()
                    .to_ascii_lowercase();
                let self_closing = inner.trim_end().ends_with('/');
                if closing {
                    if let Some(at) = stack.iter().rposition(|(n, _)| *n == name) {
                        stack.truncate(at);
                    }
                } else if !self_closing {
                    stack.push((name, i as u32));
                }
            }
            out.push(Visible {
                c: None,
                start: i as u32,
                end: end as u32 + 1,
                atomic: None,
            });
            i = end + 1;
            continue;
        }
        if c == '&' {
            if let Some(semi) = raw[i..].iter().take(10).position(|&c| c == ';') {
                let name: String = raw[i + 1..i + semi].iter().collect();
                let decoded = match name.as_str() {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    _ => name
                        .strip_prefix("#x")
                        .and_then(|h| u32::from_str_radix(h, 16).ok())
                        .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                        .and_then(char::from_u32),
                };
                if let Some(decoded) = decoded {
                    out.push(Visible {
                        c: Some(decoded),
                        start: i as u32,
                        end: (i + semi + 1) as u32,
                        atomic: atomic_of(&stack),
                    });
                    i += semi + 1;
                    continue;
                }
            }
        }
        out.push(Visible {
            c: Some(c),
            start: i as u32,
            end: i as u32 + 1,
            atomic: atomic_of(&stack),
        });
        i += 1;
    }
    out
}

fn is_gap(v: &Visible) -> bool {
    v.c.is_none_or(char::is_whitespace)
}

/// Cuts `script` into reveal units, words and sentences — the only
/// tokenizer: providers, the aligner, the estimate, the resolver and the
/// caption mapping all go through it.
///
/// - A unit is a maximal run of non-whitespace (tags count as whitespace in
///   SSML), the unit a caption reveals by word.
/// - A word is a unit with its outer punctuation stripped; inner
///   apostrophes and hyphens stay. A unit with no letter or digit left
///   ("—", "&") is no word. Han and kana characters are a word each, a
///   Thai character with its marks too: v1 syncs those scripts by
///   character.
/// - A sentence ends at `.`, `!`, `?` or `…` followed by whitespace or the
///   end (closing quotes allowed between), at `。`, `！` or `？`, or at a
///   blank line — never after a listed abbreviation, never inside a
///   decimal.
pub fn tokenize(script: &str) -> Script {
    let ssml = script.trim_start().to_lowercase().starts_with("<speak");
    let vis = visible(script, ssml);
    let mut units: Vec<[u32; 2]> = Vec::new();
    // Words as visible-index spans: (stripped start, stripped end, token
    // start, token end, unit).
    let mut spans: Vec<(usize, usize, usize, usize, u32)> = Vec::new();
    let mut i = 0usize;
    while i < vis.len() {
        if is_gap(&vis[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < vis.len() && !is_gap(&vis[i]) {
            i += 1;
        }
        let unit = units.len() as u32;
        units.push([vis[start].start, vis[i - 1].end]);
        // Segments: each cluster-script character (and Thai character) with
        // its marks alone, every other run together.
        let mut s = start;
        while s < i {
            let c = vis[s].c.unwrap_or(' ');
            let mut e = s + 1;
            if is_cluster_script(c) || (is_thai(c) && !is_combining(c)) {
                while e < i && vis[e].c.is_some_and(is_combining) {
                    e += 1;
                }
            } else {
                while e < i {
                    let n = vis[e].c.unwrap_or(' ');
                    if is_cluster_script(n) || (is_thai(n) && !is_combining(n)) {
                        break;
                    }
                    e += 1;
                }
            }
            let (mut a, mut b) = (s, e);
            while a < b && vis[a].c.is_some_and(is_margin) {
                a += 1;
            }
            while b > a && vis[b - 1].c.is_some_and(is_margin) {
                b -= 1;
            }
            if vis[a..b]
                .iter()
                .any(|v| v.c.is_some_and(char::is_alphanumeric))
            {
                spans.push((a, b, s, e, unit));
            }
            s = e;
        }
    }

    let words: Vec<ScriptWord> = spans
        .iter()
        .map(|&(a, b, s, e, unit)| ScriptWord {
            text: vis[a..b].iter().filter_map(|v| v.c).collect(),
            chars: [vis[a].start, vis[b - 1].end],
            token: [vis[s].start, vis[e - 1].end],
            unit,
            atomic: vis[a].atomic,
        })
        .collect();

    // Sentence ends: after word k, in the stretch up to the next word.
    let mut sentences = Vec::new();
    let mut first = 0usize;
    for k in 0..spans.len() {
        let (_, b, _, _, _) = spans[k];
        let next = spans.get(k + 1).map(|w| w.0).unwrap_or(vis.len());
        let gap = &vis[b..next];
        let ends = k + 1 == spans.len() || sentence_ends(gap, &words[k].text);
        if ends {
            sentences.push(ScriptSentence {
                words: [first as u32, k as u32],
                chars: [words[first].token[0], words[k].token[1]],
            });
            first = k + 1;
        }
    }
    Script {
        units,
        words,
        sentences,
        ssml,
    }
}

/// Does the stretch after a word (`gap`, up to the next word's first
/// character) end its sentence? A terminator must be followed by
/// whitespace inside the gap: one the next word leans on (".5") ends
/// nothing.
fn sentence_ends(gap: &[Visible], word: &str) -> bool {
    let newlines = gap.iter().filter(|v| v.c == Some('\n')).count();
    if newlines >= 2 {
        return true;
    }
    for (i, v) in gap.iter().enumerate() {
        let Some(c) = v.c else { continue };
        if CJK_TERMINATORS.contains(c) {
            return true;
        }
        if !TERMINATORS.contains(c) {
            continue;
        }
        // A period right after an abbreviation ends nothing.
        if i == 0 && c == '.' {
            let spelled = format!("{}.", word.to_lowercase());
            if ABBREVIATIONS.contains(&spelled.as_str()) {
                continue;
            }
        }
        let mut j = i + 1;
        while j < gap.len()
            && gap[j]
                .c
                .is_some_and(|n| CLOSERS.contains(n) || TERMINATORS.contains(n))
        {
            j += 1;
        }
        if j < gap.len() && is_gap(&gap[j]) {
            return true;
        }
    }
    false
}

/// The sentences of `words` (times filled in) on the script's sentence
/// spans.
pub fn sentences_of(script: &Script, words: &[Word]) -> Vec<Sentence> {
    script
        .sentences
        .iter()
        .filter_map(|s| {
            let first = words.get(s.words[0] as usize)?;
            let last = words.get(s.words[1] as usize)?;
            Some(Sentence {
                start: first.start,
                end: last.end,
                words: s.words,
                chars: s.chars,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Takes and hand corrections on a resource.

/// The script a take says: `speech.text` (what it was bought from), else
/// the recording's `transcript` — None when neither says anything.
pub fn take_script(resource: &crate::ProjectResource) -> Option<String> {
    let spoken = resource
        .extra
        .get("speech")
        .and_then(|s| s.get("text"))
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty());
    spoken
        .map(str::to_string)
        .or_else(|| resource.transcript.clone().filter(|t| !t.trim().is_empty()))
}

/// A resource's `speech.wordFixes`, or a sentence saying what is wrong
/// with them.
pub fn word_fixes(resource: &crate::ProjectResource) -> Result<Vec<WordFix>, String> {
    match resource
        .extra
        .get("speech")
        .and_then(|s| s.get("wordFixes"))
    {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|e| format!("resource \"{}\": speech.wordFixes: {e}", resource.id)),
    }
}

/// Applies hand corrections over a record, in order: each fix moves the
/// edges it names. Answers a sentence for every fix it ignored — made for
/// another script, naming a word the take does not have, or leaving a word
/// that ends before it starts.
pub fn apply_fixes(timing: &mut TakeTiming, fixes: &[WordFix]) -> Vec<String> {
    let mut notes = Vec::new();
    let script = tokenize(&timing.script);
    let hash = script_hash(&timing.script);
    let mut moved = false;
    for fix in fixes {
        if let Some(made_for) = &fix.script_hash {
            if *made_for != hash {
                notes.push(format!(
                    "the fix for word {} was made for another script ({made_for}, the take's is \
                     {hash}) — ignored",
                    fix.word
                ));
                continue;
            }
        }
        let Some(word) = (fix.word as usize)
            .checked_sub(1)
            .and_then(|i| timing.words.get_mut(i))
        else {
            notes.push(format!(
                "a fix names word {} but the take has {} — ignored",
                fix.word,
                timing.words.len()
            ));
            continue;
        };
        let start = fix.start.unwrap_or(word.start);
        let end = fix.end.unwrap_or(word.end);
        if !start.is_finite() || !end.is_finite() || end < start || start < 0.0 {
            notes.push(format!(
                "the fix for word {} \"{}\" would run {start:.3}–{end:.3} s — ignored",
                fix.word, word.text
            ));
            continue;
        }
        word.start = round_us(start);
        word.end = round_us(end);
        word.interpolated = false;
        word.confidence = (timing.confidence != 1.0).then_some(1.0);
        moved = true;
    }
    if moved {
        timing.sentences = sentences_of(&script, &timing.words);
    }
    notes
}

/// Whether the project carries speech fields an older lossless reader may
/// simply ignore: a recording's `transcript`, a take's `speech.wordFixes`
/// (rung 48).
pub fn carries_ignorable_speech_fields(meta: &crate::ProjectMetadata) -> bool {
    meta.composition_settings.speech_clock.is_some()
        || crate::nesting::all_layers(meta)
            .iter()
            .any(|l| l.speech_clock.is_some())
        || meta.resources.iter().flatten().any(|r| {
            r.composition
                .as_ref()
                .is_some_and(|c| c.speech_clock.is_some())
                || r.transcript.is_some()
                || r.extra
                    .get("speech")
                    .and_then(|s| s.get("wordFixes"))
                    .is_some()
        })
}

// ---------------------------------------------------------------------------
// Speech values in time fields (§6).

/// The reader that understands speech values, `during` and the speech
/// generators: an older reader decodes `{"word": 12}` as a musical value,
/// refuses the unknown key, and cannot open the file.
pub const SPEECH_READER: i64 = 50;

/// What a speech value points at in its take.
#[derive(Debug, Clone, PartialEq)]
pub enum SpeechPoint {
    /// Word n, 1-based.
    Word(u32),
    /// The nth (1-based) word whose stripped, case-folded text is `text`.
    WordText { text: String, nth: u32 },
    /// Sentence n, 1-based.
    Sentence(u32),
    /// The word start (end, gap middle, sentence start) nearest `near`
    /// output seconds.
    Snap { to: SpeechSnap, near: f64 },
}

/// What a speech `snap` lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeechSnap {
    Word,
    WordEnd,
    /// The middle of the nearest gap between two words: the safe cut.
    Gap,
    Sentence,
}

impl SpeechSnap {
    pub fn name(self) -> &'static str {
        match self {
            SpeechSnap::Word => "word",
            SpeechSnap::WordEnd => "wordEnd",
            SpeechSnap::Gap => "gap",
            SpeechSnap::Sentence => "sentence",
        }
    }

    pub fn parse(text: &str) -> Option<SpeechSnap> {
        Some(match text {
            "word" => SpeechSnap::Word,
            "wordEnd" => SpeechSnap::WordEnd,
            "gap" => SpeechSnap::Gap,
            "sentence" => SpeechSnap::Sentence,
            _ => return None,
        })
    }
}

/// Which edge of a word or sentence a value means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeechEdge {
    Start,
    End,
}

/// What is added after the edge: seconds, or beats or bars on the scope's
/// beat clock — signed, so `{"beats": -0.5}` counts back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpeechOffset {
    Seconds(f64),
    Beats(f64),
    Bars(f64),
}

/// A time that points into a take: "word 12 of the intro".
#[derive(Debug, Clone, PartialEq)]
pub struct SpeechTime {
    pub at: SpeechPoint,
    /// The take: a resource id, or the id of a layer that plays one. None:
    /// the scope's speech clock.
    pub of: Option<String>,
    pub edge: Option<SpeechEdge>,
    pub offset: Option<SpeechOffset>,
    /// Then onto the scope's beat grid.
    pub quantize: Option<crate::musical::SnapUnit>,
    /// The word an index is meant to be: followed (with a warning) when the
    /// index has moved.
    pub guard: Option<String>,
}

const SPEECH_KEYS: &[&str] = &[
    "word", "sentence", "snap", "near", "of", "nth", "edge", "offset", "quantize", "text",
];

/// Whether a time-field object is a speech value rather than a musical one.
pub fn is_speech_object(object: &Map<String, Value>) -> bool {
    object.contains_key("word")
        || object.contains_key("sentence")
        || object.contains_key("of")
        || object
            .get("snap")
            .and_then(Value::as_str)
            .is_some_and(|s| SpeechSnap::parse(s).is_some())
}

fn whole(value: &Value, key: &str) -> Result<u32, String> {
    match value.as_f64() {
        Some(n) if n.is_finite() && n >= 1.0 && n.fract() == 0.0 && n <= u32::MAX as f64 => {
            Ok(n as u32)
        }
        _ => Err(format!("\"{key}\" must be a whole number from 1")),
    }
}

impl SpeechTime {
    /// The strict decode: exactly one of `word` (a 1-based index or a word's
    /// text), `sentence`, or `snap` with `near`; `nth` only with a text
    /// `word`; `text` (the guard) only with an index; any other key refused
    /// by name.
    pub fn from_object(object: &Map<String, Value>) -> Result<SpeechTime, String> {
        if let Some(key) = object.keys().find(|k| !SPEECH_KEYS.contains(&k.as_str())) {
            return Err(format!("unknown key \"{key}\""));
        }
        let shape = "needs exactly one of \"word\", \"sentence\", or \"snap\" with \"near\"";
        let points = ["word", "sentence", "snap"]
            .iter()
            .filter(|k| object.contains_key(**k))
            .count();
        if points != 1 {
            return Err(shape.into());
        }
        let at = if let Some(word) = object.get("word") {
            if object.contains_key("near") {
                return Err(shape.into());
            }
            match word {
                Value::String(text) => {
                    if object.contains_key("text") {
                        return Err(
                            "\"text\" guards an index; a word given by its text needs none".into(),
                        );
                    }
                    let folded = fold_word(text);
                    if folded.is_empty() {
                        return Err("\"word\" names no word".into());
                    }
                    let nth = match object.get("nth") {
                        None => 1,
                        Some(n) => whole(n, "nth")?,
                    };
                    SpeechPoint::WordText {
                        text: text.clone(),
                        nth,
                    }
                }
                other => {
                    if object.contains_key("nth") {
                        return Err("\"nth\" goes with a word given by its text".into());
                    }
                    SpeechPoint::Word(whole(other, "word")?)
                }
            }
        } else if let Some(sentence) = object.get("sentence") {
            if object.contains_key("near")
                || object.contains_key("nth")
                || object.contains_key("text")
            {
                return Err(shape.into());
            }
            SpeechPoint::Sentence(whole(sentence, "sentence")?)
        } else {
            let to = object
                .get("snap")
                .and_then(Value::as_str)
                .and_then(SpeechSnap::parse)
                .ok_or("\"snap\" must be \"word\", \"wordEnd\", \"gap\" or \"sentence\"")?;
            let near = object
                .get("near")
                .and_then(Value::as_f64)
                .filter(|n| n.is_finite())
                .ok_or(shape)?;
            if object.contains_key("nth") || object.contains_key("text") {
                return Err(shape.into());
            }
            SpeechPoint::Snap { to, near }
        };
        let of = match object.get("of") {
            None => None,
            Some(Value::String(id)) if !id.is_empty() => Some(id.clone()),
            Some(_) => return Err("\"of\" names a take: a resource or layer id".into()),
        };
        let edge = match object.get("edge").map(|e| e.as_str()) {
            None => None,
            Some(Some("start")) => Some(SpeechEdge::Start),
            Some(Some("end")) => Some(SpeechEdge::End),
            Some(_) => return Err("\"edge\" must be \"start\" or \"end\"".into()),
        };
        let offset = match object.get("offset") {
            None => None,
            Some(Value::Number(n)) => match n.as_f64() {
                Some(n) if n.is_finite() => Some(SpeechOffset::Seconds(n)),
                _ => return Err("\"offset\" must be a finite number".into()),
            },
            Some(Value::Object(o)) => {
                let finite = |v: &Value| v.as_f64().filter(|n| n.is_finite());
                match (o.len(), o.get("beats"), o.get("bars")) {
                    (1, Some(n), None) => Some(SpeechOffset::Beats(
                        finite(n).ok_or("offset: \"beats\" must be a finite number")?,
                    )),
                    (1, None, Some(n)) => Some(SpeechOffset::Bars(
                        finite(n).ok_or("offset: \"bars\" must be a finite number")?,
                    )),
                    _ => return Err("offset: {\"beats\": n} or {\"bars\": n}".into()),
                }
            }
            Some(_) => return Err("\"offset\" is seconds or {\"beats\": n}".into()),
        };
        let quantize = match object.get("quantize").map(|q| q.as_str()) {
            None => None,
            Some(Some(unit)) => Some(
                crate::musical::SnapUnit::parse(unit)
                    .ok_or("\"quantize\" must be \"beat\", \"downbeat\" or \"bar\"")?,
            ),
            Some(None) => {
                return Err("\"quantize\" must be \"beat\", \"downbeat\" or \"bar\"".into())
            }
        };
        let guard = match object.get("text") {
            None => None,
            Some(Value::String(t)) if matches!(at, SpeechPoint::Word(_)) => Some(t.clone()),
            Some(_) => {
                return Err(
                    "\"text\" guards a word index: {\"word\": 12, \"text\": \"Euclid\"}".into(),
                )
            }
        };
        Ok(SpeechTime {
            at,
            of,
            edge,
            offset,
            quantize,
            guard,
        })
    }

    /// The canonical object (keys sorted, integers as integers).
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        let number = |n: f64| -> Value {
            if n.fract() == 0.0 && n.abs() < 9.0e15 {
                Value::from(n as i64)
            } else {
                Value::from(n)
            }
        };
        match &self.at {
            SpeechPoint::Word(n) => {
                map.insert("word".into(), Value::from(*n));
            }
            SpeechPoint::WordText { text, nth } => {
                map.insert("word".into(), Value::from(text.clone()));
                if *nth != 1 {
                    map.insert("nth".into(), Value::from(*nth));
                }
            }
            SpeechPoint::Sentence(n) => {
                map.insert("sentence".into(), Value::from(*n));
            }
            SpeechPoint::Snap { to, near } => {
                map.insert("snap".into(), Value::from(to.name()));
                map.insert("near".into(), number(*near));
            }
        }
        if let Some(of) = &self.of {
            map.insert("of".into(), Value::from(of.clone()));
        }
        if let Some(edge) = self.edge {
            map.insert(
                "edge".into(),
                Value::from(match edge {
                    SpeechEdge::Start => "start",
                    SpeechEdge::End => "end",
                }),
            );
        }
        match self.offset {
            Some(SpeechOffset::Seconds(n)) => {
                map.insert("offset".into(), number(n));
            }
            Some(SpeechOffset::Beats(n)) => {
                map.insert("offset".into(), serde_json::json!({ "beats": number(n) }));
            }
            Some(SpeechOffset::Bars(n)) => {
                map.insert("offset".into(), serde_json::json!({ "bars": number(n) }));
            }
            None => {}
        }
        if let Some(q) = self.quantize {
            map.insert("quantize".into(), Value::from(q.name()));
        }
        if let Some(t) = &self.guard {
            map.insert("text".into(), Value::from(t.clone()));
        }
        Value::Object(map)
    }

    pub fn canonical_text(&self) -> String {
        self.to_value().to_string()
    }

    /// The core's own wording, before the take's words are known: "word 12
    /// of intro", "the end of sentence 2", "word “Euclid” (2nd)".
    pub fn label(&self) -> String {
        let point = match &self.at {
            SpeechPoint::Word(n) => format!("word {n}"),
            SpeechPoint::WordText { text, nth } if *nth == 1 => {
                format!("word \u{201C}{text}\u{201D}")
            }
            SpeechPoint::WordText { text, nth } => format!("word \u{201C}{text}\u{201D} ({nth}th)"),
            SpeechPoint::Sentence(n) => format!("sentence {n}"),
            SpeechPoint::Snap { to, near } => format!(
                "the {} nearest {near} s",
                match to {
                    SpeechSnap::Word => "word start",
                    SpeechSnap::WordEnd => "word end",
                    SpeechSnap::Gap => "gap",
                    SpeechSnap::Sentence => "sentence start",
                }
            ),
        };
        let mut out = match self.edge {
            Some(SpeechEdge::End) => format!("the end of {point}"),
            _ => point,
        };
        if let Some(of) = &self.of {
            out.push_str(&format!(" of {of}"));
        }
        match self.offset {
            Some(SpeechOffset::Seconds(n)) if n != 0.0 => out.push_str(&format!(" {n:+} s")),
            Some(SpeechOffset::Beats(n)) => out.push_str(&format!(" {n:+} beats")),
            Some(SpeechOffset::Bars(n)) => out.push_str(&format!(" {n:+} bars")),
            _ => {}
        }
        if let Some(q) = self.quantize {
            out.push_str(&format!(", on the nearest {}", q.name()));
        }
        out
    }
}

impl schemars::JsonSchema for SpeechTime {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SpeechTime".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "properties": {
                "word": { "type": ["integer", "string"] },
                "nth": { "type": "integer", "minimum": 1 },
                "sentence": { "type": "integer", "minimum": 1 },
                "snap": { "type": "string", "enum": ["word", "wordEnd", "gap", "sentence"] },
                "near": { "type": "number" },
                "of": { "type": "string" },
                "edge": { "type": "string", "enum": ["start", "end"] },
                "offset": { "type": ["number", "object"] },
                "quantize": { "type": "string", "enum": ["beat", "downbeat", "bar"] },
                "text": { "type": "string" }
            },
            "additionalProperties": false
        })
    }
}

/// A word as speech values compare it: case-folded, outer punctuation off.
pub fn fold_word(text: &str) -> String {
    tokenize(text)
        .words
        .first()
        .map(|w| w.text.to_lowercase())
        .unwrap_or_default()
}

/// A word a `during` names: by index or by text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum WordRef {
    Index(u32),
    Text(String),
}

/// A layer that lasts while a span of a take is said: the span's start −
/// `lead` to its end + `hold` — `startTime` and `duration` together.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct During {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word: Option<WordRef>,
    /// `[first, last]`, 1-based, inclusive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub words: Option<[u32; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sentence: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub of: Option<String>,
    /// Seconds before the span (default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead: Option<f64>,
    /// Seconds after it (default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<f64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The take a scope's speech values count on when they name none: a layer
/// that plays it, or a resource placed at `startTime`. On
/// `compositionSettings`, a composition, or a layer (an override).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SpeechClock {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "resourceID"
    )]
    pub resource_id: Option<String>,
    /// Where the take's source second 0 sits (with `resourceID`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<f64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A caption reveal on a take's words: `{"of": "intro"}`, optionally pinned
/// to `words` (1-based, inclusive) or a `sentence`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RevealSync {
    /// The take: a resource id, or a layer that plays one. None: the
    /// scope's speech clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub of: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub words: Option<[u32; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sentence: Option<u32>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Captions made from a narration (docs/SPEECH-SYNC.md §7.2), on the
/// layer that plays the take: one caption layer per sentence (or phrase),
/// split when longer than `maxWords` or `maxSeconds`, each revealed on its
/// words.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SpeechCaptions {
    /// `sentence` (default) or `phrase` (split at every comma first).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_words: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_seconds: Option<f64>,
    /// Seconds a caption appears before its first word (default 0.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead: Option<f64>,
    /// Seconds it stays after its last word (default 0.25), never past the
    /// next caption's start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<f64>,
    /// The reveal each caption carries; its `sync` is filled in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reveal: Option<crate::TextReveal>,
    /// The caption style each carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<crate::SubtitleStyle>,
    /// Pieces a person deleted: not made again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip: Option<Vec<u32>>,
    /// How many pieces the last expansion made — the app form only, so a
    /// save can tell a deleted caption from one never made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub made: Option<u32>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Whether the project needs the speech reader (rung 50): a speech value a
/// save keeps, a `during`, a reveal `sync` or `captions` — an older reader
/// would render a different film.
pub fn needs_speech_reader(meta: &crate::ProjectMetadata) -> bool {
    let holds_speech = |specs: &Option<crate::musical::TimeSpecs>,
                        number: &dyn Fn(&str) -> Option<f64>| {
        specs
            .iter()
            .flatten()
            .any(|(key, entry)| entry.speech().is_some() && entry.holds(number(key)))
    };
    let synced = |style: &Option<crate::SubtitleStyle>| {
        style
            .as_ref()
            .and_then(|s| s.reveal.as_ref())
            .is_some_and(|r| r.sync.is_some())
    };
    // A generator firing on words, a range of them, a sequence entry until
    // one or per sentence.
    let speech_value = |v: &Option<Value>| {
        v.as_ref().and_then(Value::as_object).is_some_and(|o| {
            o.contains_key("words") || o.contains_key("sentences") || o.contains_key("sentence")
        })
    };
    let fires = |on: &Option<Value>, every: &Option<Value>, range: &Option<Value>| {
        speech_value(on) || speech_value(every) || speech_value(range)
    };
    let list = |field: &Option<crate::musical::OneOrMany<crate::musical::Pulse>>| match field {
        None => Vec::new(),
        Some(crate::musical::OneOrMany::One(one)) => vec![one.clone()],
        Some(crate::musical::OneOrMany::Many(many)) => many.clone(),
    };
    let shakes = |field: &Option<crate::musical::OneOrMany<crate::musical::Shake>>| match field {
        None => Vec::new(),
        Some(crate::musical::OneOrMany::One(one)) => vec![one.clone()],
        Some(crate::musical::OneOrMany::Many(many)) => many.clone(),
    };
    let sequenced = |seq: &Option<Vec<crate::musical::SequenceEntry>>| {
        seq.iter()
            .flatten()
            .any(|e| e.until.is_some() || e.per.is_some())
    };
    let flashes = match &meta.composition_settings.flash {
        None => Vec::new(),
        Some(crate::musical::OneOrMany::One(one)) => vec![one.clone()],
        Some(crate::musical::OneOrMany::Many(many)) => many.clone(),
    };
    flashes.iter().any(|f| fires(&f.on, &f.every, &f.range))
        || sequenced(&meta.composition_settings.sequence)
        || meta
            .resources
            .iter()
            .flatten()
            .filter_map(|r| r.composition.as_ref())
            .any(|c| sequenced(&c.sequence))
        || crate::nesting::all_layers(meta).iter().any(|layer| {
            list(&layer.pulse)
                .iter()
                .any(|p| fires(&p.on, &p.every, &p.range))
                || shakes(&layer.shake)
                    .iter()
                    .any(|p| fires(&p.on, &p.every, &p.range))
        })
        || meta
            .composition_settings
            .subtitle_reveal
            .as_ref()
            .is_some_and(|r| r.sync.is_some())
        || meta
            .resources
            .iter()
            .flatten()
            .any(|r| synced(&r.caption_style))
        || crate::nesting::all_layers(meta).iter().any(|layer| {
            layer.during.is_some()
                || layer.captions.is_some()
                || synced(&layer.caption_style)
                || holds_speech(&layer.time_specs, &|k| {
                    crate::musical::layer_number(layer, k)
                })
                || layer.keyframes.iter().any(|k| {
                    holds_speech(&k.time_specs, &|key| {
                        crate::musical::keyframe_number(k, key)
                    })
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(script: &str) -> Vec<String> {
        tokenize(script).words.into_iter().map(|w| w.text).collect()
    }

    fn slice(script: &str, span: [u32; 2]) -> String {
        script
            .chars()
            .skip(span[0] as usize)
            .take((span[1] - span[0]) as usize)
            .collect()
    }

    #[test]
    fn the_designs_example_cuts_as_it_says() {
        let script = "Meet Euclid. Geometry, proved step by step.";
        let t = tokenize(script);
        assert_eq!(
            words(script),
            ["Meet", "Euclid", "Geometry", "proved", "step", "by", "step"]
        );
        assert_eq!(t.words[0].chars, [0, 4]);
        assert_eq!(t.words[1].chars, [5, 11]);
        assert_eq!(t.words[2].chars, [13, 21]);
        assert_eq!(t.sentences.len(), 2);
        assert_eq!(t.sentences[0].words, [0, 1]);
        assert_eq!(t.sentences[0].chars, [0, 12]);
        assert_eq!(t.sentences[1].words, [2, 6]);
        assert_eq!(t.sentences[1].chars, [13, 43]);
        assert_eq!(t.units.len(), 7);
    }

    /// Margins go, inner apostrophes and hyphens stay, a token of
    /// punctuation alone is a unit but no word.
    #[test]
    fn margins_strip_and_punctuation_alone_is_no_word() {
        assert_eq!(
            words("“Don't” — step-by-step (really)… & more!"),
            ["Don't", "step-by-step", "really", "more"]
        );
        let t = tokenize("Wait — what?");
        assert_eq!(t.units.len(), 3);
        assert_eq!(t.words.len(), 2);
        assert_eq!(t.words[1].unit, 2);
    }

    /// `chars` count Unicode scalars: what ElevenLabs' `characters` and
    /// Swift's `unicodeScalars` count — an emoji is one, a combining mark
    /// one more, a stressed Cyrillic vowel two.
    #[test]
    fn chars_are_scalar_offsets() {
        let script = "🎉 Café nai\u{0308}ve — стои\u{0301}т дёшево.";
        let t = tokenize(script);
        let texts: Vec<&str> = t.words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["Café", "nai\u{0308}ve", "стои\u{0301}т", "дёшево"]);
        // The emoji is a unit (a caption reveals it) but has no letter.
        assert_eq!(t.units[0], [0, 1]);
        for w in &t.words {
            assert_eq!(slice(script, w.chars), w.text, "{w:?}");
        }
        assert_eq!(t.words[0].chars, [2, 6]);
        assert_eq!(t.words[1].chars, [7, 13]); // n a i ̈ v e: six scalars
        assert_eq!(t.words[2].chars, [16, 22]); // с т о и ́ т
    }

    #[test]
    fn sentences_skip_abbreviations_and_decimals() {
        let t = tokenize("Dr. Smith paid $3.50, e.g. less. Really?! \"Yes.\" Then\n\nnew para");
        let spans: Vec<[u32; 2]> = t.sentences.iter().map(|s| s.words).collect();
        let texts: Vec<String> = t.words.iter().map(|w| w.text.clone()).collect();
        assert_eq!(
            texts,
            [
                "Dr", "Smith", "paid", "$3.50", "e.g", "less", "Really", "Yes", "Then", "new",
                "para"
            ]
        );
        assert_eq!(spans, [[0, 5], [6, 6], [7, 7], [8, 8], [9, 10]]);
        // An ellipsis inside a word ends nothing; one before a space does.
        assert_eq!(tokenize("wait…what").sentences.len(), 1);
        assert_eq!(tokenize("wait… what").sentences.len(), 2);
        // A version number is no sentence end either, nor a point the next
        // word leans on.
        assert_eq!(tokenize("Version 3.2 ships.").sentences.len(), 1);
        assert_eq!(tokenize("It costs .5 dollars.").sentences.len(), 1);
    }

    #[test]
    fn ssml_words_come_from_text_nodes() {
        let script = r#"<speak>Hello <emphasis level="strong">world</emphasis>! It costs <say-as interpret-as="cardinal">12</say-as> &amp; more.<break time="1s"/>Done.</speak>"#;
        let t = tokenize(script);
        assert!(t.ssml);
        let texts: Vec<&str> = t.words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(
            texts,
            ["Hello", "world", "It", "costs", "12", "more", "Done"]
        );
        for w in &t.words {
            assert_eq!(slice(script, w.chars), w.text, "chars index the raw SSML");
        }
        let twelve = &t.words[4];
        let say_as = script.find("<say-as").unwrap() as u32;
        assert_eq!(twelve.atomic, Some(say_as));
        assert!(t.words[1].atomic.is_none(), "emphasis is not atomic");
        assert_eq!(t.sentences.len(), 3);
        // An entity is one character of a word.
        let amp = tokenize("<speak>R&amp;D works</speak>");
        assert_eq!(amp.words[0].text, "R&D");
        assert_eq!(amp.words[0].chars, [7, 14]);
    }

    #[test]
    fn scripts_without_spaces_sync_by_character() {
        let t = tokenize("你好。世界！ OK");
        let texts: Vec<&str> = t.words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["你", "好", "世", "界", "OK"]);
        assert_eq!(t.sentences.len(), 3);
        assert_eq!(t.units.len(), 2);
        let thai = tokenize("สวัสดี");
        assert_eq!(thai.words.len(), 4, "{:?}", thai.words);
    }

    #[test]
    fn the_script_hash_is_eight_hex_of_sha256() {
        assert_eq!(script_hash(""), "e3b0c442");
        assert_eq!(script_hash("abc"), "ba7816bf");
    }

    #[test]
    fn sources_round_trip_as_text() {
        for text in [
            "provider:elevenlabs",
            "local:kokoro-en-q8/af_heart",
            "aligned",
            "transcribed:openai",
            "estimate",
            "future:thing",
        ] {
            let source = TimingSource::parse(text);
            assert_eq!(source.as_text(), text);
            let json = serde_json::to_string(&source).unwrap();
            assert_eq!(serde_json::from_str::<TimingSource>(&json).unwrap(), source);
        }
        assert_eq!(TimingSource::parse("aligned"), TimingSource::Aligned);
        assert_eq!(TimingSource::parse("estimate"), TimingSource::Estimate);
        assert_eq!(
            TimingSource::parse("provider:google"),
            TimingSource::Provider("google".into())
        );
        assert!(TimingSource::parse("provider:google").is_authoritative());
        assert!(!TimingSource::Aligned.is_authoritative());
    }

    fn record(script: &str) -> TakeTiming {
        let t = tokenize(script);
        let words: Vec<Word> = t
            .words
            .iter()
            .enumerate()
            .map(|(i, w)| Word {
                text: w.text.clone(),
                start: i as f64,
                end: i as f64 + 0.5,
                chars: w.chars,
                confidence: None,
                interpolated: false,
                prominence: None,
            })
            .collect();
        TakeTiming {
            timing_version: TIMING_VERSION,
            audio: MediaSource::default(),
            script: script.into(),
            script_hash: script_hash(script),
            duration: 10.0,
            source: TimingSource::Aligned,
            estimated: false,
            confidence: 0.7,
            sentences: sentences_of(&t, &words),
            words,
            aligner_version: Some(1),
            extra: Map::new(),
        }
    }

    #[test]
    fn fixes_move_the_named_edges_and_say_what_they_ignore() {
        let mut timing = record("One two. Three four.");
        let fixes: Vec<WordFix> = serde_json::from_value(serde_json::json!([
            {"word": 2, "end": 1.8},
            {"word": 3, "start": 2.2, "scriptHash": script_hash("One two. Three four.")},
            {"word": 4, "start": 9.0, "scriptHash": "deadbeef"},
            {"word": 9, "start": 1.0},
            {"word": 1, "start": 2.0, "end": 1.0}
        ]))
        .unwrap();
        let notes = apply_fixes(&mut timing, &fixes);
        assert_eq!(timing.words[1].end, 1.8);
        assert_eq!(timing.words[2].start, 2.2);
        assert_eq!(timing.words[2].confidence, Some(1.0));
        assert_eq!(timing.words[3].start, 3.0, "a fix for another script");
        assert_eq!(timing.sentences[0].end, 1.8);
        assert_eq!(timing.sentences[1].start, 2.2);
        assert_eq!(notes.len(), 3, "{notes:?}");
        assert!(notes[0].contains("another script"));
        assert!(notes[1].contains("the take has 4"));
    }

    /// The record round-trips, keeping what a newer writer added.
    #[test]
    fn the_record_round_trips_with_unknown_keys() {
        let mut timing = record("Hello there.");
        timing.words[0].interpolated = true;
        timing
            .extra
            .insert("audioOffset".into(), serde_json::json!(0.025));
        let text = serde_json::to_string(&timing).unwrap();
        assert!(text.contains("\"timingVersion\":1"));
        assert!(text.contains("\"interpolated\":true"));
        assert!(!text.contains("prominence"));
        let back: TakeTiming = serde_json::from_str(&text).unwrap();
        assert_eq!(back, timing);
    }
}

//! Word timing from the providers that report it, and the one mapper from
//! what a source said onto the script's own words (docs/SPEECH-SYNC.md
//! §2). ElevenLabs answers per character (`/with-timestamps`); Google
//! answers per `<mark>` (`v1beta1`, `enableTimePointing`), for the voice
//! families that take SSML marks; OpenAI and Chirp answer with audio only
//! and their takes are aligned offline (S5). Timing adds no request: it
//! comes back with the audio it describes.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use promo_model::speech::{
    round_us, sentences_of, tokenize, Script, SpokenWord, TakeTiming, TimingSource, Word,
    TIMING_VERSION,
};

/// A take as a provider answered it: the audio bytes, and the words it
/// said when the provider says when.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Take {
    pub audio: Vec<u8>,
    pub spoken: Option<Spoken>,
    /// Why a take that could have been timed was not (fewer marks than
    /// words, a refused SSML request) — a report line, never an error.
    pub note: Option<String>,
}

/// What a source said, before it is mapped onto the script's words.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spoken {
    pub words: Vec<SpokenWord>,
    pub source: TimingSource,
    /// 0–1, for the take as a whole.
    pub confidence: f64,
    /// The text the words' `chars` index: what the synthesizer was given
    /// (before the stress spellings were converted at the wire).
    pub against: String,
}

// ---------------------------------------------------------------------------
// ElevenLabs: characters with times.

/// A take from ElevenLabs' `/with-timestamps` answer for `text` (what
/// the synthesizer was given): the audio, and each script word's start and
/// end from its characters. The answer's `alignment` is over the
/// characters SENT (`normalized_alignment` is over ElevenLabs' own
/// normalization, which maps back to nothing); those go back through the
/// stress spellings' map to the script's characters.
pub fn elevenlabs_take(text: &str, answer: &Value) -> Result<Take, String> {
    let audio = answer
        .get("audio_base64")
        .and_then(Value::as_str)
        .ok_or("ElevenLabs returned no audio")?;
    let audio = crate::base64_decode(audio)?;
    let spoken = answer
        .get("alignment")
        .and_then(|a| elevenlabs_spoken(text, a));
    let note = spoken
        .is_none()
        .then(|| "ElevenLabs returned no usable timing — the take will be aligned".to_string());
    Ok(Take {
        audio,
        spoken,
        note,
    })
}

fn elevenlabs_spoken(text: &str, alignment: &Value) -> Option<Spoken> {
    let characters = alignment.get("characters")?.as_array()?;
    let starts = alignment.get("character_start_times_seconds")?.as_array()?;
    let ends = alignment.get("character_end_times_seconds")?.as_array()?;
    if characters.len() != starts.len() || characters.len() != ends.len() {
        return None;
    }
    // Each answered character, as scalars, with its times.
    let mut got: Vec<(char, f64, f64)> = Vec::new();
    for ((c, s), e) in characters.iter().zip(starts).zip(ends) {
        let (Some(c), Some(s), Some(e)) = (c.as_str(), s.as_f64(), e.as_f64()) else {
            return None;
        };
        for scalar in c.chars() {
            got.push((scalar, s, e));
        }
    }
    let (sent, to_script) = crate::stress::marking_stress_mapped(text);
    let sent: Vec<char> = sent.chars().collect();
    let answered: Vec<char> = got.iter().map(|g| g.0).collect();
    // Each answered character's sent index: one for one when ElevenLabs
    // echoed the text, else the edit-distance alignment's.
    let (to_sent, exact) = if answered == sent {
        ((0..sent.len()).map(Some).collect::<Vec<_>>(), true)
    } else {
        (align_chars(&answered, &sent), false)
    };
    let script = tokenize(text);
    let mut words = Vec::new();
    for word in &script.words {
        let (a, b) = (word.chars[0], word.chars[1]);
        let mut start = f64::INFINITY;
        let mut end = f64::NEG_INFINITY;
        for (k, at) in to_sent.iter().enumerate() {
            let Some(at) = at else { continue };
            let Some(&scalar) = to_script.get(*at) else {
                continue;
            };
            if scalar < a || scalar >= b {
                continue;
            }
            start = start.min(got[k].1);
            if !got[k].0.is_whitespace() {
                end = end.max(got[k].2);
            }
        }
        if start.is_finite() && end.is_finite() && end >= start {
            words.push(SpokenWord {
                text: word.text.clone(),
                start: round_us(start),
                end: Some(round_us(end)),
                chars: Some(word.chars),
            });
        }
    }
    (!words.is_empty()).then(|| Spoken {
        words,
        source: TimingSource::Provider("elevenlabs".into()),
        confidence: if exact { 1.0 } else { 0.9 },
        against: text.to_string(),
    })
}

/// For each character of `answered`, the index of the character of `sent`
/// it stands for — a Levenshtein alignment with backpointers, ties to the
/// diagonal, then to the lower index. An answered character the text never
/// had maps to None.
fn align_chars(answered: &[char], sent: &[char]) -> Vec<Option<usize>> {
    let (n, m) = (answered.len(), sent.len());
    let width = m + 1;
    let mut cost = vec![0u32; (n + 1) * width];
    for (i, row) in cost.chunks_mut(width).enumerate() {
        row[0] = i as u32;
    }
    for (j, cell) in cost.iter_mut().take(width).enumerate() {
        *cell = j as u32;
    }
    for i in 1..=n {
        for j in 1..=m {
            let same = answered[i - 1] == sent[j - 1]
                || fold_char(answered[i - 1]) == fold_char(sent[j - 1]);
            let diagonal = cost[(i - 1) * width + j - 1] + u32::from(!same);
            let up = cost[(i - 1) * width + j] + 1;
            let left = cost[i * width + j - 1] + 1;
            cost[i * width + j] = diagonal.min(up).min(left);
        }
    }
    let mut out = vec![None; n];
    let (mut i, mut j) = (n, m);
    while i > 0 && j > 0 {
        let here = cost[i * width + j];
        let same =
            answered[i - 1] == sent[j - 1] || fold_char(answered[i - 1]) == fold_char(sent[j - 1]);
        if here == cost[(i - 1) * width + j - 1] + u32::from(!same) {
            out[i - 1] = Some(j - 1);
            i -= 1;
            j -= 1;
        } else if here == cost[(i - 1) * width + j] + 1 {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Google: a `<mark>` before each word.

/// The Google voice classes that take SSML `<mark>` timepoints — the
/// third segment of a classic voice's name (`en-US-Neural2-F`). Chirp,
/// Journey and anything unknown take no marks; a bare (Gemini) name takes
/// none.
const MARKED_CLASSES: &[&str] = &[
    "Standard", "Wavenet", "Neural2", "News", "Polyglot", "Studio",
];

/// Does this Google voice report where each word starts? Decided by name.
pub fn google_supports_marks(voice: &str) -> bool {
    let name = crate::google_voice_name(voice);
    let parts: Vec<&str> = name.split('-').collect();
    parts.len() >= 4
        && crate::google_locale(name) == format!("{}-{}", parts[0], parts[1])
        && MARKED_CLASSES.contains(&parts[2])
}

/// The SSML a marked Google request sends for `text`, and the script word
/// each mark stands for (mark `w{k}` before word k). Plain text becomes
/// `<speak>`, then the text with the stress spellings converted and XML
/// escaped, a mark before each word, whitespace as written. A script that
/// is already SSML gets marks in its text only: a word inside `<say-as>`,
/// `<sub>` or `<phoneme>` is marked once, before the element (a mark
/// inside one is invalid), and the element's other words share that mark.
pub fn google_ssml_marked(text: &str) -> (String, Vec<usize>) {
    let script = tokenize(text);
    let mut marks: Vec<usize> = Vec::new();
    let mut out = String::new();
    if script.ssml {
        // Insert at raw scalar indices.
        let mut at: Vec<(u32, usize)> = Vec::new();
        let mut seen_atomic: Vec<u32> = Vec::new();
        for (k, word) in script.words.iter().enumerate() {
            match word.atomic {
                Some(element) if seen_atomic.contains(&element) => {}
                Some(element) => {
                    seen_atomic.push(element);
                    at.push((element, k));
                }
                None => at.push((word.chars[0], k)),
            }
        }
        let mut next = at.iter().peekable();
        for (i, c) in text.chars().enumerate() {
            while let Some(&&(index, k)) = next.peek() {
                if index as usize != i {
                    break;
                }
                out.push_str(&format!("<mark name=\"w{k}\"/>"));
                marks.push(k);
                next.next();
            }
            out.push(c);
        }
        return (out, marks);
    }
    let (marked, to_script) = crate::stress::marking_stress_mapped(text);
    let mut starts = script.words.iter().enumerate().peekable();
    out.push_str("<speak>");
    for (j, c) in marked.chars().enumerate() {
        let scalar = to_script.get(j).copied().unwrap_or(u32::MAX);
        while let Some((k, word)) = starts.peek() {
            if word.chars[0] > scalar {
                break;
            }
            if word.chars[0] == scalar {
                out.push_str(&format!("<mark name=\"w{k}\"/>"));
                marks.push(*k);
            }
            starts.next();
        }
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out.push_str("</speak>");
    (out, marks)
}

/// Is this refusal of a marked request about the marks — a 400 that names
/// SSML, marks or time pointing — so the plain request may be tried once?
pub fn google_refused_marks(error: &str) -> bool {
    let lower = error.to_lowercase();
    lower.contains("answered 400")
        && ["ssml", "mark", "timepoint", "time_pointing", "timepointing"]
            .iter()
            .any(|word| lower.contains(word))
}

/// A take from Google's `v1beta1` answer to a marked request for `text`:
/// the audio, and each marked word's start (its mark's time). A word ends
/// where the next mark is — the pull-back to the end of voiced energy
/// waits for the aligner's VAD (S5) — and the last where the take ends.
/// Fewer timepoints than marks: the audio is used, the marks are not, and
/// the take is aligned instead.
pub fn google_take(text: &str, answer: &Value, marks: &[usize]) -> Result<Take, String> {
    let audio = answer
        .get("audioContent")
        .and_then(Value::as_str)
        .ok_or("Google returned no audio content")?;
    let audio = crate::base64_decode(audio)?;
    let points: Vec<(usize, f64)> = answer
        .get("timepoints")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|p| {
                    let name = p.get("markName")?.as_str()?;
                    let k: usize = name.strip_prefix('w')?.parse().ok()?;
                    Some((k, p.get("timeSeconds")?.as_f64()?))
                })
                .collect()
        })
        .unwrap_or_default();
    if points.len() < marks.len() || marks.is_empty() {
        return Ok(Take {
            audio,
            spoken: None,
            note: Some(format!(
                "Google returned {} of {} word marks — the take will be aligned",
                points.len(),
                marks.len()
            )),
        });
    }
    let script = tokenize(text);
    let mut points = points;
    points.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
    let words: Vec<SpokenWord> = points
        .iter()
        .enumerate()
        .filter_map(|(i, &(k, time))| {
            let word = script.words.get(k)?;
            Some(SpokenWord {
                text: word.text.clone(),
                start: round_us(time),
                end: points.get(i + 1).map(|next| round_us(next.1)),
                chars: Some(word.chars),
            })
        })
        .collect();
    Ok(Take {
        audio,
        spoken: Some(Spoken {
            words,
            source: TimingSource::Provider("google".into()),
            confidence: 0.95,
            against: text.to_string(),
        }),
        note: None,
    })
}

// ---------------------------------------------------------------------------
// The mapper (§2.5).

/// Lowercase, accents and combining marks off: what two spellings of one
/// word share. Covers Latin-1 and Latin Extended-A, Cyrillic ё/й, and every
/// combining mark — the cases the providers' normalizations produce.
fn fold_char(c: char) -> char {
    let lower = c.to_lowercase().next().unwrap_or(c);
    match lower {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
        'ç' | 'ć' | 'č' => 'c',
        'ď' => 'd',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ę' | 'ě' => 'e',
        'ì' | 'í' | 'î' | 'ï' | 'ī' => 'i',
        'ñ' | 'ń' | 'ň' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => 'o',
        'ř' => 'r',
        'ś' | 'š' => 's',
        'ť' => 't',
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' => 'u',
        'ý' | 'ÿ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        'ł' => 'l',
        'ё' => 'е',
        'й' => 'и',
        other => other,
    }
}

fn is_mark(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036F)
}

/// A word as the mapper compares it: folded, marks, apostrophes and
/// hyphens off.
fn fold(word: &str) -> String {
    word.chars()
        .filter(|c| !is_mark(*c) && !matches!(c, '\'' | '’' | '-' | '‐'))
        .map(fold_char)
        .filter(|c| c.is_alphanumeric())
        .collect()
}

fn edit_distance(a: &[char], b: &[char]) -> usize {
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, &x) in a.iter().enumerate() {
        let mut row = vec![i + 1; b.len() + 1];
        for (j, &y) in b.iter().enumerate() {
            row[j + 1] = (previous[j] + usize::from(x != y))
                .min(previous[j + 1] + 1)
                .min(row[j] + 1);
        }
        previous = row;
    }
    previous[b.len()]
}

/// A script word a source may say as several: numbers, money, percent,
/// dates, acronyms.
fn spelled_out(word: &str) -> bool {
    word.chars().any(|c| {
        c.is_ascii_digit() || matches!(c, '$' | '€' | '£' | '¥' | '₽' | '%' | '&' | '@' | '#')
    }) || word.chars().filter(|c| c.is_uppercase()).count() >= 2
}

/// The longest run of spoken words one script word may stand for.
const MERGE: usize = 8;

#[derive(Clone, Copy, PartialEq)]
enum Move {
    Start,
    Match,
    Merge(u8),
    Delete,
    Insert,
}

/// For each script word, the spoken words it was said as (a run, possibly
/// empty), by a monotonic alignment of the two token lists: match or
/// substitute (1:1), one script word to up to eight spoken ones (1:k),
/// a script word nothing said, a spoken word the script lacks. Costs:
/// 0 for the same word folded; 0 for a compound said apart, or 0.2 for a
/// number, money, percent or acronym said as several; 0.5 within an edit
/// distance of a third of the word; otherwise 1 a substitution, deletion
/// or insertion. Ties go to the lower index.
fn align_words(script: &[String], spoken: &[String]) -> Vec<std::ops::Range<usize>> {
    let (n, m) = (script.len(), spoken.len());
    let s: Vec<String> = script.iter().map(|w| fold(w)).collect();
    let p: Vec<String> = spoken.iter().map(|w| fold(w)).collect();
    let width = m + 1;
    let mut cost = vec![f64::INFINITY; (n + 1) * width];
    let mut from = vec![Move::Start; (n + 1) * width];
    cost[0] = 0.0;
    let substitute = |a: &str, b: &str| -> f64 {
        if a == b {
            return 0.0;
        }
        let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
        let longest = a.len().max(b.len()).max(1);
        if edit_distance(&a, &b) * 3 <= longest {
            0.5
        } else {
            1.0
        }
    };
    for i in 0..=n {
        for j in 0..=m {
            let here = cost[i * width + j];
            if !here.is_finite() {
                continue;
            }
            let mut relax = |ii: usize, jj: usize, add: f64, how: Move| {
                let at = ii * width + jj;
                let value = here + add;
                if value < cost[at] - 1e-12 {
                    cost[at] = value;
                    from[at] = how;
                }
            };
            if i < n && j < m {
                relax(i + 1, j + 1, substitute(&s[i], &p[j]), Move::Match);
            }
            if i < n {
                for k in 2..=MERGE.min(m - j.min(m)) {
                    let joined: String = p[j..j + k].concat();
                    let add = if joined == s[i] {
                        0.0
                    } else if spelled_out(&script[i]) {
                        0.2
                    } else {
                        continue;
                    };
                    relax(i + 1, j + k, add, Move::Merge(k as u8));
                }
                relax(i + 1, j, 1.0, Move::Delete);
            }
            if j < m {
                relax(i, j + 1, 1.0, Move::Insert);
            }
        }
    }
    let mut runs = vec![0..0; n];
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        match from[i * width + j] {
            Move::Match => {
                runs[i - 1] = j - 1..j;
                i -= 1;
                j -= 1;
            }
            Move::Merge(k) => {
                let k = k as usize;
                runs[i - 1] = j - k..j;
                i -= 1;
                j -= k;
            }
            Move::Delete => {
                runs[i - 1] = j..j;
                i -= 1;
            }
            Move::Insert => j -= 1,
            Move::Start => break,
        }
    }
    runs
}

/// The record of a take saying `script_text`, from what a source said and
/// the take's measured `duration` (§2.5). The spoken words' `chars` index
/// `spoken.against`: the script itself, or the script with its stress
/// spellings converted (what the apps hand the synthesizer); either maps
/// straight onto the script's words. Anything else is aligned word by word.
/// A script word the source never voiced on its own is interpolated within
/// its neighbours' gap (confidence 0.3). Over 25% of words deleted or
/// inserted is a mismatch — the audio does not say this script — stored
/// with confidence at most 0.3.
pub fn record(script_text: &str, spoken: &Spoken, duration: f64) -> TakeTiming {
    let script = tokenize(script_text);
    let n = script.words.len();
    let to_script = chars_map(script_text, &spoken.against);
    let mut spoken_words: Vec<SpokenWord> = spoken.words.clone();
    spoken_words.sort_by(|a, b| a.start.total_cmp(&b.start));
    // Fill the ends a source left open: the next word's start, else the
    // take's end.
    let ends: Vec<f64> = (0..spoken_words.len())
        .map(|i| {
            spoken_words[i]
                .end
                .unwrap_or_else(|| spoken_words.get(i + 1).map(|w| w.start).unwrap_or(duration))
        })
        .collect();
    // Each script word's run of spoken words.
    let runs: Vec<Vec<usize>> = match &to_script {
        Some(map) if spoken_words.iter().all(|w| w.chars.is_some()) => {
            let mut runs = vec![Vec::new(); n];
            for (k, w) in spoken_words.iter().enumerate() {
                let Some(at) = w.chars.and_then(|c| map(c[0])) else {
                    continue;
                };
                if let Some(i) = script
                    .words
                    .iter()
                    .position(|sw| sw.chars[0] <= at && at < sw.chars[1])
                {
                    runs[i].push(k);
                }
            }
            runs
        }
        _ => {
            let texts: Vec<String> = script.words.iter().map(|w| w.text.clone()).collect();
            let said: Vec<String> = spoken_words.iter().map(|w| w.text.clone()).collect();
            align_words(&texts, &said)
                .into_iter()
                .map(|r| r.collect())
                .collect()
        }
    };
    let used: usize = runs.iter().map(Vec::len).sum();
    let deleted = runs.iter().filter(|r| r.is_empty()).count();
    let inserted = spoken_words.len().saturating_sub(used);
    let mismatch = n > 0 && (deleted + inserted) * 4 > n;
    let mut words: Vec<Option<(f64, f64)>> = runs
        .iter()
        .map(|run| {
            let first = *run.first()?;
            let last = *run.last()?;
            Some((
                spoken_words[first].start,
                ends[last].max(spoken_words[first].start),
            ))
        })
        .collect();
    // Keep the times monotonic: a word never starts before the one before
    // it ended.
    let mut floor = 0.0f64;
    for slot in words.iter_mut().flatten() {
        slot.0 = slot.0.max(floor);
        slot.1 = slot.1.max(slot.0);
        floor = slot.1;
    }
    let confidence = if mismatch {
        spoken.confidence.min(0.3)
    } else {
        spoken.confidence
    };
    let interpolated = interpolate(&script, &mut words, duration);
    let words: Vec<Word> = script
        .words
        .iter()
        .zip(words)
        .zip(interpolated)
        .map(|((w, times), guessed)| {
            let (start, end) = times.unwrap_or((0.0, 0.0));
            Word {
                text: w.text.clone(),
                start: round_us(start),
                end: round_us(end),
                chars: w.chars,
                confidence: (guessed && confidence > 0.3).then_some(0.3),
                interpolated: guessed,
                prominence: None,
            }
        })
        .collect();
    TakeTiming {
        timing_version: TIMING_VERSION,
        audio: Default::default(),
        script: script_text.to_string(),
        script_hash: promo_model::speech::script_hash(script_text),
        duration: round_us(duration),
        source: spoken.source.clone(),
        estimated: matches!(spoken.source, TimingSource::Estimate),
        confidence,
        sentences: sentences_of(&script, &words),
        words,
        aligner_version: None,
        extra: Default::default(),
    }
}

/// How a spoken word's `chars` (into `against`) become the script's:
/// the same text, or the script with its stress spellings converted. None:
/// the two texts are not those, and the words are matched by spelling.
fn chars_map(script: &str, against: &str) -> Option<Box<dyn Fn(u32) -> Option<u32>>> {
    if against == script {
        return Some(Box::new(Some));
    }
    let (marked, map) = crate::stress::marking_stress_mapped(script);
    (marked == against).then(|| -> Box<dyn Fn(u32) -> Option<u32>> {
        Box::new(move |at: u32| map.get(at as usize).copied())
    })
}

/// Fills the words no source voiced: each run of them shares its
/// neighbours' gap (the previous word's end to the next one's start, the
/// take's ends at the edges) by letter count. Answers which were filled.
fn interpolate(script: &Script, words: &mut [Option<(f64, f64)>], duration: f64) -> Vec<bool> {
    let filled: Vec<bool> = words.iter().map(Option::is_none).collect();
    let mut i = 0;
    while i < words.len() {
        if words[i].is_some() {
            i += 1;
            continue;
        }
        let first = i;
        while i < words.len() && words[i].is_none() {
            i += 1;
        }
        let from = if first == 0 {
            0.0
        } else {
            words[first - 1].map(|w| w.1).unwrap_or(0.0)
        };
        let to = words
            .get(i)
            .copied()
            .flatten()
            .map(|w| w.0)
            .unwrap_or(duration)
            .max(from);
        let weights: Vec<f64> = (first..i)
            .map(|k| script.words[k].text.chars().count().max(1) as f64)
            .collect();
        let total: f64 = weights.iter().sum();
        let mut t = from;
        for (k, w) in (first..i).zip(&weights) {
            let span = (to - from) * w / total;
            words[k] = Some((t, t + span));
            t += span;
        }
    }
    filled
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spoken_words(list: &[(&str, f64, f64)]) -> Vec<SpokenWord> {
        list.iter()
            .map(|&(text, start, end)| SpokenWord {
                text: text.into(),
                start,
                end: Some(end),
                chars: None,
            })
            .collect()
    }

    /// A local voice says "$49.99" as six words: one script word spans
    /// all six, and nothing after it shifts.
    #[test]
    fn attach_merges_spelled_numbers() {
        let script = "It costs $49.99 today.";
        let spoken = Spoken {
            words: spoken_words(&[
                ("it", 0.10, 0.20),
                ("costs", 0.22, 0.60),
                ("forty", 0.70, 0.95),
                ("nine", 0.96, 1.20),
                ("dollars", 1.21, 1.60),
                ("ninety", 1.65, 1.90),
                ("nine", 1.91, 2.10),
                ("cents", 2.11, 2.50),
                ("today", 2.60, 3.00),
            ]),
            source: TimingSource::Local("kokoro-en-q8/af_heart".into()),
            confidence: 0.97,
            against: "It costs forty nine dollars ninety nine cents today.".into(),
        };
        let timing = record(script, &spoken, 3.4);
        assert_eq!(timing.words.len(), 4);
        assert_eq!(timing.words[2].text, "$49.99");
        assert_eq!((timing.words[2].start, timing.words[2].end), (0.7, 2.5));
        assert_eq!((timing.words[3].start, timing.words[3].end), (2.6, 3.0));
        assert!(timing.words.iter().all(|w| !w.interpolated));
        assert_eq!(timing.confidence, 0.97);
        assert_eq!(timing.sentences.len(), 1);
        assert_eq!(timing.sentences[0].end, 3.0);
    }

    /// A word the source never said is shared its neighbours' gap and
    /// flagged; a source saying something else entirely is a mismatch.
    #[test]
    fn unvoiced_words_interpolate_and_a_different_text_is_a_mismatch() {
        let spoken = Spoken {
            words: spoken_words(&[("meet", 0.1, 0.3), ("geometry", 1.0, 1.5)]),
            source: TimingSource::Aligned,
            confidence: 0.8,
            against: String::new(),
        };
        // One word of three deleted: 33% — a mismatch, confidence capped
        // (and the guessed word's own is the take's, so not written).
        let timing = record("Meet Euclid, geometry.", &spoken, 2.0);
        let euclid = &timing.words[1];
        assert!(euclid.interpolated);
        assert_eq!((euclid.start, euclid.end), (0.3, 1.0));
        assert_eq!(timing.confidence, 0.3);
        assert_eq!(euclid.confidence, None);
        // One of five: the take keeps its confidence, the guess says 0.3.
        let mostly = Spoken {
            words: spoken_words(&[
                ("meet", 0.1, 0.3),
                ("geometry", 1.0, 1.5),
                ("proved", 1.6, 1.9),
                ("here", 2.0, 2.2),
            ]),
            ..spoken.clone()
        };
        let timing = record("Meet Euclid, geometry proved here.", &mostly, 2.5);
        assert_eq!(timing.confidence, 0.8);
        assert!(timing.words[1].interpolated);
        assert_eq!(timing.words[1].confidence, Some(0.3));
        assert!(!timing.words[2].interpolated);

        let close = Spoken {
            words: spoken_words(&[
                ("meet", 0.1, 0.3),
                ("euklid", 0.4, 0.8),
                ("geometry", 1.0, 1.5),
                ("proved", 1.6, 2.0),
            ]),
            ..spoken
        };
        let timing = record("Meet Euclid, geometry proved.", &close, 2.2);
        assert!(timing.words.iter().all(|w| !w.interpolated));
        assert_eq!(timing.words[1].start, 0.4, "a near spelling still matches");
        assert_eq!(timing.confidence, 0.8);
    }

    fn fixture(name: &str) -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../fixtures/speech")
            .join(name);
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn fixture_mp3(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../fixtures/speech")
            .join(name);
        std::fs::read(path).unwrap()
    }

    const SCRIPT: &str = "Meet Euclid. Geometry, proved step by step.";

    /// ElevenLabs' characters become the script's words: each word from its
    /// first character's start to its last letter's end, the audio the
    /// committed MP3's bytes.
    #[test]
    fn elevenlabs_alignment_maps_to_script_chars() {
        let take = elevenlabs_take(SCRIPT, &fixture("elevenlabs-with-timestamps.json")).unwrap();
        assert_eq!(take.audio, fixture_mp3("elevenlabs-with-timestamps.mp3"));
        let spoken = take.spoken.expect("timed");
        assert_eq!(spoken.confidence, 1.0);
        assert_eq!(spoken.source.as_text(), "provider:elevenlabs");
        let starts: Vec<f64> = spoken.words.iter().map(|w| w.start).collect();
        assert_eq!(starts, [0.12, 0.42, 1.4, 2.16, 2.56, 2.84, 3.02]);
        assert_eq!(spoken.words[1].end, Some(0.9));
        assert_eq!(spoken.words[1].chars, Some([5, 11]));
        let timing = record(SCRIPT, &spoken, 3.7);
        assert_eq!(timing.words.len(), 7);
        assert_eq!(timing.sentences[1].start, 1.4);
        assert_eq!(timing.sentences[1].end, 3.4);
        assert!(!timing.estimated);

        // Stress-marked Russian: ElevenLabs was sent "стои́т" (one more
        // character than the script's "стоИт") and handed "и́" back as one
        // character; the word still spans the script's own five.
        let script = "Это стоИт денег.";
        let take = elevenlabs_take(script, &fixture("elevenlabs-stress-ru.json")).unwrap();
        let spoken = take.spoken.expect("timed");
        assert_eq!(spoken.confidence, 1.0, "the echo is the sent text");
        assert_eq!(spoken.words.len(), 3);
        assert_eq!(spoken.words[1].text, "стоИт");
        assert_eq!(spoken.words[1].chars, Some([4, 9]));
        assert_eq!(
            (spoken.words[1].start, spoken.words[1].end),
            (0.46, Some(0.95))
        );
        assert_eq!(spoken.words[2].start, 1.02);

        // The apps hand the synthesizer the script with its stress already
        // converted; the record still lands on the script's characters.
        let marked = crate::stress::marking_stress(script);
        let take = elevenlabs_take(&marked, &fixture("elevenlabs-stress-ru.json")).unwrap();
        let timing = record(script, &take.spoken.unwrap(), 1.8);
        assert_eq!(timing.words[1].text, "стоИт");
        assert_eq!(timing.words[1].chars, [4, 9]);
        assert_eq!(timing.words[1].start, 0.46);
        assert!(timing.words.iter().all(|w| !w.interpolated));
    }

    /// ElevenLabs dropping or merging characters does not lose the words:
    /// the edit-distance alignment maps what came back, at confidence 0.9.
    #[test]
    fn elevenlabs_char_mismatch_falls_back_to_dp() {
        let mut answer = fixture("elevenlabs-with-timestamps.json");
        let alignment = &mut answer["alignment"];
        // Drop the comma after "Geometry" and the final period.
        for key in [
            "characters",
            "character_start_times_seconds",
            "character_end_times_seconds",
        ] {
            let list = alignment[key].as_array_mut().unwrap();
            list.remove(21);
            list.pop();
        }
        let take = elevenlabs_take(SCRIPT, &answer).unwrap();
        let spoken = take.spoken.expect("still timed");
        assert_eq!(spoken.confidence, 0.9);
        let starts: Vec<f64> = spoken.words.iter().map(|w| w.start).collect();
        assert_eq!(starts, [0.12, 0.42, 1.4, 2.16, 2.56, 2.84, 3.02]);
        assert_eq!(spoken.words[2].end, Some(1.96));
        assert_eq!(spoken.words[6].end, Some(3.4));
    }

    /// One mark before each word, the words escaped, the whitespace and
    /// the stress conversion as the plain request would send them.
    #[test]
    fn google_ssml_has_one_mark_per_word_and_escapes() {
        let (ssml, marks) = google_ssml_marked("Tom & Jerry's \"test\" <now>, — done.\nНу, стоИт.");
        assert_eq!(marks, [0, 1, 2, 3, 4, 5, 6]);
        assert_eq!(
            ssml,
            "<speak><mark name=\"w0\"/>Tom &amp; <mark name=\"w1\"/>Jerry&apos;s \
             &quot;<mark name=\"w2\"/>test&quot; <mark name=\"w3\"/>&lt;now&gt;, — \
             <mark name=\"w4\"/>done.\n<mark name=\"w5\"/>Ну, <mark name=\"w6\"/>стои\u{0301}т.\
             </speak>"
        );
    }

    /// An SSML script is marked in its text only: a word inside say-as,
    /// sub or phoneme gets one mark before the element, the element's
    /// other words share it.
    #[test]
    fn google_ssml_script_marks_text_nodes_only() {
        let script = r#"<speak>It costs <say-as interpret-as="currency">$49 USD</say-as>, <emphasis>really</emphasis>.</speak>"#;
        let (ssml, marks) = google_ssml_marked(script);
        assert_eq!(marks, [0, 1, 2, 4]);
        assert_eq!(
            ssml,
            r#"<speak><mark name="w0"/>It <mark name="w1"/>costs <mark name="w2"/><say-as interpret-as="currency">$49 USD</say-as>, <emphasis><mark name="w4"/>really</emphasis>.</speak>"#
        );
        // The shared word is interpolated in the record, the rest timed.
        let answer = serde_json::json!({
            "audioContent": "SUQz",
            "timepoints": [{"markName": "w0", "timeSeconds": 0.1},
                           {"markName": "w1", "timeSeconds": 0.3},
                           {"markName": "w2", "timeSeconds": 0.7},
                           {"markName": "w4", "timeSeconds": 1.6}]
        });
        let take = google_take(script, &answer, &marks).unwrap();
        let timing = record(script, &take.spoken.unwrap(), 2.0);
        let words: Vec<(&str, bool)> = timing
            .words
            .iter()
            .map(|w| (w.text.as_str(), w.interpolated))
            .collect();
        assert_eq!(
            words,
            [
                ("It", false),
                ("costs", false),
                ("$49", false),
                ("USD", true),
                ("really", false)
            ]
        );
        assert_eq!(timing.words[4].end, 2.0, "the last word ends with the take");
    }

    /// Google's marks become word starts; with fewer timepoints than marks
    /// the audio is kept and the take is left to be aligned, said so.
    #[test]
    fn google_missing_timepoints_falls_back_to_alignment() {
        let (_, marks) = google_ssml_marked(SCRIPT);
        let full = fixture("google-timepoints.json");
        let take = google_take(SCRIPT, &full, &marks).unwrap();
        assert_eq!(take.audio, fixture_mp3("google-timepoints.mp3"));
        let spoken = take.spoken.expect("timed");
        assert_eq!(spoken.confidence, 0.95);
        assert_eq!(spoken.words[0].end, Some(0.42), "until the next mark");
        let timing = record(SCRIPT, &spoken, 3.7);
        assert_eq!(timing.words[6].end, 3.7);
        assert_eq!(timing.source.as_text(), "provider:google");

        let mut short = full.clone();
        short["timepoints"].as_array_mut().unwrap().truncate(5);
        let take = google_take(SCRIPT, &short, &marks).unwrap();
        assert!(take.spoken.is_none());
        assert_eq!(take.audio, fixture_mp3("google-timepoints.mp3"));
        assert!(take.note.unwrap().contains("5 of 7 word marks"));
    }

    /// Marks by voice family: the classic classes take them; Chirp,
    /// Journey, unknown classes and bare Gemini names take the plain request.
    #[test]
    fn chirp_voices_take_the_plain_request() {
        for voice in [
            "en-US-Neural2-F",
            "en-GB-Wavenet-B",
            "ru-RU-Standard-A",
            "en-US-News-K",
            "en-US-Polyglot-1",
            "en-US-Studio-O",
            "cmn-CN-Standard-A",
            "en-US-Neural2-F@en-US",
        ] {
            assert!(google_supports_marks(voice), "{voice}");
        }
        for voice in [
            "en-US-Chirp3-HD-Kore",
            "en-US-Chirp-HD-F",
            "en-US-Journey-D",
            "en-US-Casual-K",
            "Achernar",
            "Kore@de-DE",
        ] {
            assert!(!google_supports_marks(voice), "{voice}");
        }
        assert!(google_refused_marks(
            "https://texttospeech.googleapis.com/v1beta1/text:synthesize answered 400: \
             {\"error\": {\"message\": \"This voice does not support SSML input.\"}}"
        ));
        assert!(!google_refused_marks("… answered 403: API key not valid"));
    }

    /// A near spelling is a cheaper match than an unrelated word, so a
    /// filler the source inserted after it takes the insertion.
    #[test]
    fn a_near_spelling_wins_over_a_filler() {
        let spoken = Spoken {
            words: spoken_words(&[
                ("meet", 0.1, 0.3),
                ("euklid", 0.4, 0.8),
                ("the", 0.85, 0.95),
                ("geometry", 1.0, 1.5),
            ]),
            source: TimingSource::Aligned,
            confidence: 0.8,
            against: String::new(),
        };
        let timing = record("Meet Euclid geometry.", &spoken, 1.8);
        assert_eq!(timing.words[1].start, 0.4);
        assert_eq!(timing.words[2].start, 1.0);
    }

    /// Characters into the stress-converted text (what the apps send) map
    /// straight onto the script's words — so a repeated word the source
    /// skipped is the one interpolated, where matching by spelling alone
    /// would hand its time to the wrong repeat.
    #[test]
    fn stress_marked_chars_map_onto_the_script() {
        let script = "стоИт стоИт стоИт.";
        let sent = crate::stress::marking_stress(script);
        let words = tokenize(&sent).words;
        let spoken = Spoken {
            words: [0usize, 2]
                .iter()
                .map(|&k| SpokenWord {
                    text: words[k].text.clone(),
                    start: 0.2 + k as f64 * 0.5,
                    end: Some(0.5 + k as f64 * 0.5),
                    chars: Some(words[k].chars),
                })
                .collect(),
            source: TimingSource::Provider("elevenlabs".into()),
            confidence: 1.0,
            against: sent,
        };
        let timing = record(script, &spoken, 2.0);
        assert!(timing.words[1].interpolated);
        assert!(!timing.words[2].interpolated);
        assert_eq!(timing.words[2].start, 1.2);
        assert_eq!(timing.words[2].chars, [12, 17]);
    }

    /// The character aligner maps what came back onto what was sent, a
    /// dropped and an inserted character included.
    #[test]
    fn character_alignment_survives_drops_and_inserts() {
        let sent: Vec<char> = "Hello, world".chars().collect();
        let answered: Vec<char> = "Hello world!".chars().collect();
        let map = align_chars(&answered, &sent);
        assert_eq!(map[0], Some(0));
        assert_eq!(map[5], Some(6), "the space after the dropped comma");
        assert_eq!(map[10], Some(11));
        assert_eq!(map[11], None, "an added character maps nowhere");
    }
}

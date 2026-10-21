//! How long words take to say, without saying them — the estimate behind
//! a silent placeholder's length and its word times, and behind the layout
//! of a take nothing has timed yet (docs/SPEECH-SYNC.md §5, §6.4). Here,
//! beside the word-timing record, so the timing pass reads it without the
//! provider client; `promo_speech` re-exports it, with the calibration
//! that reads a project's receipts.

/// Seconds of silence providers wrap around speech, both ends together:
/// a take of N syllables runs N / rate plus this.
const PADDING: f64 = 0.4;

/// English narration rate, syllables per second, as a synthesized voice
/// reads a promo line (~155 words a minute). The project's own takes
/// replace it per voice ([`calibration`]).
const ENGLISH_RATE: f64 = 4.2;

/// What a script would take to say.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Estimate {
    /// The take's length, padding included.
    pub seconds: f64,
    /// Syllables (moras for Japanese) counted.
    pub syllables: f64,
    /// Pauses punctuation adds, seconds.
    pub pauses: f64,
    /// The multiplier this project's own takes in this voice applied (1
    /// when none exist yet).
    pub calibration: f64,
}

/// The writing system a character belongs to, as far as counting goes.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Script {
    Latin,
    Cyrillic,
    Greek,
    Hangul,
    Han,
    Kana,
    Indic,
    Thai,
    Abjad,
    Other,
}

fn script_of(c: char) -> Option<Script> {
    let u = c as u32;
    Some(match u {
        _ if c.is_ascii_alphabetic() => Script::Latin,
        0x00C0..=0x024F | 0x1E00..=0x1EFF => Script::Latin,
        0x0370..=0x03FF | 0x1F00..=0x1FFF => Script::Greek,
        0x0400..=0x052F => Script::Cyrillic,
        0xAC00..=0xD7A3 | 0x1100..=0x11FF | 0x3130..=0x318F => Script::Hangul,
        0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F => Script::Han,
        0x3040..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F => Script::Kana,
        0x0900..=0x0DFF => Script::Indic,
        0x0E00..=0x0E7F => Script::Thai,
        0x0590..=0x05FF | 0x0600..=0x06FF | 0x0750..=0x077F | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF => {
            Script::Abjad
        }
        _ if c.is_alphabetic() => Script::Other,
        _ => return None,
    })
}

/// The script most of the text's letters are in.
fn dominant_script(text: &str) -> Script {
    let mut counts: Vec<(Script, usize)> = Vec::new();
    for script in text.chars().filter_map(script_of) {
        match counts.iter_mut().find(|(s, _)| *s == script) {
            Some((_, n)) => *n += 1,
            None => counts.push((script, 1)),
        }
    }
    // Japanese mixes kanji and kana: any kana makes Han read as Japanese.
    let kana = counts.iter().any(|(s, _)| *s == Script::Kana);
    let best = counts
        .iter()
        .max_by_key(|(_, n)| *n)
        .map(|(s, _)| *s)
        .unwrap_or(Script::Latin);
    if best == Script::Han && kana {
        Script::Kana
    } else {
        best
    }
}

/// The language's base code ("en" from "en-US-Neural2-F"), when the voice
/// or the caller says.
fn base_language(language: Option<&str>) -> Option<String> {
    let code = language?.split(['-', '_']).next()?.to_ascii_lowercase();
    (code.len() == 2 || code.len() == 3).then_some(code)
}

/// Syllables (moras) per second for synthesized narration, per language —
/// scaled from English by the syllable rates read speech runs at across
/// languages (Pellegrino, Coupé & Marsico 2011: languages that pack less
/// into a syllable say more of them a second).
fn default_rate(language: Option<&str>, script: Script) -> f64 {
    let ratio = match base_language(language).as_deref() {
        Some("en") => 1.0,
        Some("es") => 7.82 / 6.19,
        Some("fr") => 7.18 / 6.19,
        Some("it") => 6.99 / 6.19,
        Some("de") => 5.97 / 6.19,
        Some("ja") => 7.84 / 6.19,
        Some("zh") | Some("cmn") | Some("yue") => 5.18 / 6.19,
        Some("vi") => 5.22 / 6.19,
        Some("pt") => 1.15,
        Some("ru") | Some("uk") | Some("pl") | Some("cs") => 1.08,
        Some("ko") => 1.12,
        _ => match script {
            Script::Latin => 1.0,
            Script::Han => 5.18 / 6.19,
            Script::Kana => 7.84 / 6.19,
            Script::Hangul => 1.12,
            Script::Cyrillic | Script::Greek => 1.08,
            _ => 1.05,
        },
    };
    ENGLISH_RATE * ratio
}

fn is_vowel(c: char) -> bool {
    matches!(
        c.to_lowercase().next().unwrap_or(c),
        'a' | 'e' | 'i' | 'o' | 'u' | 'y'
            | 'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'æ' | 'è' | 'é' | 'ê' | 'ë' | 'ì' | 'í'
            | 'î' | 'ï' | 'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ù' | 'ú' | 'û' | 'ü' | 'ý'
            | 'ÿ' | 'ą' | 'ę' | 'ő' | 'ű' | 'ă' | 'ơ' | 'ư' | 'œ'
            // Cyrillic
            | 'а' | 'е' | 'ё' | 'и' | 'о' | 'у' | 'ы' | 'э' | 'ю' | 'я' | 'і' | 'ї' | 'є'
            // Greek
            | 'α' | 'ε' | 'η' | 'ι' | 'ο' | 'υ' | 'ω' | 'ά' | 'έ' | 'ή' | 'ί' | 'ό' | 'ύ'
            | 'ώ' | 'ϊ' | 'ϋ' | 'ΐ' | 'ΰ'
    )
}

/// Vowel groups in one word, with English's silent endings taken off when
/// the language is English (or unsaid, in Latin script).
fn word_syllables(word: &str, english: bool) -> f64 {
    let lower: Vec<char> = word.to_lowercase().chars().collect();
    if lower.is_empty() {
        return 0.0;
    }
    let mut groups = 0usize;
    let mut previous = false;
    for &c in &lower {
        let vowel = is_vowel(c);
        if vowel && !previous {
            groups += 1;
        }
        previous = vowel;
    }
    if english && groups > 1 {
        let s: String = lower.iter().collect();
        // "make", "time" — a silent final e (but "table", "little" keep it).
        if s.ends_with('e') && !s.ends_with("le") && !s.ends_with("ee") {
            groups -= 1;
        }
        // "moved", "jumped" — -ed after anything but t/d is no syllable.
        if s.ends_with("ed") && !s.ends_with("ted") && !s.ends_with("ded") {
            groups -= 1;
        }
        // "makes", "times" — the same e before an s.
        if s.ends_with("es")
            && !["ses", "zes", "ches", "shes", "ges", "ces", "xes"]
                .iter()
                .any(|ending| s.ends_with(ending))
        {
            groups -= 1;
        }
    }
    groups.max(1) as f64
}

/// Moras in kana: every kana is one, small ya/yu/yo/vowels join the one
/// before (きょ is one mora), the small tsu and the long mark count.
fn kana_moras(c: char) -> f64 {
    match c {
        'ゃ' | 'ゅ' | 'ょ' | 'ぁ' | 'ぃ' | 'ぅ' | 'ぇ' | 'ぉ' | 'ャ' | 'ュ' | 'ョ' | 'ァ'
        | 'ィ' | 'ゥ' | 'ェ' | 'ォ' => 0.0,
        _ => 1.0,
    }
}

/// English words for a whole number, for counting their syllables.
fn number_words(n: u64) -> String {
    const ONES: [&str; 20] = [
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
    ];
    const TENS: [&str; 10] = [
        "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
    ];
    fn below_thousand(n: u64) -> String {
        let mut out = Vec::new();
        if n >= 100 {
            out.push(format!("{} hundred", ONES[(n / 100) as usize]));
        }
        let rest = n % 100;
        if rest >= 20 {
            out.push(TENS[(rest / 10) as usize].to_string());
            if !rest.is_multiple_of(10) {
                out.push(ONES[(rest % 10) as usize].to_string());
            }
        } else if rest > 0 || n == 0 {
            out.push(ONES[rest as usize].to_string());
        }
        out.join(" ")
    }
    if n < 1000 {
        return below_thousand(n);
    }
    let mut parts = Vec::new();
    for (scale, name) in [
        (1_000_000_000_000u64, "trillion"),
        (1_000_000_000, "billion"),
        (1_000_000, "million"),
        (1_000, "thousand"),
    ] {
        if n >= scale && !(n / scale).is_multiple_of(1000) {
            parts.push(format!("{} {name}", below_thousand((n / scale) % 1000)));
        }
    }
    if !n.is_multiple_of(1000) {
        parts.push(below_thousand(n % 1000));
    }
    parts.join(" ")
}

/// Syllables a run of digits takes to say: spelled out in English (a year
/// like 2026 as "twenty twenty-six"), roughly per digit elsewhere.
fn number_syllables(digits: &str, english: bool, grouped: bool) -> f64 {
    let n: u64 = match digits.parse() {
        Ok(n) => n,
        Err(_) => return digits.len() as f64 * 1.2,
    };
    if !english {
        // Other languages' number words run a little longer than English.
        return number_syllables(digits, true, grouped) * 1.15;
    }
    // 2026 alone is a year ("twenty twenty-six"); 1,250 is a quantity.
    let year = !grouped && digits.len() == 4 && (1100..2100).contains(&n) && !n.is_multiple_of(100);
    let words = if year {
        format!("{} {}", number_words(n / 100), number_words(n % 100))
    } else {
        number_words(n)
    };
    words.split_whitespace().map(number_word_syllables).sum()
}

/// Exact syllables of the words [`number_words`] writes — the general rule
/// would take "hundred" for a silent -ed.
fn number_word_syllables(word: &str) -> f64 {
    match word {
        "eleven" | "seventeen" | "seventy" => 3.0,
        "zero" | "seven" | "thirteen" | "fourteen" | "fifteen" | "sixteen" | "eighteen"
        | "nineteen" | "twenty" | "thirty" | "forty" | "fifty" | "sixty" | "eighty" | "ninety"
        | "hundred" | "thousand" | "million" | "billion" | "trillion" => 2.0,
        _ => 1.0,
    }
}

/// Pause, in seconds, a punctuation mark asks for in the middle of a script.
fn pause_for(c: char) -> f64 {
    match c {
        ',' | ';' | ':' | '、' | '，' | '；' | '：' => 0.25,
        '.' | '!' | '?' | '。' | '！' | '？' | '…' => 0.45,
        '—' | '–' => 0.3,
        '\n' => 0.35,
        _ => 0.0,
    }
}

/// Whether `text` in `language` counts as English, and the script it is
/// mostly in — decided once for a whole script.
fn reading(text: &str, language: Option<&str>) -> (bool, Script) {
    let script = dominant_script(text);
    let english = match base_language(language).as_deref() {
        Some(code) => code == "en",
        None => script == Script::Latin,
    };
    (english, script)
}

/// Syllables and pauses in `text`, read as `language` (or by its script).
fn count(text: &str, language: Option<&str>) -> (f64, f64) {
    let (english, script) = reading(text, language);
    count_as(text, english, script)
}

/// [`count`] with the reading decided — so a word counts the way the
/// whole script around it does.
fn count_as(text: &str, english: bool, script: Script) -> (f64, f64) {
    let mut syllables = 0.0;
    let mut word = String::new();
    let mut digits = String::new();
    let chars: Vec<char> = text.trim().chars().collect();
    let flush_word = |word: &mut String, syllables: &mut f64| {
        if !word.is_empty() {
            *syllables += word_syllables(word, english);
            word.clear();
        }
    };
    let mut grouped = false;
    let flush_digits = |digits: &mut String, syllables: &mut f64, grouped: &mut bool| {
        if !digits.is_empty() {
            *syllables += number_syllables(digits, english, *grouped);
            digits.clear();
        }
        *grouped = false;
    };
    let mut pauses = 0.0;
    let mut indic_previous_consonant = false;
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_digit() {
            flush_word(&mut word, &mut syllables);
            digits.push(c);
            continue;
        }
        // "1,250" and "3.5" stay one number.
        if (c == ',' || c == '.')
            && !digits.is_empty()
            && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit())
        {
            if c == '.' {
                flush_digits(&mut digits, &mut syllables, &mut grouped);
                syllables += 1.0; // "point"
            } else {
                grouped = true;
            }
            continue;
        }
        flush_digits(&mut digits, &mut syllables, &mut grouped);
        match script_of(c) {
            Some(Script::Latin | Script::Cyrillic | Script::Greek) => word.push(c),
            Some(Script::Hangul) => {
                flush_word(&mut word, &mut syllables);
                if (0xAC00..=0xD7A3).contains(&(c as u32)) {
                    syllables += 1.0;
                }
            }
            Some(Script::Han) => {
                flush_word(&mut word, &mut syllables);
                // A kanji reads as ~1.7 moras in Japanese, one syllable in Chinese.
                syllables += if script == Script::Kana { 1.7 } else { 1.0 };
            }
            Some(Script::Kana) => {
                flush_word(&mut word, &mut syllables);
                syllables += kana_moras(c);
            }
            Some(Script::Indic) => {
                flush_word(&mut word, &mut syllables);
                let u = c as u32 & 0x7F; // position within the script's block
                let virama = u == 0x4D;
                let independent_vowel = (0x05..=0x14).contains(&u);
                let consonant = (0x15..=0x39).contains(&u) || (0x58..=0x5F).contains(&u);
                if virama && indic_previous_consonant {
                    syllables -= 1.0; // a conjunct: the consonant joins the next
                }
                if independent_vowel || consonant {
                    syllables += 1.0;
                }
                indic_previous_consonant = consonant;
            }
            Some(Script::Thai) => {
                flush_word(&mut word, &mut syllables);
                syllables += 0.45;
            }
            Some(Script::Abjad) => {
                flush_word(&mut word, &mut syllables);
                syllables += 0.55;
            }
            Some(Script::Other) => {
                flush_word(&mut word, &mut syllables);
                syllables += 0.4;
            }
            None => {
                flush_word(&mut word, &mut syllables);
                // A pause only between things said — none at the very end.
                if chars[i + 1..]
                    .iter()
                    .any(|n| script_of(*n).is_some() || n.is_ascii_digit())
                {
                    pauses += pause_for(c);
                }
                if c == '%' {
                    syllables += 2.0; // "percent"
                }
            }
        }
    }
    flush_word(&mut word, &mut syllables);
    flush_digits(&mut digits, &mut syllables, &mut grouped);
    (syllables.max(0.0), pauses)
}

/// The estimate before calibration: syllables / the language's rate,
/// plus pauses and padding — (seconds, syllables, pauses).
pub fn raw_seconds(text: &str, language: Option<&str>) -> (f64, f64, f64) {
    let (syllables, pauses) = count(text, language);
    let rate = default_rate(language, dominant_script(text));
    let seconds = if syllables > 0.0 {
        syllables / rate + pauses + PADDING
    } else {
        0.0
    };
    (seconds, syllables, pauses)
}

/// How long `text` takes to say in a voice of `language` (a code like
/// "en-US", or None to read it by its script), scaled by `calibration` —
/// the ratio this project's own takes in that voice ran at against the
/// estimate ([`calibration`]).
pub fn estimate(text: &str, language: Option<&str>, calibration: f64) -> Estimate {
    let (seconds, syllables, pauses) = raw_seconds(text, language);
    let calibration = if calibration.is_finite() && calibration > 0.0 {
        calibration
    } else {
        1.0
    };
    Estimate {
        seconds: seconds * calibration,
        syllables,
        pauses,
        calibration,
    }
}

/// How long one word of a script takes, in the estimate's own terms.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WordWeight {
    /// Syllables (moras) the word takes to say — numbers spelled out.
    pub syllables: f64,
    /// Pauses inside the word ("e.g", "3.5" have none; a word with an
    /// inner dash may), seconds.
    pub pause_within: f64,
    /// The pause punctuation asks for after the word, before the next one,
    /// seconds; 0 after the last word.
    pub pause_after: f64,
}

/// The estimate per word: the same syllable counts and punctuation pauses
/// the length estimate uses ([`estimate`]), made per word of the one
/// tokenizer (`crate::speech::tokenize`). Their sum is the whole
/// script's count.
pub fn word_weights(
    script: &crate::speech::Script,
    text: &str,
    language: Option<&str>,
) -> Vec<WordWeight> {
    let (english, dominant) = reading(text, language);
    let chars: Vec<char> = text.chars().collect();
    script
        .words
        .iter()
        .enumerate()
        .map(|(i, word)| {
            let (syllables, pause_within) = count_as(&word.text, english, dominant);
            let pause_after = match script.words.get(i + 1) {
                None => 0.0,
                Some(next) => {
                    let mut in_tag = false;
                    let mut pause = 0.0;
                    for &c in &chars[word.chars[1] as usize..next.chars[0] as usize] {
                        match c {
                            '<' if script.ssml => in_tag = true,
                            '>' if in_tag => in_tag = false,
                            _ if in_tag => {}
                            _ if script_of(c).is_none() && !c.is_ascii_digit() => {
                                pause += pause_for(c)
                            }
                            _ => {}
                        }
                    }
                    pause
                }
            };
            WordWeight {
                syllables,
                pause_within,
                pause_after,
            }
        })
        .collect()
}

/// Word times for a take nothing has timed (docs/SPEECH-SYNC.md §5): a
/// placeholder, or a take before its timing exists. `PADDING / 2` of
/// lead-in and as much at the end; the rest shared between the words in
/// proportion to their syllables, the punctuation pauses kept and scaled
/// by the same factor — so the times fill `duration` exactly. A duration
/// that is not a positive number is the estimate's own length (scaled by
/// `calibration`). Source `estimate`, flagged `estimated`, confidence 0.3;
/// the `audio` source is filled in when the record is stored.
pub fn estimate_words(
    text: &str,
    duration: f64,
    language: Option<&str>,
    calibration: f64,
) -> crate::speech::TakeTiming {
    use crate::speech::{round_us, Word};
    let script = crate::speech::tokenize(text);
    let weights = word_weights(&script, text, language);
    let rate = default_rate(language, dominant_script(text));
    let duration = if duration.is_finite() && duration > 0.0 {
        duration
    } else {
        estimate(text, language, calibration).seconds
    };
    let lead = (PADDING / 2.0).min(duration / 4.0);
    let spans: Vec<f64> = weights
        .iter()
        .map(|w| w.syllables.max(0.5) / rate + w.pause_within)
        .collect();
    let raw: f64 = spans.iter().sum::<f64>() + weights.iter().map(|w| w.pause_after).sum::<f64>();
    let scale = if raw > 0.0 {
        (duration - 2.0 * lead).max(0.0) / raw
    } else {
        0.0
    };
    let mut t = lead;
    let words: Vec<Word> = script
        .words
        .iter()
        .zip(spans.iter().zip(&weights))
        .map(|(word, (span, weight))| {
            let start = t;
            t += span * scale;
            let end = t;
            t += weight.pause_after * scale;
            Word {
                text: word.text.clone(),
                start: round_us(start),
                end: round_us(end),
                chars: word.chars,
                confidence: None,
                interpolated: false,
                prominence: None,
            }
        })
        .collect();
    crate::speech::TakeTiming {
        timing_version: crate::speech::TIMING_VERSION,
        audio: Default::default(),
        script: text.to_string(),
        script_hash: crate::speech::script_hash(text),
        duration: round_us(duration),
        source: crate::speech::TimingSource::Estimate,
        estimated: true,
        confidence: 0.3,
        sentences: crate::speech::sentences_of(&script, &words),
        words,
        aligner_version: Some(crate::speech::ESTIMATE_VERSION),
        extra: Default::default(),
    }
}

/// The language a voice speaks, from its name when the name says it
/// (Google's "en-US-Neural2-F"); None for voices that speak whatever
/// they are given (OpenAI's "alloy").
pub fn voice_language(voice: &str) -> Option<String> {
    let mut parts = voice.split('-');
    let lang = parts.next()?;
    let region = parts.next()?;
    (lang.len() == 2 && lang.chars().all(|c| c.is_ascii_lowercase()) && region.len() == 2)
        .then(|| format!("{lang}-{region}"))
}

/// A silent MP3 of at least `seconds`: MPEG-1 Layer III, 32 kHz mono at
/// 32 kbit/s, every frame the same 144 bytes whose side information says
/// "nothing coded" — what every decoder plays as silence. ~4 KB a second,
/// and the same bytes on every platform for the same length.
pub fn silent_mp3(seconds: f64) -> Vec<u8> {
    // 1152 samples a frame at 32 kHz = 36 ms.
    const FRAME_SECONDS: f64 = 1152.0 / 32_000.0;
    const FRAME_BYTES: usize = 144; // 144 × 32 kbit/s ÷ 32 kHz, no padding
    let frames = (seconds.max(0.0) / FRAME_SECONDS).ceil().max(1.0) as usize;
    let mut frame = [0u8; FRAME_BYTES];
    // Sync, MPEG-1, Layer III, no CRC | 32 kbit/s, 32 kHz, no padding |
    // mono, original. Side info and main data stay zero: no granule codes
    // anything.
    frame[..4].copy_from_slice(&[0xFF, 0xFB, 0x18, 0xC4]);
    frame.repeat(frames)
}

/// The length [`silent_mp3`] gives `seconds` — whole frames, so a hair
/// longer.
pub fn silent_mp3_seconds(seconds: f64) -> f64 {
    let frames = (seconds.max(0.0) / (1152.0 / 32_000.0)).ceil().max(1.0);
    frames * 1152.0 / 32_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn syl(text: &str) -> f64 {
        count(text, Some("en")).0
    }

    /// English words count the way they are said, silent endings off.
    #[test]
    fn english_syllables() {
        assert_eq!(syl("make"), 1.0);
        assert_eq!(syl("table"), 2.0);
        assert_eq!(syl("moved"), 1.0);
        assert_eq!(syl("wanted"), 2.0);
        assert_eq!(syl("narration"), 3.0);
        assert_eq!(syl("Some footage tells the story on its own."), 10.0);
    }

    /// Numbers are said, not read as digits: 2026 is a year, 1,250 one
    /// number, 3.5 has a "point".
    #[test]
    fn numbers_are_spelled() {
        assert_eq!(syl("2026"), 5.0); // "twenty twenty-six"
        assert_eq!(syl("1,250"), 8.0); // one thou-sand two hun-dred fif-ty — not "twelve fifty"
        assert_eq!(syl("3.5"), 3.0); // three point five
        assert_eq!(syl("50%"), syl("fifty percent"));
    }

    /// Each writing system counts its own way.
    #[test]
    fn scripts_count_their_own_units() {
        assert_eq!(count("안녕하세요", None).0, 5.0); // five Hangul blocks
        assert_eq!(count("你好世界", None).0, 4.0); // four characters
        assert_eq!(count("きょうは", None).0, 3.0); // kyo-u-wa: the small ょ joins
        assert_eq!(count("привет", None).0, 2.0); // при-вет
        assert_eq!(count("hola mundo", Some("es")).0, 4.0);
    }

    /// Pauses come from punctuation between things said, never after the
    /// last word.
    #[test]
    fn pauses_follow_punctuation() {
        assert_eq!(count("Hello.", Some("en")).1, 0.0);
        assert!((count("Hello, world. Again!", Some("en")).1 - 0.70).abs() < 1e-9);
    }

    /// The five spoken takes this repo holds (OpenAI alloy): the default
    /// rate lands within ~20% on three, the two long-padded ones show why
    /// a project calibrates.
    #[test]
    fn the_default_rate_lands_near_real_takes() {
        for (text, actual) in [
            ("Some footage tells the story on its own.", 2.71),
            ("The camera never moved — the viewport did.", 3.12),
            ("Narration is written here, not recorded.", 3.26),
        ] {
            let e = estimate(text, None, 1.0).seconds;
            assert!(
                (e - actual).abs() / actual < 0.2,
                "{text}: {e:.2} vs {actual}"
            );
        }
    }

    /// A voice's name says its language when it can.
    #[test]
    fn voice_names_say_their_language() {
        assert_eq!(voice_language("en-US-Neural2-F").as_deref(), Some("en-US"));
        assert_eq!(voice_language("ja-JP-Wavenet-B").as_deref(), Some("ja-JP"));
        assert_eq!(voice_language("alloy"), None);
    }

    /// The words' weights add up to the whole script's count, so an
    /// estimate's word times and its length never disagree.
    #[test]
    fn estimate_words_sum_matches_estimate() {
        for text in [
            "Meet Euclid. Geometry, proved step by step.",
            "Built for 1,250 worksheets — e.g. 3.5 times faster, 50% cheaper!",
            "The camera never moved — the viewport did.\n\nThen: a new line.",
            "Приложение стои'т три доллара.",
        ] {
            let script = crate::speech::tokenize(text);
            let weights = word_weights(&script, text, None);
            let (syllables, pauses) = count(text, None);
            let sum: f64 = weights.iter().map(|w| w.syllables).sum();
            let paused: f64 = weights.iter().map(|w| w.pause_after + w.pause_within).sum();
            assert!(
                (sum - syllables).abs() < 1e-9,
                "{text}: {sum} vs {syllables}"
            );
            assert!(
                (paused - pauses).abs() < 1e-9,
                "{text}: {paused} vs {pauses}"
            );
            // With no duration, the record runs the estimate's own length.
            let timing = estimate_words(text, f64::NAN, None, 1.0);
            let length = estimate(text, None, 1.0).seconds;
            assert!((timing.duration - length).abs() < 1e-6);
        }
    }

    /// A placeholder's words fill its file exactly: 0.2 s in, 0.2 s out,
    /// in order, every word a positive span, the pauses where the
    /// punctuation is.
    #[test]
    fn estimate_words_fill_the_placeholder_exactly() {
        let text = "Meet Euclid. Geometry, proved step by step.";
        let duration = silent_mp3_seconds(estimate(text, None, 1.0).seconds);
        let timing = estimate_words(text, duration, None, 1.0);
        assert!(timing.estimated);
        assert_eq!(timing.source, crate::speech::TimingSource::Estimate);
        assert_eq!(timing.confidence, 0.3);
        assert_eq!(timing.words.len(), 7);
        assert_eq!(timing.words[0].start, 0.2);
        let last = timing.words.last().unwrap();
        assert!(
            (last.end - (duration - 0.2)).abs() < 2e-6,
            "{} vs {duration}",
            last.end
        );
        for pair in timing.words.windows(2) {
            assert!(pair[0].start < pair[0].end && pair[0].end <= pair[1].start);
        }
        // The sentence break is longer than the comma, which is longer
        // than a plain word gap (none).
        let gap = |i: usize| timing.words[i + 1].start - timing.words[i].end;
        assert!(gap(1) > gap(2) && gap(2) > gap(3) && gap(3).abs() < 1e-6);
        assert_eq!(timing.sentences.len(), 2);
        assert_eq!(timing.sentences[1].start, timing.words[2].start);
        // A take too short for its padding still lays out in order.
        let tiny = estimate_words(text, 0.3, None, 1.0);
        assert!(tiny.words.last().unwrap().end <= 0.3 + 1e-9);
        assert!(estimate_words("", 1.0, None, 1.0).words.is_empty());
    }

    /// The silent take is whole 36 ms frames of 144 bytes, MPEG-1 Layer III.
    #[test]
    fn silent_mp3_is_whole_frames() {
        let mp3 = silent_mp3(3.0);
        assert_eq!(mp3.len() % 144, 0);
        assert_eq!(&mp3[..2], &[0xFF, 0xFB]);
        let seconds = silent_mp3_seconds(3.0);
        assert!((3.0..3.04).contains(&seconds));
        assert_eq!(mp3.len() / 144, (seconds / 0.036).round() as usize);
    }
}

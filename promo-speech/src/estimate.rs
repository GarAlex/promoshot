//! How long words take to say, without saying them — and a silent take of
//! that length. A narration with no provider key still has a length the
//! cut can be timed against: a placeholder whose file is silence, marked
//! as such, replaced by the real take the moment a key exists.
//!
//! The estimate counts syllables the way each writing system spells them
//! (a Hangul block or a Chinese character is one; kana are moras; Latin,
//! Cyrillic and Greek count vowel groups; Indic scripts count consonant
//! clusters; Arabic and Hebrew, which leave vowels unwritten, go by letter
//! count), spells out numbers, adds the pauses punctuation asks for, and
//! divides by a speaking rate — the language's default, or better, the
//! rate this project's own spoken clips in that voice actually ran at.

use serde_json::Value;

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

/// Syllables and pauses in `text`, read as `language` (or by its script).
fn count(text: &str, language: Option<&str>) -> (f64, f64) {
    let script = dominant_script(text);
    let lang = base_language(language);
    let english = match lang.as_deref() {
        Some(code) => code == "en",
        None => script == Script::Latin,
    };
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
/// plus pauses and padding.
fn raw_seconds(text: &str, language: Option<&str>) -> (f64, f64, f64) {
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

/// How this project's own spoken takes in `provider`/`voice` ran against
/// the estimate: the median of actual ÷ estimated over every narration
/// whose receipt holds and whose length is measured — placeholders and
/// stale takes left out. 1 when there is no take yet; held to 0.6–1.6 so
/// one odd clip cannot run away with it.
pub fn calibration(doc: &Value, provider: &str, voice: &str) -> f64 {
    let mut ratios: Vec<f64> = doc
        .get("resources")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(|resource| {
            let speech = resource.get("speech")?;
            if speech.get("placeholder").and_then(Value::as_bool) == Some(true) {
                return None;
            }
            let text = speech.get("text")?.as_str()?;
            let p = speech
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or("openai");
            let v = speech
                .get("voiceID")
                .and_then(Value::as_str)
                .unwrap_or("alloy");
            if p != provider || v != voice {
                return None;
            }
            let receipt = speech.get("renderedHash")?.as_str()?;
            if receipt != crate::fingerprint(p, v, text) {
                return None;
            }
            let actual = resource.get("duration")?.as_f64()?;
            let (estimated, _, _) = raw_seconds(text, voice_language(v).as_deref());
            (estimated > 0.5 && actual > 0.0).then(|| actual / estimated)
        })
        .collect();
    if ratios.is_empty() {
        return 1.0;
    }
    ratios.sort_by(f64::total_cmp);
    let mid = ratios.len() / 2;
    let median = if ratios.len().is_multiple_of(2) {
        (ratios[mid - 1] + ratios[mid]) / 2.0
    } else {
        ratios[mid]
    };
    median.clamp(0.6, 1.6)
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

    /// Calibration is the median of actual ÷ estimate over this voice's
    /// takes whose receipt holds; placeholders and other voices don't count.
    #[test]
    fn calibration_learns_the_voice() {
        let text = "Narration is written here, not recorded.";
        let estimated = estimate(text, None, 1.0).seconds;
        let take = |voice: &str, duration: f64, placeholder: bool| {
            serde_json::json!({"id": "a", "duration": duration, "speech": {
                "text": text, "provider": "openai", "voiceID": voice,
                "renderedHash": crate::fingerprint("openai", voice, text),
                "placeholder": placeholder }})
        };
        let doc = serde_json::json!({"resources": [
            take("nova", estimated * 1.3, false),
            take("nova", estimated * 1.25, false),
            take("nova", estimated * 1.35, false),
            take("nova", estimated * 9.0, true),
            take("alloy", estimated * 0.7, false),
        ]});
        assert!((calibration(&doc, "openai", "nova") - 1.3).abs() < 1e-9);
        assert!((calibration(&doc, "openai", "alloy") - 0.7).abs() < 1e-9);
        assert_eq!(calibration(&doc, "openai", "echo"), 1.0);
    }

    /// A voice's name says its language when it can.
    #[test]
    fn voice_names_say_their_language() {
        assert_eq!(voice_language("en-US-Neural2-F").as_deref(), Some("en-US"));
        assert_eq!(voice_language("ja-JP-Wavenet-B").as_deref(), Some("ja-JP"));
        assert_eq!(voice_language("alloy"), None);
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

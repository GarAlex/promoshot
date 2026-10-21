//! Speech values resolved to seconds (docs/SPEECH-SYNC.md §6): a time
//! field that says "word 12 of the intro" becomes the second that word is
//! said on the output timeline — through the layer that plays the take,
//! its trims, speed and cuts — plus an offset (seconds or beats) and onto
//! the beat grid when asked. It runs inside the beat pass's loop, so a
//! clock layer that is itself sequenced or on a beat settles first. The
//! engine never learns a word exists: it reads the numbers.

use std::collections::{BTreeMap, BTreeSet};

use promo_model::speech::{
    fold_word, During, SpeechClock, SpeechEdge, SpeechOffset, SpeechPoint, SpeechSnap, SpeechTime,
    TakeTiming, WordRef,
};
use promo_model::{ProjectLayer, ProjectLayerKind, ProjectResource, ProjectResourceKind};

use crate::beats::Grid;
use crate::musical::{BeatGrids, Scope, TimingProblem};

/// A layer that plays a take, as the round found it.
#[derive(Debug, Clone)]
pub(crate) struct Player {
    pub id: String,
    pub resource: String,
    pub start: f64,
    pub end: Option<f64>,
    pub cut: Option<String>,
}

/// One scope's speech clock and the takes its layers play.
pub(crate) struct Speech<'a> {
    resources: &'a [ProjectResource],
    inputs: &'a BeatGrids,
    clock: Option<&'a SpeechClock>,
    players: Vec<Player>,
}

/// Where a take's source second `s` lands on the output timeline.
#[derive(Debug, Clone)]
enum Placement {
    Player(Player),
    /// A clock's stated `startTime`, the take at its own speed.
    Stated(f64),
}

/// A take a value counts on, with its words.
#[derive(Debug, Clone)]
pub(crate) struct Take {
    pub id: String,
    resource: ProjectResource,
    pub timing: TakeTiming,
    placement: Placement,
    /// No record: laid out on an estimate.
    pub estimated_now: bool,
}

impl Take {
    /// Output seconds of the take's source second `s` — None when the layer
    /// never plays it (trimmed out, or past the layer's end).
    pub fn out(&self, s: f64) -> Option<f64> {
        match &self.placement {
            Placement::Stated(origin) => {
                Some(origin + s / crate::mapping::effective_speed(&self.resource))
            }
            Placement::Player(p) => {
                let material = crate::mapping::resource_for_cut(&self.resource, p.cut.as_deref());
                let t = p.start + crate::mapping::timeline_output_time_for_source(&material, s)?;
                match p.end {
                    Some(end) if t > end + 1e-6 => None,
                    _ => Some(t),
                }
            }
        }
    }

    /// The layer that plays it, when one does.
    pub fn player(&self) -> Option<&Player> {
        match &self.placement {
            Placement::Player(p) => Some(p),
            Placement::Stated(_) => None,
        }
    }
}

/// Why a value cannot be resolved.
pub(crate) enum Fault {
    /// The word, sentence or index is not in the take.
    Word(String),
    /// The take cannot be found, or says nothing.
    Take(String),
    /// The word is in the take but the layer never plays it.
    Trimmed(String),
}

impl Fault {
    /// The sentence that says what is wrong.
    pub fn why(self) -> String {
        match self {
            Fault::Word(why) | Fault::Take(why) | Fault::Trimmed(why) => why,
        }
    }
}

/// A resolved value: the output seconds, the word it landed on, and what
/// to say about it.
pub(crate) struct Resolved {
    pub seconds: f64,
    pub word: Option<(u32, String)>,
    pub take: String,
    pub warnings: Vec<TimingProblem>,
}

impl<'a> Speech<'a> {
    pub fn new(
        clock: Option<&'a SpeechClock>,
        layers: &[ProjectLayer],
        resources: &'a [ProjectResource],
        inputs: &'a BeatGrids,
    ) -> Speech<'a> {
        fn walk(layers: &[ProjectLayer], out: &mut Vec<Player>) {
            for layer in layers {
                if layer.is_enabled
                    && matches!(
                        layer.kind,
                        ProjectLayerKind::Audio | ProjectLayerKind::Video
                    )
                {
                    if let Some(id) = &layer.resource_id {
                        out.push(Player {
                            id: layer.id.clone(),
                            resource: id.clone(),
                            start: layer.start_time,
                            end: layer.duration.map(|d| layer.start_time + d),
                            cut: layer.media_cut_id.clone(),
                        });
                    }
                }
                if let Some(members) = layer.members.as_deref() {
                    walk(members, out);
                }
            }
        }
        let mut players = Vec::new();
        walk(layers, &mut players);
        players.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.id.cmp(&b.id)));
        Speech {
            resources,
            inputs,
            clock,
            players,
        }
    }

    fn resource(&self, id: &str) -> Option<&'a ProjectResource> {
        self.resources.iter().find(|r| r.id == id)
    }

    /// The take a value of `owner` names (`of`, else the owner's speech
    /// clock, else the scope's, else the scope's only take).
    pub fn take_for(
        &self,
        own_clock: Option<&SpeechClock>,
        owner: &str,
        of: Option<&str>,
    ) -> Result<(Take, Vec<TimingProblem>), Fault> {
        let mut warnings = Vec::new();
        let clock = own_clock.or(self.clock);
        let named: String = match of {
            Some(of) => of.to_string(),
            None => match clock {
                Some(c) if c.layer.is_some() => c.layer.clone().unwrap_or_default(),
                Some(c) if c.resource_id.is_some() => c.resource_id.clone().unwrap_or_default(),
                _ => {
                    let mut takes: Vec<&str> = self
                        .players
                        .iter()
                        .filter(|p| {
                            self.resource(&p.resource)
                                .is_some_and(|r| promo_model::speech::take_script(r).is_some())
                        })
                        .map(|p| p.resource.as_str())
                        .collect();
                    takes.dedup();
                    takes.sort();
                    takes.dedup();
                    match takes.as_slice() {
                        [one] => {
                            warnings.push(TimingProblem::ImplicitTake {
                                layer: owner.to_string(),
                                take: one.to_string(),
                            });
                            one.to_string()
                        }
                        [] => {
                            return Err(Fault::Take(
                                "names no take, and nothing here plays one — add \"of\"".into(),
                            ))
                        }
                        many => {
                            return Err(Fault::Take(format!(
                                "names no take, and several play here ({}) — add \"of\", or a \
                                 speechClock",
                                many.join(", ")
                            )))
                        }
                    }
                }
            },
        };
        // A layer id: that layer and what it plays.
        let (resource, placement) = if let Some(p) = self.players.iter().find(|p| p.id == named) {
            let resource = self
                .resource(&p.resource)
                .ok_or_else(|| Fault::Take(format!("layer \"{named}\" plays no resource here")))?;
            (resource, Placement::Player(p.clone()))
        } else {
            let resource = self.resource(&named).ok_or_else(|| {
                Fault::Take(format!(
                    "\"{named}\" is no take here — a resource or a layer that plays one"
                ))
            })?;
            if !matches!(
                resource.kind,
                ProjectResourceKind::Audio | ProjectResourceKind::Video
            ) {
                return Err(Fault::Take(format!("\"{named}\" is not audio or video")));
            }
            let playing: Vec<&Player> = self
                .players
                .iter()
                .filter(|p| p.resource == named)
                .collect();
            match playing.first() {
                Some(p) => {
                    if playing.len() > 1 {
                        warnings.push(TimingProblem::SeveralPlayers {
                            take: named.clone(),
                            layers: playing.iter().map(|p| p.id.clone()).collect(),
                        });
                    }
                    (resource, Placement::Player((*p).clone()))
                }
                None => match clock
                    .filter(|c| c.resource_id.as_deref() == Some(named.as_str()))
                    .and_then(|c| c.start_time)
                    .filter(|s| s.is_finite())
                {
                    Some(start) => (resource, Placement::Stated(start)),
                    None => {
                        return Err(Fault::Take(format!(
                            "nothing here plays \"{named}\" — add a layer that plays it, or a \
                             speechClock {{\"resourceID\": \"{named}\", \"startTime\": …}}"
                        )))
                    }
                },
            }
        };
        let script = promo_model::speech::take_script(resource).ok_or_else(|| {
            Fault::Take(format!(
                "\"{}\" says nothing: give it speech.text, or a transcript",
                resource.id
            ))
        })?;
        let (mut timing, estimated_now) = match self.inputs.words.by_resource.get(&resource.id) {
            Some(record) => (record.clone(), false),
            None => {
                let length = resource
                    .duration
                    .filter(|d| *d > 0.0)
                    .or(match &placement {
                        Placement::Player(p) => p.end.map(|e| e - p.start).filter(|d| *d > 0.0),
                        Placement::Stated(_) => None,
                    })
                    .unwrap_or(f64::NAN);
                let voice = resource
                    .extra
                    .get("speech")
                    .and_then(|s| s.get("voiceID"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let language = promo_model::estimate::voice_language(voice);
                (
                    promo_model::estimate::estimate_words(
                        &script,
                        length,
                        language.as_deref(),
                        1.0,
                    ),
                    true,
                )
            }
        };
        if let Ok(fixes) = promo_model::speech::word_fixes(resource) {
            let _ = promo_model::speech::apply_fixes(&mut timing, &fixes);
        }
        let mut resource = resource.clone();
        if resource.duration.is_none_or(|d| d <= 0.0) {
            resource.duration = Some(timing.duration);
        }
        Ok((
            Take {
                id: resource.id.clone(),
                resource,
                timing,
                placement,
                estimated_now,
            },
            warnings,
        ))
    }

    /// `value` (a field of `owner`) in output seconds.
    pub fn resolve(
        &self,
        owner: &ProjectLayer,
        value: &SpeechTime,
        default: SpeechEdge,
        grid: &Grid,
    ) -> Result<Resolved, Fault> {
        let (take, mut warnings) = self.take_for(
            owner.speech_clock.as_ref(),
            &owner.name,
            value.of.as_deref(),
        )?;
        let edge = value.edge.unwrap_or(default);
        let words = &take.timing.words;
        let (mut seconds, word) = match &value.at {
            SpeechPoint::Snap { to, near } => {
                let candidates: Vec<(f64, Option<usize>)> = match to {
                    SpeechSnap::Word => words
                        .iter()
                        .enumerate()
                        .map(|(i, w)| (w.start, Some(i)))
                        .collect(),
                    SpeechSnap::WordEnd => words
                        .iter()
                        .enumerate()
                        .map(|(i, w)| (w.end, Some(i)))
                        .collect(),
                    SpeechSnap::Gap => words
                        .windows(2)
                        .map(|p| ((p[0].end + p[1].start) / 2.0, None))
                        .collect(),
                    SpeechSnap::Sentence => take
                        .timing
                        .sentences
                        .iter()
                        .map(|s| (s.start, Some(s.words[0] as usize)))
                        .collect(),
                };
                let best = candidates
                    .iter()
                    .filter_map(|(s, i)| Some((take.out(*s)?, *i)))
                    .min_by(|a, b| (a.0 - near).abs().total_cmp(&(b.0 - near).abs()))
                    .ok_or_else(|| {
                        Fault::Word(format!(
                            "\"{}\" has no {} the layer plays",
                            take.id,
                            to.name()
                        ))
                    })?;
                let word = best.1.map(|i| (i as u32 + 1, words[i].text.clone()));
                (best.0, word)
            }
            point => {
                let (source, index) = self.point(
                    &take,
                    point,
                    value.guard.as_deref(),
                    edge,
                    owner,
                    &mut warnings,
                )?;
                let word = index.map(|i| (i as u32 + 1, words[i].text.clone()));
                let out = take.out(source).ok_or_else(|| {
                    Fault::Trimmed(match &word {
                        Some((n, text)) => format!(
                            "word {n} \"{text}\" is trimmed out of layer \"{}\"",
                            take.player().map(|p| p.id.as_str()).unwrap_or("?")
                        ),
                        None => format!(
                            "{} is trimmed out of layer \"{}\"",
                            value.label(),
                            take.player().map(|p| p.id.as_str()).unwrap_or("?")
                        ),
                    })
                })?;
                (out, word)
            }
        };
        match value.offset {
            Some(SpeechOffset::Seconds(n)) => seconds += n,
            Some(SpeechOffset::Beats(n)) => seconds = grid.time_at(grid.index_at(seconds) + n),
            Some(SpeechOffset::Bars(n)) => {
                seconds = grid.time_at(grid.index_at(seconds) + grid.bars(n))
            }
            None => {}
        }
        if let Some(unit) = value.quantize {
            seconds = grid.time_at(grid.snap(unit, seconds));
        }
        Ok(Resolved {
            seconds,
            word,
            take: take.id.clone(),
            warnings,
        })
    }

    /// A word or sentence point in the take's source seconds, and the word
    /// index it is on.
    fn point(
        &self,
        take: &Take,
        point: &SpeechPoint,
        guard: Option<&str>,
        edge: SpeechEdge,
        owner: &ProjectLayer,
        warnings: &mut Vec<TimingProblem>,
    ) -> Result<(f64, Option<usize>), Fault> {
        let words = &take.timing.words;
        let at = |i: usize| match edge {
            SpeechEdge::Start => words[i].start,
            SpeechEdge::End => words[i].end,
        };
        match point {
            SpeechPoint::Word(n) => {
                let i = *n as usize - 1;
                if i >= words.len() {
                    return Err(Fault::Word(format!(
                        "word {n} is past the end of \"{}\" ({} words)",
                        take.id,
                        words.len()
                    )));
                }
                if let Some(guard) = guard {
                    let wanted = fold_word(guard);
                    if fold_word(&words[i].text) != wanted {
                        let found = words
                            .iter()
                            .enumerate()
                            .filter(|(_, w)| fold_word(&w.text) == wanted)
                            .min_by_key(|(k, _)| (k.abs_diff(i), *k))
                            .map(|(k, _)| k);
                        return match found {
                            Some(k) => {
                                warnings.push(TimingProblem::GuardMoved {
                                    layer: owner.name.clone(),
                                    word: *n,
                                    now: words[i].text.clone(),
                                    guard: guard.to_string(),
                                    to: k as u32 + 1,
                                });
                                Ok((at(k), Some(k)))
                            }
                            None => Err(Fault::Word(format!(
                                "word {n} is now \"{}\", and \"{}\" says no \"{guard}\"",
                                words[i].text, take.id
                            ))),
                        };
                    }
                }
                Ok((at(i), Some(i)))
            }
            SpeechPoint::WordText { text, nth } => {
                let wanted = fold_word(text);
                let hits: Vec<usize> = words
                    .iter()
                    .enumerate()
                    .filter(|(_, w)| fold_word(&w.text) == wanted)
                    .map(|(k, _)| k)
                    .collect();
                match hits.get(*nth as usize - 1) {
                    Some(&k) => Ok((at(k), Some(k))),
                    None if hits.is_empty() => {
                        let closest = words
                            .iter()
                            .enumerate()
                            .min_by_key(|(_, w)| distance(&fold_word(&w.text), &wanted))
                            .map(|(k, w)| format!(" (closest: word {} \"{}\")", k + 1, w.text))
                            .unwrap_or_default();
                        Err(Fault::Word(format!(
                            "no word \"{text}\" in \"{}\"{closest}",
                            take.id
                        )))
                    }
                    None => Err(Fault::Word(format!(
                        "\"{}\" says \"{text}\" {} time{}, not {nth}",
                        take.id,
                        hits.len(),
                        if hits.len() == 1 { "" } else { "s" }
                    ))),
                }
            }
            SpeechPoint::Sentence(n) => {
                let sentences = &take.timing.sentences;
                let Some(s) = sentences.get(*n as usize - 1) else {
                    return Err(Fault::Word(format!(
                        "sentence {n} is past the end of \"{}\" ({} sentences)",
                        take.id,
                        sentences.len()
                    )));
                };
                Ok(match edge {
                    SpeechEdge::Start => (s.start, Some(s.words[0] as usize)),
                    SpeechEdge::End => (s.end, Some(s.words[1] as usize)),
                })
            }
            SpeechPoint::Snap { .. } => unreachable!("snaps are resolved on the output timeline"),
        }
    }

    /// A `during`'s span in output seconds, its take and the core's words
    /// for it.
    pub fn during(
        &self,
        owner: &ProjectLayer,
        during: &During,
    ) -> Result<(f64, f64, String, String), Fault> {
        let (take, _) = self.take_for(
            owner.speech_clock.as_ref(),
            &owner.name,
            during.of.as_deref(),
        )?;
        let words = &take.timing.words;
        let (first, last) = match (&during.word, during.words, during.sentence) {
            (Some(word), None, None) => {
                let i = match word {
                    WordRef::Index(n) => (*n as usize)
                        .checked_sub(1)
                        .filter(|i| *i < words.len())
                        .ok_or_else(|| {
                            Fault::Word(format!("word {n} is past the end of \"{}\"", take.id))
                        })?,
                    WordRef::Text(text) => words
                        .iter()
                        .position(|w| fold_word(&w.text) == fold_word(text))
                        .ok_or_else(|| {
                            Fault::Word(format!("no word \"{text}\" in \"{}\"", take.id))
                        })?,
                };
                (i, i)
            }
            (None, Some([a, b]), None) if a >= 1 && a <= b && (b as usize) <= words.len() => {
                (a as usize - 1, b as usize - 1)
            }
            (None, Some([a, b]), None) => {
                return Err(Fault::Word(format!(
                    "words {a}–{b} are not a span of \"{}\" ({} words)",
                    take.id,
                    words.len()
                )))
            }
            (None, None, Some(n)) => {
                let s = take
                    .timing
                    .sentences
                    .get((n as usize).saturating_sub(1))
                    .filter(|_| n >= 1)
                    .ok_or_else(|| {
                        Fault::Word(format!("sentence {n} is past the end of \"{}\"", take.id))
                    })?;
                (s.words[0] as usize, s.words[1] as usize)
            }
            _ => {
                return Err(Fault::Word(
                    "during needs exactly one of \"word\", \"words\" or \"sentence\"".into(),
                ))
            }
        };
        let start = take.out(words[first].start).ok_or_else(|| {
            Fault::Trimmed(format!(
                "word {} \"{}\" is trimmed out",
                first + 1,
                words[first].text
            ))
        })?;
        let end = take.out(words[last].end).ok_or_else(|| {
            Fault::Trimmed(format!(
                "word {} \"{}\" is trimmed out",
                last + 1,
                words[last].text
            ))
        })?;
        let start = start - during.lead.unwrap_or(0.0).max(0.0);
        let end = end + during.hold.unwrap_or(0.0).max(0.0);
        let words_of = |i: usize| format!("word {} \u{201C}{}\u{201D}", i + 1, words[i].text);
        let label = match (during.sentence, first == last) {
            (Some(n), _) => format!("during sentence {n}"),
            (None, true) => format!("during {}", words_of(first)),
            (None, false) => format!("from {} to {}", words_of(first), words_of(last)),
        };
        Ok((start, end, take.id, label))
    }
}

impl Fault {
    /// The problem a fault at `field` of layer `layer` is.
    pub(crate) fn problem(self, layer: &str, field: &str, spec: String) -> TimingProblem {
        let (layer, field) = (layer.to_string(), field.to_string());
        match self {
            Fault::Word(why) => TimingProblem::SpeechNoWord {
                layer,
                field,
                spec,
                why,
            },
            Fault::Take(why) => TimingProblem::SpeechTake {
                layer,
                field,
                spec,
                why,
            },
            Fault::Trimmed(why) => TimingProblem::SpeechTrimmedOut {
                layer,
                field,
                spec,
                why,
            },
        }
    }
}

/// Whether a sequence counts on a take: a `per` entry, or an `until` that
/// names words.
pub(crate) fn sequence_uses_speech(
    sequence: Option<&[promo_model::musical::SequenceEntry]>,
) -> bool {
    sequence.unwrap_or(&[]).iter().any(|e| {
        e.per.is_some()
            || e.until
                .as_ref()
                .and_then(serde_json::Value::as_object)
                .is_some_and(promo_model::speech::is_speech_object)
    })
}

/// A sequence with its `per` entries made into one entry per layer
/// (docs/SPEECH-SYNC.md §7.3): slide k lasts until sentence k+1's start −
/// `lead`, the last until the last sentence's end + `hold` — each an
/// `until` the pass resolves like any speech value, `quantize` with it.
pub(crate) fn expand_per(
    sequence: &[promo_model::musical::SequenceEntry],
    speech: &Speech,
) -> (Vec<promo_model::musical::SequenceEntry>, Vec<TimingProblem>) {
    use serde_json::json;
    let mut out = Vec::new();
    let mut problems = Vec::new();
    for entry in sequence {
        let Some(per) = entry.per.as_deref() else {
            out.push(entry.clone());
            continue;
        };
        let layers = entry.layers.clone().unwrap_or_default();
        let here = format!("sequence per {per}");
        if per != "sentence" {
            problems.push(TimingProblem::Generator {
                layer: here,
                why: format!("\"per\": \"{per}\" — only \"sentence\" is known"),
            });
            continue;
        }
        let take = match speech.take_for(None, &here, entry.of.as_deref()) {
            Ok((take, _)) => take,
            Err(f) => {
                problems.push(TimingProblem::Generator {
                    layer: here,
                    why: f.why(),
                });
                continue;
            }
        };
        let sentences = take.timing.sentences.len();
        if layers.is_empty() || sentences == 0 {
            continue;
        }
        if layers.len() != sentences {
            problems.push(TimingProblem::PerSlots {
                take: take.id.clone(),
                layers: layers.len(),
                sentences,
            });
        }
        let lead = entry.lead.unwrap_or(0.0);
        let hold = entry.hold.unwrap_or(0.5);
        let used = layers.len().min(sentences);
        for (k, layer) in layers.iter().take(used).enumerate() {
            let mut until = if k + 1 < used {
                json!({"sentence": k + 2, "of": take.id})
            } else {
                json!({"sentence": sentences, "of": take.id, "edge": "end"})
            };
            let offset = if k + 1 < used { -lead } else { hold };
            if offset != 0.0 {
                until["offset"] = json!(offset);
            }
            if let Some(q) = &entry.quantize {
                until["quantize"] = json!(q);
            }
            out.push(promo_model::musical::SequenceEntry {
                layer: layer.clone(),
                layers: None,
                until: Some(until),
                per: None,
                of: None,
                lead: None,
                hold: None,
                quantize: None,
                beats: None,
                bars: None,
                seconds: None,
                duration: None,
                extra: Default::default(),
            });
        }
    }
    (out, problems)
}

/// Edit distance, for "closest: …".
fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut row = vec![i + 1; b.len() + 1];
        for (j, y) in b.iter().enumerate() {
            row[j + 1] = (previous[j] + usize::from(x != y))
                .min(previous[j + 1] + 1)
                .min(row[j] + 1);
        }
        previous = row;
    }
    previous[b.len()]
}

/// The speech fields a layer's specs may hold, and the edge each defaults
/// to: a start is a word's start, a duration runs to its end.
const POSITIONS: &[(&str, SpeechEdge)] = &[
    ("startTime", SpeechEdge::Start),
    ("duration", SpeechEdge::End),
];

/// A resolved speech value, for validate's "Speech times".
#[derive(Debug, Clone, PartialEq)]
pub struct SpeechRow {
    /// `layers[s3].startTime`.
    pub at: String,
    pub spec: String,
    /// The number written at the field (a keyframe's is layer-local, a
    /// duration a length).
    pub seconds: f64,
    /// The output instant it names: the start, the end, the keyframe's
    /// moment.
    pub instant: f64,
    /// The core's words for where it landed: `word 4 “Primes”`, `ends with
    /// sentence 2`.
    pub label: String,
    pub take: String,
    /// The word it landed on (1-based) and its text.
    pub word: Option<(u32, String)>,
    /// The take's timing source (`provider:elevenlabs`, `aligned 0.72`,
    /// `estimate`).
    pub source: String,
}

/// One pass over a scope's layers (stage members included): every speech
/// start, length, keyframe time and `during`, written as seconds. Answers
/// the problems and, with `rows`, the listing.
#[allow(clippy::too_many_arguments)]
pub(crate) fn speech_pass(
    layers: &mut [ProjectLayer],
    speech: &Speech,
    scope: &Scope,
    during_ids: &BTreeSet<String>,
    at: &str,
    rows: &mut Vec<SpeechRow>,
    used: &mut BTreeSet<String>,
) -> Vec<TimingProblem> {
    let mut problems = Vec::new();
    for layer in layers.iter_mut() {
        let here = format!("{at}[{}]", layer.id);
        let grid = scope.grid_of(layer);
        let fault = |problems: &mut Vec<TimingProblem>,
                     layer: &ProjectLayer,
                     field: &str,
                     spec: String,
                     fault: Fault| {
            problems.push(match fault {
                Fault::Word(why) => TimingProblem::SpeechNoWord {
                    layer: layer.name.clone(),
                    field: field.into(),
                    spec,
                    why,
                },
                Fault::Take(why) => TimingProblem::SpeechTake {
                    layer: layer.name.clone(),
                    field: field.into(),
                    spec,
                    why,
                },
                Fault::Trimmed(why) => TimingProblem::SpeechTrimmedOut {
                    layer: layer.name.clone(),
                    field: field.into(),
                    spec,
                    why,
                },
            })
        };
        let source = |take: &str| -> String {
            match speech.inputs.words.by_resource.get(take) {
                Some(t) => match &t.source {
                    promo_model::speech::TimingSource::Aligned => {
                        format!("aligned {:.2}", t.confidence)
                    }
                    other => other.as_text(),
                },
                None => "estimate (no timing yet)".into(),
            }
        };
        // `during` places start and length together.
        if let (Some(during), true) = (layer.during.clone(), during_ids.contains(&layer.id)) {
            match speech.during(layer, &during) {
                Ok((start, end, take, span_label)) => {
                    used.insert(take.clone());
                    crate::musical::place_derived(layer, "startTime", "during", start);
                    if end > start {
                        crate::musical::place_derived(layer, "duration", "during", end - start);
                    }
                    rows.push(SpeechRow {
                        at: format!("{here}.during"),
                        spec: serde_json::to_string(&during).unwrap_or_default(),
                        seconds: start,
                        instant: start,
                        label: span_label,
                        take: take.clone(),
                        word: None,
                        source: source(&take),
                    });
                }
                Err(f) => fault(
                    &mut problems,
                    layer,
                    "during",
                    serde_json::to_string(&during).unwrap_or_default(),
                    f,
                ),
            }
        }
        for (key, default) in POSITIONS {
            let Some(value) = layer
                .time_specs
                .as_ref()
                .and_then(|m| m.get(*key))
                .and_then(|e| e.speech())
            else {
                continue;
            };
            match speech.resolve(layer, &value, *default, &grid) {
                Ok(resolved) => {
                    used.insert(resolved.take.clone());
                    problems.extend(resolved.warnings.iter().cloned());
                    let seconds = if *key == "duration" {
                        let length = resolved.seconds - layer.start_time;
                        if length <= 0.0 {
                            problems.push(TimingProblem::EndsBeforeItStarts {
                                layer: layer.name.clone(),
                                spec: value.canonical_text(),
                            });
                            continue;
                        }
                        length
                    } else {
                        resolved.seconds
                    };
                    crate::musical::write_spec(
                        layer,
                        key,
                        seconds,
                        &label(&value, &resolved, *key == "duration"),
                    );
                    rows.push(SpeechRow {
                        at: format!("{here}.{key}"),
                        spec: value.canonical_text(),
                        seconds: promo_model::musical::layer_number(layer, key).unwrap_or(seconds),
                        instant: resolved.seconds,
                        label: label(&value, &resolved, *key == "duration"),
                        take: resolved.take.clone(),
                        word: resolved.word,
                        source: source(&resolved.take),
                    });
                }
                Err(f) => fault(&mut problems, layer, key, value.canonical_text(), f),
            }
        }
        for key in [
            "fadeIn",
            "fadeOut",
            "transitionIn.duration",
            "transitionOut.duration",
        ] {
            if let Some(value) = layer
                .time_specs
                .as_ref()
                .and_then(|m| m.get(key))
                .and_then(|e| e.speech())
            {
                problems.push(TimingProblem::SpeechForLength {
                    layer: layer.name.clone(),
                    field: key.into(),
                    spec: value.canonical_text(),
                });
            }
        }
        let start = layer.start_time;
        let owner = layer.clone();
        for keyframe in &mut layer.keyframes {
            if let Some(value) = keyframe
                .time_specs
                .as_ref()
                .and_then(|m| m.get("transitionDuration"))
                .and_then(|e| e.speech())
            {
                problems.push(TimingProblem::SpeechForLength {
                    layer: owner.name.clone(),
                    field: format!("keyframes[{}].transitionDuration", keyframe.id),
                    spec: value.canonical_text(),
                });
            }
            let Some(value) = keyframe
                .time_specs
                .as_ref()
                .and_then(|m| m.get("time"))
                .and_then(|e| e.speech())
            else {
                continue;
            };
            let field = format!("keyframes[{}].time", keyframe.id);
            match speech.resolve(&owner, &value, SpeechEdge::Start, &grid) {
                Ok(resolved) => {
                    used.insert(resolved.take.clone());
                    problems.extend(resolved.warnings.iter().cloned());
                    let local = resolved.seconds - start;
                    promo_model::musical::set_keyframe_number(keyframe, "time", local);
                    if let Some(entry) =
                        keyframe.time_specs.as_mut().and_then(|m| m.get_mut("time"))
                    {
                        entry.seconds = local;
                        entry.label = label(&value, &resolved, false);
                    }
                    rows.push(SpeechRow {
                        at: format!("{here}.{field}"),
                        spec: value.canonical_text(),
                        seconds: local,
                        instant: resolved.seconds,
                        label: label(&value, &resolved, false),
                        take: resolved.take.clone(),
                        word: resolved.word,
                        source: source(&resolved.take),
                    });
                }
                Err(f) => fault(&mut problems, &owner, &field, value.canonical_text(), f),
            }
        }
        if let Some(members) = layer.members.as_mut() {
            problems.extend(speech_pass(
                members,
                speech,
                scope,
                during_ids,
                &format!("{here}.members"),
                rows,
                used,
            ));
        }
    }
    problems
}

/// The core's words for a resolved value: "word 12 ‘Euclid’", "ends with
/// sentence 2".
fn label(value: &SpeechTime, resolved: &Resolved, ends: bool) -> String {
    let word = resolved
        .word
        .as_ref()
        .map(|(n, text)| format!("word {n} \u{201C}{text}\u{201D}"));
    let base = match (&value.at, word) {
        (SpeechPoint::Sentence(n), _) => format!("sentence {n}"),
        (_, Some(w)) => w,
        (_, None) => value.label(),
    };
    let edge = value.edge.unwrap_or(if ends {
        SpeechEdge::End
    } else {
        SpeechEdge::Start
    });
    match (ends, edge) {
        (true, SpeechEdge::End) => format!("ends with {base}"),
        (true, SpeechEdge::Start) => format!("ends as {base} starts"),
        (false, SpeechEdge::End) => format!("the end of {base}"),
        (false, SpeechEdge::Start) => base,
    }
}

/// Whether anything in the project — its layers or a composition's — is
/// timed on speech: what makes an open align its takes.
pub fn project_uses_speech(meta: &promo_model::ProjectMetadata) -> bool {
    let layers = meta.layers.as_deref().unwrap_or(&[]);
    let resources = meta.resources.as_deref().unwrap_or(&[]);
    crate::speech_effects::any_sync(layers, resources)
        || promo_model::nesting::all_layers(meta)
            .iter()
            .any(|l| l.captions.is_some())
        || uses_speech(layers)
        || meta
            .resources
            .iter()
            .flatten()
            .filter_map(|r| r.composition.as_ref())
            .any(|c| uses_speech(&c.layers) || crate::speech_effects::any_sync(&c.layers, &[]))
}

/// Whether any layer of a scope uses speech timing.
pub(crate) fn uses_speech(layers: &[ProjectLayer]) -> bool {
    layers.iter().any(|l| {
        l.during.is_some()
            || l.time_specs
                .iter()
                .flatten()
                .any(|(_, e)| e.speech().is_some())
            || l.keyframes.iter().any(|k| {
                k.time_specs
                    .iter()
                    .flatten()
                    .any(|(_, e)| e.speech().is_some())
            })
            || l.members.as_deref().is_some_and(uses_speech)
    })
}

/// One take a scope's values counted on, as validate and inspect list it.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeechClockInfo {
    pub scope: String,
    pub take: String,
    /// The layer that plays it, if one does.
    pub layer: Option<String>,
    pub source: String,
    pub confidence: f64,
    pub words: usize,
    pub sentences: usize,
    pub estimated: bool,
    pub stale: bool,
}

/// What a reader of a speech-cut edit wants to hear, for the takes a scope
/// used: timing that is estimated, unsure, stale or doubted; a take longer
/// than the layer that plays it; a script synced by character; and every
/// visual edge that cuts through a word.
#[allow(clippy::too_many_arguments)]
pub(crate) fn findings(
    layers: &[ProjectLayer],
    speech: &Speech,
    used: &BTreeSet<String>,
    fps: f64,
    scope_name: &str,
    problems: &mut Vec<TimingProblem>,
    clocks: &mut Vec<SpeechClockInfo>,
    missing: &mut Vec<String>,
) {
    let frame = 1.0 / fps.max(1.0);
    let mut takes: BTreeMap<String, Take> = BTreeMap::new();
    for id in used {
        // A value named this take, so it resolves: take it as `of` names it.
        if let Ok((take, _)) = speech.take_for(None, "", Some(id)) {
            takes.insert(id.clone(), take);
        }
    }
    for (id, take) in &takes {
        let t = &take.timing;
        let stale = speech.inputs.words.stale.contains(id);
        clocks.push(SpeechClockInfo {
            scope: scope_name.into(),
            take: id.clone(),
            layer: take.player().map(|p| p.id.clone()),
            source: if take.estimated_now {
                "estimate (no timing yet)".into()
            } else {
                t.source.as_text()
            },
            confidence: t.confidence,
            words: t.words.len(),
            sentences: t.sentences.len(),
            estimated: t.estimated,
            stale,
        });
        if take.estimated_now {
            missing.push(id.clone());
            problems.push(TimingProblem::SpeechEstimated { take: id.clone() });
        } else if t.estimated {
            problems.push(TimingProblem::SpeechPlaceholder { take: id.clone() });
        } else if t.confidence < 0.6 {
            problems.push(TimingProblem::SpeechUnsure {
                take: id.clone(),
                confidence: t.confidence,
            });
        }
        if stale {
            problems.push(TimingProblem::SpeechStale { take: id.clone() });
        }
        if let Some(notes) = t
            .extra
            .get("warnings")
            .and_then(serde_json::Value::as_array)
        {
            for note in notes.iter().filter_map(serde_json::Value::as_str) {
                problems.push(TimingProblem::SpeechMayNotSay {
                    take: id.clone(),
                    note: note.into(),
                });
            }
        }
        if t.script
            .chars()
            .any(|c| matches!(c as u32, 0x3040..=0x30FF | 0x4E00..=0x9FFF | 0x0E00..=0x0E7F))
        {
            problems.push(TimingProblem::SpeechByCharacter { take: id.clone() });
        }
        // Words the layer never plays.
        if take.player().is_some() {
            let unheard: Vec<usize> = (0..t.words.len())
                .filter(|&i| take.out(t.words[i].start).is_none())
                .collect();
            if let (Some(a), Some(b)) = (unheard.first(), unheard.last()) {
                problems.push(TimingProblem::TakeLongerThanLayer {
                    take: id.clone(),
                    layer: take.player().map(|p| p.id.clone()).unwrap_or_default(),
                    first: *a as u32 + 1,
                    last: *b as u32 + 1,
                });
            }
        }
    }
    // Visual edges through a word of a take playing then.
    fn walk(layers: &[ProjectLayer], out: &mut Vec<(String, f64)>) {
        for l in layers {
            if let Some(m) = l.members.as_deref() {
                walk(m, out);
            }
            if !l.is_enabled
                || l.generated_by.is_some()
                || matches!(
                    l.kind,
                    ProjectLayerKind::Audio | ProjectLayerKind::Background
                )
            {
                continue;
            }
            out.push((l.name.clone(), l.start_time));
            if let Some(d) = l.duration {
                out.push((l.name.clone(), l.start_time + d));
            }
        }
    }
    let mut edges = Vec::new();
    walk(layers, &mut edges);
    for (name, at) in edges {
        if at <= frame {
            continue;
        }
        for (id, take) in &takes {
            let t = &take.timing;
            let spans: Vec<Option<(f64, f64)>> = t
                .words
                .iter()
                .map(|w| Some((take.out(w.start)?, take.out(w.end)?)))
                .collect();
            let Some((i, (from, to))) = spans.iter().enumerate().find_map(|(i, s)| {
                s.filter(|(a, b)| at > a + frame && at < b - frame)
                    .map(|s| (i, s))
            }) else {
                continue;
            };
            let gaps: Vec<f64> = spans
                .windows(2)
                .filter_map(|p| Some((p[0]?.1 + p[1]?.0) / 2.0))
                .collect();
            let gap = gaps
                .iter()
                .copied()
                .min_by(|a, b| (a - at).abs().total_cmp(&(b - at).abs()));
            problems.push(TimingProblem::CutMidWord {
                layer: name.clone(),
                at,
                word: t.words[i].text.clone(),
                take: id.clone(),
                from,
                to,
                gap,
            });
        }
    }
}

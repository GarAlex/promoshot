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
    /// The layer's own beyond-the-end policy (None: the resource's).
    pub beyond_end: Option<promo_model::BeyondEnd>,
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

/// A word whose own confidence is under this is said beside a value that
/// lands on it: its edge may be a tenth of a second off.
pub const WORD_UNSURE_BELOW: f64 = 0.5;

/// A take a value counts on, with its words.
#[derive(Debug, Clone)]
pub(crate) struct Take {
    pub id: String,
    resource: ProjectResource,
    pub timing: TakeTiming,
    placement: Placement,
    /// No record: laid out on an estimate.
    pub estimated_now: bool,
    /// Which play of a looping layer, 0-based: `out` lands in it.
    play: usize,
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
                let t = p.start
                    + self.play as f64 * self.period().unwrap_or(0.0)
                    + crate::mapping::timeline_output_time_for_source(&material, s)?;
                match p.end {
                    Some(end) if t > end + 1e-6 => None,
                    _ => Some(t),
                }
            }
        }
    }

    /// One play's length on the timeline, when the layer that plays the
    /// take loops it.
    fn period(&self) -> Option<f64> {
        let p = self.player()?;
        let material = crate::mapping::resource_for_cut(&self.resource, p.cut.as_deref());
        let looped = match p.beyond_end {
            Some(promo_model::BeyondEnd::Loop) => true,
            Some(_) => false,
            None => material.is_looped(),
        };
        let period = crate::mapping::timeline_loop_period(&material);
        (looped && period.is_finite() && period > 1e-6).then_some(period)
    }

    /// How many times the layer plays the take: more than one only for a
    /// looping layer with a set end.
    pub fn plays(&self) -> usize {
        match (
            self.period(),
            self.player().and_then(|p| Some((p.start, p.end?))),
        ) {
            (Some(period), Some((start, end))) if end > start => {
                (((end - start) / period - 1e-6).ceil() as usize).clamp(1, 10_000)
            }
            _ => 1,
        }
    }

    /// The same take in play `k` (0-based).
    pub fn in_play(&self, k: usize) -> Take {
        Take {
            play: k,
            ..self.clone()
        }
    }

    /// Every play, in order.
    pub fn each_play(&self) -> Vec<Take> {
        (0..self.plays()).map(|k| self.in_play(k)).collect()
    }

    /// The same take in the play whose window holds output second `t`
    /// (the first before it, the last after it).
    pub fn play_holding(&self, t: f64) -> Take {
        let k = match (self.period(), self.player()) {
            (Some(period), Some(p)) if t.is_finite() => ((t - p.start) / period).floor().max(0.0),
            _ => 0.0,
        };
        self.in_play((k as usize).min(self.plays() - 1))
    }

    /// The play (0-based) now in use.
    pub fn play(&self) -> usize {
        self.play
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
    /// The play of a looping take it landed in, 1-based, when the take
    /// plays more than once (or the value names one).
    pub play: Option<u32>,
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
                            beyond_end: layer.beyond_end,
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
                play: 0,
            },
            warnings,
        ))
    }

    /// `value` (a field of `owner`) in output seconds. On a looping take
    /// with no `play`, `near` says which play counts: a span (a keyframe's
    /// owner's life) settles it when exactly one play lands inside; else
    /// the play landing nearest its start (the field's current number)
    /// counts, and a warning says the take is said more than once.
    pub fn resolve(
        &self,
        owner: &ProjectLayer,
        value: &SpeechTime,
        default: SpeechEdge,
        grid: &Grid,
        near: Option<(f64, f64)>,
    ) -> Result<Resolved, Fault> {
        let (take, mut warnings) = self.take_for(
            owner.speech_clock.as_ref(),
            &owner.name,
            value.of.as_deref(),
        )?;
        let plays = take.plays();
        // The plays a value may land in: the one it names, else all.
        let pool: Vec<Take> = match value.play {
            Some(n) if n as usize > plays => {
                return Err(Fault::Word(format!(
                    "play {n} is past the end: layer \"{}\" plays \"{}\" {plays} time{}",
                    take.player().map(|p| p.id.as_str()).unwrap_or("?"),
                    take.id,
                    if plays == 1 { "" } else { "s" }
                )))
            }
            Some(n) => vec![take.in_play(n as usize - 1)],
            None => take.each_play(),
        };
        let edge = value.edge.unwrap_or(default);
        let words = &take.timing.words;
        let finish = |mut seconds: f64| {
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
            seconds
        };
        let (seconds, word, play) = match &value.at {
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
                        .enumerate()
                        .map(|(i, p)| ((p[0].end + p[1].start) / 2.0, Some(i)))
                        .collect(),
                    SpeechSnap::Sentence => take
                        .timing
                        .sentences
                        .iter()
                        .map(|s| (s.start, Some(s.words[0] as usize)))
                        .collect(),
                };
                // A snap names its own instant, so every play is a
                // candidate and the nearest wins: nothing is ambiguous.
                let best = pool
                    .iter()
                    .flat_map(|t| {
                        candidates
                            .iter()
                            .filter_map(move |(s, i)| Some((t.out(*s)?, *i, t.play())))
                    })
                    .min_by(|a, b| (a.0 - near).abs().total_cmp(&(b.0 - near).abs()))
                    .ok_or_else(|| {
                        Fault::Word(format!(
                            "\"{}\" has no {} the layer plays",
                            take.id,
                            to.name()
                        ))
                    })?;
                let word = best.1.map(|i| (i as u32 + 1, words[i].text.clone()));
                (finish(best.0), word, best.2)
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
                let landed: Vec<(f64, usize)> = pool
                    .iter()
                    .filter_map(|t| Some((finish(t.out(source)?), t.play())))
                    .collect();
                let Some(&first) = landed.first() else {
                    return Err(Fault::Trimmed(match &word {
                        Some((n, text)) => format!(
                            "word {n} \"{text}\" is trimmed out of layer \"{}\"",
                            take.player().map(|p| p.id.as_str()).unwrap_or("?")
                        ),
                        None => format!(
                            "{} is trimmed out of layer \"{}\"",
                            value.label(),
                            take.player().map(|p| p.id.as_str()).unwrap_or("?")
                        ),
                    }));
                };
                // Said more than once: the play nearest the field's number
                // (where it was, or where the editor put it), else the
                // first — and say so, so the file can name one.
                let near = near.filter(|(a, b)| a.is_finite() && b.is_finite());
                let inside: Vec<(f64, usize)> = match near {
                    Some((a, b)) if b > a + 1e-9 => landed
                        .iter()
                        .copied()
                        .filter(|(t, _)| *t >= a - 1e-6 && *t <= b + 1e-6)
                        .collect(),
                    _ => Vec::new(),
                };
                let chosen = inside
                    .first()
                    .copied()
                    .or_else(|| {
                        let (n, _) = near?;
                        landed
                            .iter()
                            .copied()
                            .min_by(|a, b| (a.0 - n).abs().total_cmp(&(b.0 - n).abs()))
                    })
                    .unwrap_or(first);
                if value.play.is_none() && landed.len() > 1 && inside.len() != 1 {
                    warnings.push(TimingProblem::SpeechLoopAmbiguous {
                        layer: owner.name.clone(),
                        spec: value.canonical_text(),
                        take: take.id.clone(),
                        player: take.player().map(|p| p.id.clone()).unwrap_or_default(),
                        plays: landed.len() as u32,
                        used: chosen.1 as u32 + 1,
                    });
                }
                (chosen.0, word, chosen.1)
            }
        };
        // Which play, when there is a choice to name.
        let play = (value.play.is_some() || plays > 1).then_some(play as u32 + 1);
        // A word the timing itself doubts: said beside the value.
        if let Some((n, text)) = &word {
            if let Some(c) = words[*n as usize - 1]
                .confidence
                .filter(|c| *c < WORD_UNSURE_BELOW)
            {
                warnings.push(TimingProblem::SpeechWordUnsure {
                    layer: owner.name.clone(),
                    take: take.id.clone(),
                    word: *n,
                    text: text.clone(),
                    confidence: c,
                });
            }
        }
        Ok(Resolved {
            seconds,
            word,
            take: take.id.clone(),
            play,
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
            // Where the field is now, on the output timeline.
            let near = match *key {
                "duration" => layer.duration.map(|d| layer.start_time + d),
                _ => Some(layer.start_time),
            }
            .map(|n| (n, n));
            match speech.resolve(layer, &value, *default, &grid, near) {
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
            // The owner's life: a keyframe on a word falls in the play
            // the layer is showing.
            let near = Some((start, start + owner.duration.unwrap_or(0.0).max(0.0)));
            match speech.resolve(&owner, &value, SpeechEdge::Start, &grid, near) {
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
    let out = label_in_play(value, resolved, ends);
    match resolved.play {
        Some(n) if !out.contains(&format!("(play {n})")) => format!("{out} (play {n})"),
        _ => out,
    }
}

fn label_in_play(value: &SpeechTime, resolved: &Resolved, ends: bool) -> String {
    let word = resolved
        .word
        .as_ref()
        .map(|(n, text)| format!("word {n} \u{201C}{text}\u{201D}"));
    let base = match (&value.at, word) {
        (SpeechPoint::Sentence(n), _) => format!("sentence {n}"),
        (
            SpeechPoint::Snap {
                to: SpeechSnap::Gap,
                ..
            },
            Some(w),
        ) => {
            return if ends {
                format!("ends at the gap after {w}")
            } else {
                format!("the gap after {w}")
            };
        }
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
    // Edges of layers that carry sound, through a word of a take playing
    // then: a sound that starts or stops mid-word cuts the word. A picture
    // that comes or goes during one cuts nothing heard (issue #18: 38 such
    // warnings on a cut, all on silent image layers, buried the real ones).
    fn walk(
        layers: &[ProjectLayer],
        resources: &[ProjectResource],
        silent: &BTreeSet<String>,
        out: &mut Vec<(String, String, f64)>,
    ) {
        for l in layers {
            if let Some(m) = l.members.as_deref() {
                walk(m, resources, silent, out);
            }
            if !l.is_enabled || l.generated_by.is_some() || !carries_sound(l, resources, silent, 0)
            {
                continue;
            }
            out.push((l.id.clone(), l.name.clone(), l.start_time));
            if let Some(d) = l.duration {
                out.push((l.id.clone(), l.name.clone(), l.start_time + d));
            }
        }
    }
    let mut edges = Vec::new();
    walk(
        layers,
        speech.resources,
        &speech.inputs.silent_media,
        &mut edges,
    );
    for (layer_id, name, at) in edges {
        if at <= frame {
            continue;
        }
        for (id, take) in &takes {
            // The take's own layer ending inside it is the take being
            // longer than its layer, said once as that.
            if take.player().is_some_and(|p| p.id == layer_id) {
                continue;
            }
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

/// Whether a layer makes sound an edge of it could cut: an audio layer on
/// something with sound, a video whose file is not known to be silent and
/// whose volume is not zero, a composition with such a layer inside, a
/// caption with a narration clip. Pictures — images, drawings, models, a
/// background, a stage itself — carry none.
fn carries_sound(
    layer: &ProjectLayer,
    resources: &[ProjectResource],
    silent: &BTreeSet<String>,
    depth: usize,
) -> bool {
    match layer.kind {
        ProjectLayerKind::Audio | ProjectLayerKind::Video => {
            if let Some(shown) = promo_model::nesting::composition_of(layer, resources) {
                return depth < promo_model::nesting::MAX_DEPTH
                    && shown.composition.as_ref().is_some_and(|c| {
                        c.layers.iter().any(|inner| {
                            inner.is_enabled && carries_sound(inner, resources, silent, depth + 1)
                        })
                    });
            }
            crate::audio::sound_resource(layer, resources).is_some_and(|r| {
                r.effective_volume() > 0.0
                    && !(r.kind == ProjectResourceKind::Video && silent.contains(&r.id))
            })
        }
        ProjectLayerKind::Caption => {
            layer.caption_voice_clip.is_some()
                || layer.resource_id.as_deref().is_some_and(|id| {
                    resources
                        .iter()
                        .any(|r| r.id == id && r.caption_voice_clip.is_some())
                })
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// The editors' questions (docs/SPEECH-SYNC.md §8.2): a layer edge snapped to
// a word, a gap or a sentence, and the words to draw under a voice-over.

/// What a speech snap lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeechUnit {
    Word,
    Gap,
    Sentence,
}

impl SpeechUnit {
    pub fn parse(text: &str) -> Option<SpeechUnit> {
        match text {
            "word" => Some(SpeechUnit::Word),
            "gap" => Some(SpeechUnit::Gap),
            "sentence" => Some(SpeechUnit::Sentence),
            _ => None,
        }
    }
}

/// A layer edge snapped to speech: the value an editor writes with
/// `setTime`, the seconds it resolves to (a start's position, an end's
/// length) and the core's words for it.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeechSnapped {
    pub value: serde_json::Value,
    pub seconds: f64,
    pub label: String,
}

/// One word of a take on the output timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct WordTick {
    /// 1-based.
    pub index: u32,
    pub text: String,
    pub start: f64,
    pub end: f64,
    /// The word opens a sentence.
    pub sentence_start: bool,
    pub confidence: Option<f64>,
    pub prominence: Option<f64>,
}

/// A layer, its scope's layers and the scope's speech clock — the project's
/// (stage members included) or a composition's.
fn scope_of<'a>(
    meta: &'a promo_model::ProjectMetadata,
    layer_id: &str,
) -> Option<(
    &'a ProjectLayer,
    &'a [ProjectLayer],
    Option<&'a SpeechClock>,
)> {
    fn find<'a>(layers: &'a [ProjectLayer], id: &str) -> Option<&'a ProjectLayer> {
        layers.iter().find_map(|l| {
            if l.id == id {
                Some(l)
            } else {
                l.members.as_deref().and_then(|m| find(m, id))
            }
        })
    }
    let layers = meta.layers.as_deref().unwrap_or(&[]);
    if let Some(layer) = find(layers, layer_id) {
        return Some((
            layer,
            layers,
            meta.composition_settings.speech_clock.as_ref(),
        ));
    }
    meta.resources.iter().flatten().find_map(|r| {
        let c = r.composition.as_ref()?;
        Some((
            find(&c.layers, layer_id)?,
            c.layers.as_slice(),
            c.speech_clock.as_ref(),
        ))
    })
}

impl<'a> Speech<'a> {
    /// The take a layer's edge at `t` counts on: its own or the scope's
    /// speech clock, else the take playing at `t`, else the scope's only.
    fn take_at(&self, owner: &ProjectLayer, t: f64) -> Result<Take, String> {
        if owner.speech_clock.is_some() || self.clock.is_some() {
            return self
                .take_for(owner.speech_clock.as_ref(), &owner.name, None)
                .map(|(take, _)| take)
                .map_err(Fault::why);
        }
        let playing = self.players.iter().find(|p| {
            p.start <= t + 1e-9
                && p.end.is_none_or(|e| t <= e + 1e-9)
                && self
                    .resource(&p.resource)
                    .is_some_and(|r| promo_model::speech::take_script(r).is_some())
        });
        let of = playing.map(|p| p.id.clone());
        self.take_for(None, &owner.name, of.as_deref())
            .map(|(take, _)| take)
            .map_err(Fault::why)
    }
}

/// The candidates a unit offers on a take, in output seconds, in every
/// play of a looping take: (time, word index 0-based, sentence index
/// 0-based, play 0-based).
fn candidates(take: &Take, unit: SpeechUnit, end: bool) -> Vec<(f64, usize, usize, usize)> {
    take.each_play()
        .iter()
        .flat_map(|t| {
            candidates_in_play(t, unit, end)
                .into_iter()
                .map(|(at, w, s)| (at, w, s, t.play()))
        })
        .collect()
}

fn candidates_in_play(take: &Take, unit: SpeechUnit, end: bool) -> Vec<(f64, usize, usize)> {
    let t = &take.timing;
    let sentence_of = |i: usize| {
        t.sentences
            .iter()
            .position(|s| (s.words[0] as usize..=s.words[1] as usize).contains(&i))
            .unwrap_or(0)
    };
    match unit {
        SpeechUnit::Word => t
            .words
            .iter()
            .enumerate()
            .filter_map(|(i, w)| {
                Some((
                    take.out(if end { w.end } else { w.start })?,
                    i,
                    sentence_of(i),
                ))
            })
            .collect(),
        SpeechUnit::Gap => t
            .words
            .windows(2)
            .enumerate()
            .filter_map(|(i, p)| {
                Some((take.out((p[0].end + p[1].start) / 2.0)?, i, sentence_of(i)))
            })
            .collect(),
        SpeechUnit::Sentence => t
            .sentences
            .iter()
            .enumerate()
            .filter_map(|(k, s)| {
                let at = if end { s.end } else { s.start };
                Some((take.out(at)?, s.words[0] as usize, k))
            })
            .collect(),
    }
}

/// `edge` of layer `layer_id` on the nearest word, gap between words, or
/// sentence of the take it counts on (docs/SPEECH-SYNC.md §8.2): a start on
/// a word's start, an end on a word's end. The value is written with the
/// word's text as its guard, so a rewritten line follows the word.
pub fn snap_speech(
    meta: &promo_model::ProjectMetadata,
    inputs: &BeatGrids,
    layer_id: &str,
    edge: crate::musical::Edge,
    unit: SpeechUnit,
) -> Result<SpeechSnapped, String> {
    use serde_json::json;
    let (layer, layers, clock) = scope_of(meta, layer_id)
        .ok_or_else(|| format!("no layer \"{layer_id}\" in the project"))?;
    let resources = meta.resources.as_deref().unwrap_or(&[]);
    let speech = Speech::new(clock, layers, resources, inputs);
    let start = layer.start_time;
    let end_edge = edge == crate::musical::Edge::End;
    let near = if end_edge {
        start
            + layer
                .duration
                .ok_or("the layer has no set length to snap")?
    } else {
        start
    };
    let take = speech.take_at(layer, near)?;
    let (at, word, sentence, play) = candidates(&take, unit, end_edge)
        .into_iter()
        .filter(|(t, _, _, _)| !end_edge || *t > start + 1e-4)
        .min_by(|a, b| (a.0 - near).abs().total_cmp(&(b.0 - near).abs()))
        .ok_or_else(|| {
            format!(
                "\"{}\" has nothing to snap to that the layer plays",
                take.id
            )
        })?;
    let words = &take.timing.words;
    let quoted = |i: usize| format!("word {} \u{201C}{}\u{201D}", i + 1, words[i].text);
    let (mut value, base) = match unit {
        SpeechUnit::Word => (
            json!({"word": word + 1, "of": take.id, "text": words[word].text}),
            quoted(word),
        ),
        SpeechUnit::Sentence => (
            json!({"sentence": sentence + 1, "of": take.id}),
            format!("sentence {}", sentence + 1),
        ),
        SpeechUnit::Gap => (
            json!({"snap": "gap", "near": (at * 1e6).round() / 1e6, "of": take.id}),
            format!("the gap after {}", quoted(word)),
        ),
    };
    // A gap is a point either edge may sit on; a word or sentence ends
    // where its sound does.
    if end_edge && unit != SpeechUnit::Gap {
        value["edge"] = json!("end");
    }
    // A looping take says the word more than once: name the play. A gap
    // names its own instant, so it needs none.
    let base = if take.plays() > 1 && unit != SpeechUnit::Gap {
        value["play"] = json!(play + 1);
        format!("{base} (play {})", play + 1)
    } else {
        base
    };
    let label = if end_edge {
        match unit {
            SpeechUnit::Gap => format!("ends at {base}"),
            _ => format!("ends with {base}"),
        }
    } else {
        base
    };
    Ok(SpeechSnapped {
        value,
        seconds: if end_edge { at - start } else { at },
        label,
    })
}

/// The nearest word start, gap or sentence start to output time `seconds`
/// on the take layer `layer_id` counts on there — what a timeline drag
/// snapping to speech lands on. None when there is none.
pub fn snap_speech_time(
    meta: &promo_model::ProjectMetadata,
    inputs: &BeatGrids,
    layer_id: &str,
    seconds: f64,
    unit: SpeechUnit,
) -> Option<f64> {
    let (layer, layers, clock) = scope_of(meta, layer_id)?;
    let resources = meta.resources.as_deref().unwrap_or(&[]);
    let speech = Speech::new(clock, layers, resources, inputs);
    let take = speech.take_at(layer, seconds).ok()?;
    candidates(&take, unit, false)
        .into_iter()
        .map(|(t, _, _, _)| t)
        .min_by(|a, b| (a - seconds).abs().total_cmp(&(b - seconds).abs()))
}

/// The words of the take layer `layer_id` plays, on the output timeline —
/// what a word lane under a voice-over draws. Empty for a layer that plays
/// no take; words the layer never reaches are left out.
pub fn word_ticks(
    meta: &promo_model::ProjectMetadata,
    inputs: &BeatGrids,
    layer_id: &str,
) -> Result<Vec<WordTick>, String> {
    let (layer, layers, clock) = scope_of(meta, layer_id)
        .ok_or_else(|| format!("no layer \"{layer_id}\" in the project"))?;
    let resources = meta.resources.as_deref().unwrap_or(&[]);
    let speech = Speech::new(clock, layers, resources, inputs);
    let (take, _) = speech
        .take_for(None, &layer.name, Some(layer_id))
        .map_err(Fault::why)?;
    let starts: BTreeSet<u32> = take.timing.sentences.iter().map(|s| s.words[0]).collect();
    // A looping layer says its words once per play: a tick for each.
    Ok(take
        .each_play()
        .iter()
        .flat_map(|take| {
            take.timing
                .words
                .iter()
                .enumerate()
                .filter_map(|(i, w)| {
                    Some(WordTick {
                        index: i as u32 + 1,
                        text: w.text.clone(),
                        start: take.out(w.start)?,
                        end: take.out(w.end)?,
                        sentence_start: starts.contains(&(i as u32)),
                        confidence: w.confidence.or(Some(take.timing.confidence)),
                        prominence: w.prominence,
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect())
}

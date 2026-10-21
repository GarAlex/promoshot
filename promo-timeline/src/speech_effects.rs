//! Captions on a take's words (docs/SPEECH-SYNC.md §7.1–7.2): a caption
//! reveal with `sync` gets each word's arrival from the take, written as
//! `unitTimes` and `unitEnds` (layer-local seconds, the app form only); and
//! `captions` on a layer that plays a take makes one caption layer per
//! sentence or phrase, each revealed on its words. Resolution writes the
//! numbers; the engine reads `unitTimes` and never learns a word exists.

use std::collections::BTreeSet;

use promo_model::speech::{fold_word, tokenize, RevealSync, SpeechCaptions, TakeTiming};
use promo_model::{ProjectLayer, ProjectLayerKind, ProjectMetadata, ProjectResource, TextReveal};
use serde_json::{json, Value};

use crate::musical::{BeatGrids, TimingProblem};
use crate::speech_time::{Speech, Take};

/// Each caption unit's arrival and end in layer-local seconds, and how
/// many of the units the take's words matched.
pub(crate) struct UnitTimes {
    pub times: Vec<f64>,
    pub ends: Vec<f64>,
    pub matched: usize,
    pub units: usize,
    /// 1-based, inclusive: the take's words the caption was matched to.
    pub span: Option<(u32, u32)>,
}

/// The take's word indices (0-based) a reveal may match: the pinned
/// `words` or `sentence`, else all of them.
fn window(sync: &RevealSync, timing: &TakeTiming) -> Result<(usize, usize), String> {
    let count = timing.words.len();
    if count == 0 {
        return Err("the take has no words".into());
    }
    if let Some([a, b]) = sync.words {
        if a >= 1 && a <= b && (b as usize) <= count {
            return Ok((a as usize - 1, b as usize - 1));
        }
        return Err(format!(
            "words {a}–{b} are not a span of the take ({count} words)"
        ));
    }
    if let Some(n) = sync.sentence {
        let s = (n as usize)
            .checked_sub(1)
            .and_then(|i| timing.sentences.get(i))
            .ok_or_else(|| format!("sentence {n} is past the end of the take"))?;
        return Ok((s.words[0] as usize, s.words[1] as usize));
    }
    Ok((0, count - 1))
}

/// The best local alignment of `a` (the caption's words) inside `b` (the
/// take's): pairs of indices, in order. Equal words score 2; a mismatch or
/// a skipped word costs 1 — so a caption that is one sentence of a longer
/// narration finds that sentence.
fn align(a: &[String], b: &[String]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let mut h = vec![vec![0i64; m + 1]; n + 1];
    let mut best = (0i64, 0usize, 0usize);
    for i in 1..=n {
        for j in 1..=m {
            let diag = h[i - 1][j - 1] + if a[i - 1] == b[j - 1] { 2 } else { -1 };
            let v = diag.max(h[i - 1][j] - 1).max(h[i][j - 1] - 1).max(0);
            h[i][j] = v;
            if v > best.0 {
                best = (v, i, j);
            }
        }
    }
    let (mut i, mut j) = (best.1, best.2);
    let mut pairs = Vec::new();
    while i > 0 && j > 0 && h[i][j] > 0 {
        let score = h[i][j];
        if score == h[i - 1][j - 1] + if a[i - 1] == b[j - 1] { 2 } else { -1 } {
            if a[i - 1] == b[j - 1] {
                pairs.push((i - 1, j - 1));
            }
            i -= 1;
            j -= 1;
        } else if score == h[i - 1][j] - 1 {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    pairs.reverse();
    pairs
}

/// The arrival of each whitespace-separated word of `text` (the units a
/// word reveal walks) on the take, in layer-local seconds from `start`.
/// A unit with no word of its own (punctuation) takes the next word's
/// start; one the take does not say is placed between its neighbours.
pub(crate) fn unit_times(
    text: &str,
    sync: &RevealSync,
    take: &Take,
    start: f64,
) -> Result<UnitTimes, String> {
    let timing = &take.timing;
    let (from, to) = window(sync, timing)?;
    let units: Vec<&str> = text.split_whitespace().collect();
    // Each unit's own words, folded: a hyphenated unit may be several.
    let mut sub: Vec<String> = Vec::new();
    let mut owner: Vec<usize> = Vec::new();
    for (u, unit) in units.iter().enumerate() {
        for w in tokenize(unit).words {
            sub.push(w.text.to_lowercase());
            owner.push(u);
        }
    }
    let said: Vec<String> = timing.words[from..=to]
        .iter()
        .map(|w| fold_word(&w.text))
        .collect();
    let pairs = align(&sub, &said);
    let mut first: Vec<Option<(f64, f64)>> = vec![None; units.len()];
    let mut span: Option<(u32, u32)> = None;
    for (i, j) in pairs {
        let word = &timing.words[from + j];
        let (Some(s), Some(e)) = (take.out(word.start), take.out(word.end)) else {
            continue;
        };
        let u = owner[i];
        let entry = first[u].get_or_insert((s - start, e - start));
        entry.0 = entry.0.min(s - start);
        entry.1 = entry.1.max(e - start);
        let n = (from + j) as u32 + 1;
        span = Some(match span {
            Some((a, b)) => (a.min(n), b.max(n)),
            None => (n, n),
        });
    }
    let wordless: Vec<bool> = units.iter().map(|u| tokenize(u).words.is_empty()).collect();
    let matched = first.iter().filter(|f| f.is_some()).count();
    let speaking = wordless.iter().filter(|w| !**w).count();
    if matched == 0 {
        return Err("none of its words are in the take".into());
    }
    // Fill the gaps: a punctuation unit takes the next arrival; a word the
    // take does not say sits between its neighbours, by index.
    let known: Vec<(usize, (f64, f64))> = first
        .iter()
        .enumerate()
        .filter_map(|(u, f)| f.map(|f| (u, f)))
        .collect();
    let mut times = Vec::with_capacity(units.len());
    let mut ends = Vec::with_capacity(units.len());
    for u in 0..units.len() {
        if let Some((s, e)) = first[u] {
            times.push(s);
            ends.push(e);
            continue;
        }
        let before = known.iter().rev().find(|(k, _)| *k < u);
        let after = known.iter().find(|(k, _)| *k > u);
        let (s, e) = match (before, after) {
            (_, Some((_, next))) if wordless[u] => (next.0, next.0),
            (Some((_, prev)), None) if wordless[u] => (prev.1, prev.1),
            (Some((a, prev)), Some((b, next))) => {
                let f = (u - a) as f64 / (b - a) as f64;
                let s = prev.0 + f * (next.0 - prev.0);
                (s, s.max(prev.1).min(next.0))
            }
            (None, Some((_, next))) => (next.0, next.0),
            (Some((_, prev)), None) => (prev.1, prev.1),
            (None, None) => (0.0, 0.0),
        };
        times.push(s);
        ends.push(e);
    }
    // Arrivals never go back in time.
    for u in 1..times.len() {
        if times[u] < times[u - 1] {
            times[u] = times[u - 1];
        }
        if ends[u] < times[u] {
            ends[u] = times[u];
        }
    }
    Ok(UnitTimes {
        times,
        ends,
        matched: matched.min(speaking),
        units: speaking,
        span,
    })
}

/// Rounded to the microsecond the records keep, so the app form's numbers
/// read back as themselves.
fn us(t: f64) -> f64 {
    (t * 1e6).round() / 1e6
}

/// Whether any caption of a scope reveals on a take's words.
pub(crate) fn any_sync(layers: &[ProjectLayer], resources: &[ProjectResource]) -> bool {
    let synced = |style: &Option<promo_model::SubtitleStyle>| {
        style
            .as_ref()
            .and_then(|s| s.reveal.as_ref())
            .is_some_and(|r| r.sync.is_some())
    };
    fn walk(
        layers: &[ProjectLayer],
        synced: &dyn Fn(&Option<promo_model::SubtitleStyle>) -> bool,
    ) -> bool {
        layers.iter().any(|l| {
            synced(&l.caption_style) || l.members.as_deref().is_some_and(|m| walk(m, synced))
        })
    }
    walk(layers, &synced) || resources.iter().any(|r| synced(&r.caption_style))
}

/// Fills `unitTimes`/`unitEnds` on every synced reveal of a scope's
/// caption layers — the layer's own style, or the caption resource it
/// shows (for the first layer that shows it) — and says what did not match.
#[allow(clippy::too_many_arguments)]
pub(crate) fn sync_reveals(
    layers: &mut [ProjectLayer],
    resources: &mut [ProjectResource],
    takes: &[ProjectResource],
    speech_clock: Option<&promo_model::speech::SpeechClock>,
    inputs: &BeatGrids,
    problems: &mut Vec<TimingProblem>,
    missing: &mut Vec<String>,
) {
    if !any_sync(layers, resources) {
        return;
    }
    let all: Vec<ProjectLayer> = layers.to_vec();
    let speech = Speech::new(speech_clock, &all, takes, inputs);
    let mut done_resources: BTreeSet<String> = BTreeSet::new();
    fn walk(
        layers: &mut [ProjectLayer],
        resources: &mut [ProjectResource],
        speech: &Speech,
        done: &mut BTreeSet<String>,
        problems: &mut Vec<TimingProblem>,
        missing: &mut Vec<String>,
    ) {
        for layer in layers.iter_mut() {
            if let Some(members) = layer.members.as_mut() {
                walk(members, resources, speech, done, problems, missing);
            }
            if layer.kind != ProjectLayerKind::Caption {
                continue;
            }
            let own = layer
                .caption_style
                .as_ref()
                .and_then(|s| s.reveal.as_ref())
                .and_then(|r| r.sync.clone());
            let showing = layer.resource_id.as_ref().and_then(|id| {
                resources
                    .iter()
                    .position(|r| r.id == *id && r.caption_style.is_some())
            });
            let (sync, on_resource) = match (own, showing) {
                (Some(sync), _) => (sync, None),
                (None, Some(index)) => {
                    let resource = &resources[index];
                    let Some(sync) = resource
                        .caption_style
                        .as_ref()
                        .and_then(|s| s.reveal.as_ref())
                        .and_then(|r| r.sync.clone())
                    else {
                        continue;
                    };
                    if !done.insert(resource.id.clone()) {
                        continue;
                    }
                    (sync, Some(index))
                }
                _ => continue,
            };
            let text = match on_resource {
                Some(index) => resources[index].caption_text.clone(),
                None => layer.caption_text.clone(),
            }
            .unwrap_or_default();
            let found = speech
                .take_for(layer.speech_clock.as_ref(), &layer.name, sync.of.as_deref())
                .map_err(|f| f.why());
            let answer = found.and_then(|(take, _)| {
                if take.estimated_now {
                    if !missing.contains(&take.id) {
                        missing.push(take.id.clone());
                    }
                    let estimated = TimingProblem::SpeechEstimated {
                        take: take.id.clone(),
                    };
                    if !problems.contains(&estimated) {
                        problems.push(estimated);
                    }
                }
                unit_times(&text, &sync, &take, layer.start_time)
            });
            let reveal: &mut TextReveal = match on_resource {
                Some(index) => resources[index]
                    .caption_style
                    .as_mut()
                    .and_then(|s| s.reveal.as_mut())
                    .expect("a synced reveal"),
                None => layer
                    .caption_style
                    .as_mut()
                    .and_then(|s| s.reveal.as_mut())
                    .expect("a synced reveal"),
            };
            match answer {
                Ok(found) => {
                    reveal.unit_times = Some(found.times.iter().copied().map(us).collect());
                    reveal.unit_ends = Some(found.ends.iter().copied().map(us).collect());
                    if found.matched < found.units {
                        problems.push(TimingProblem::RevealUnmatched {
                            layer: layer.name.clone(),
                            matched: found.matched,
                            units: found.units,
                            span: found.span,
                        });
                    }
                }
                Err(why) => {
                    reveal.unit_times = None;
                    reveal.unit_ends = None;
                    problems.push(TimingProblem::RevealSync {
                        layer: layer.name.clone(),
                        why,
                    });
                }
            }
        }
    }
    walk(
        layers,
        resources,
        &speech,
        &mut done_resources,
        problems,
        missing,
    );
}

/// Where a piece of a sentence may be split: after word `i` (0-based),
/// with a comma or semicolon there.
fn comma_after(timing: &TakeTiming, i: usize) -> bool {
    let chars: Vec<char> = timing.script.chars().collect();
    let mut k = timing.words[i].chars[1] as usize;
    while k < chars.len() && !chars[k].is_whitespace() {
        if matches!(chars[k], ',' | ';' | ':' | '\u{2014}' | '\u{2013}') {
            return true;
        }
        k += 1;
    }
    false
}

/// A sentence's words split into pieces of at most `max_words` words and
/// `max_seconds` seconds: at the comma nearest the middle, else at the gap
/// nearest it, recursively.
fn split(
    timing: &TakeTiming,
    a: usize,
    b: usize,
    max_words: usize,
    max_seconds: f64,
    out: &mut Vec<(usize, usize)>,
) {
    let words = &timing.words;
    let long = b + 1 - a > max_words || words[b].end - words[a].start > max_seconds;
    if !long || a == b {
        out.push((a, b));
        return;
    }
    let middle = (words[a].start + words[b].end) / 2.0;
    let near = |i: &usize| ((words[*i].end + words[*i + 1].start) / 2.0 - middle).abs();
    let cuts: Vec<usize> = (a..b).collect();
    let at = cuts
        .iter()
        .copied()
        .filter(|i| comma_after(timing, *i))
        .min_by(|x, y| near(x).total_cmp(&near(y)))
        .or_else(|| {
            cuts.iter()
                .copied()
                .min_by(|x, y| near(x).total_cmp(&near(y)))
        })
        .unwrap_or(a);
    split(timing, a, at, max_words, max_seconds, out);
    split(timing, at + 1, b, max_words, max_seconds, out);
}

/// The pieces of a take a captions generator makes, by word indices.
pub(crate) fn pieces(timing: &TakeTiming, config: &SpeechCaptions) -> Vec<(usize, usize)> {
    let max_words = config.max_words.filter(|n| *n > 0).unwrap_or(10) as usize;
    let max_seconds = config.max_seconds.filter(|s| *s > 0.0).unwrap_or(4.0);
    let phrase = config.by.as_deref() == Some("phrase");
    let mut out = Vec::new();
    for sentence in &timing.sentences {
        let (a, b) = (sentence.words[0] as usize, sentence.words[1] as usize);
        if b >= timing.words.len() || a > b {
            continue;
        }
        let mut parts = Vec::new();
        if phrase {
            let mut start = a;
            for i in a..b {
                if comma_after(timing, i) {
                    parts.push((start, i));
                    start = i + 1;
                }
            }
            parts.push((start, b));
        } else {
            parts.push((a, b));
        }
        for (x, y) in parts {
            split(timing, x, y, max_words, max_seconds, &mut out);
        }
    }
    out
}

/// The script's text of words `a..=b`, with the punctuation that closes the
/// last one.
fn slice(timing: &TakeTiming, a: usize, b: usize) -> String {
    let chars: Vec<char> = timing.script.chars().collect();
    let from = timing.words[a].chars[0] as usize;
    let mut to = timing.words[b].chars[1] as usize;
    while to < chars.len() && !chars[to].is_whitespace() {
        to += 1;
    }
    chars[from.min(chars.len())..to.min(chars.len())]
        .iter()
        .collect::<String>()
        .trim()
        .to_string()
}

/// `captions` on the project's layers (§7.2), on the final numbers: one
/// generated caption layer per piece, revealed on its words. A piece whose
/// id a person's layer holds (an edited caption) or that `skip` names (a
/// deleted one) is left alone.
pub(crate) fn expand_captions(
    meta: &mut ProjectMetadata,
    inputs: &BeatGrids,
) -> Vec<TimingProblem> {
    let mut problems = Vec::new();
    let layers = meta.layers.clone().unwrap_or_default();
    if !layers.iter().any(|l| l.captions.is_some()) {
        return problems;
    }
    let resources = meta.resources.clone().unwrap_or_default();
    let speech = Speech::new(
        meta.composition_settings.speech_clock.as_ref(),
        &layers,
        &resources,
        inputs,
    );
    let ids: BTreeSet<String> = promo_model::nesting::all_layers(meta)
        .iter()
        .map(|l| l.id.clone())
        .collect();
    let mut top = layers.iter().map(|l| l.sort_index).max().unwrap_or(0);
    let mut made: Vec<(String, u32)> = Vec::new();
    let mut new_layers: Vec<ProjectLayer> = Vec::new();
    for layer in &layers {
        let Some(config) = layer.captions.as_ref() else {
            continue;
        };
        if !matches!(
            layer.kind,
            ProjectLayerKind::Audio | ProjectLayerKind::Video
        ) {
            problems.push(TimingProblem::Generator {
                layer: layer.name.clone(),
                why: "captions go on the audio or video layer that plays the take".into(),
            });
            continue;
        }
        let take = match speech.take_for(None, &layer.name, Some(&layer.id)) {
            Ok((take, _)) => take,
            Err(f) => {
                problems.push(TimingProblem::Generator {
                    layer: layer.name.clone(),
                    why: format!("captions: {}", f.why()),
                });
                continue;
            }
        };
        let timing = &take.timing;
        let list = pieces(timing, config);
        made.push((layer.id.clone(), list.len() as u32));
        let skip = config.skip.clone().unwrap_or_default();
        let lead = config.lead.unwrap_or(0.1).max(0.0);
        let hold = config.hold.unwrap_or(0.25).max(0.0);
        let starts: Vec<Option<f64>> = list
            .iter()
            .map(|(a, _)| take.out(timing.words[*a].start).map(|s| s - lead))
            .collect();
        for (k, (a, b)) in list.iter().enumerate() {
            let id = promo_model::musical::caption_id(&layer.id, k);
            if skip.contains(&(k as u32)) || ids.contains(&id) {
                continue;
            }
            let (Some(start), Some(last)) = (starts[k], take.out(timing.words[*b].end)) else {
                continue;
            };
            let mut end = last + hold;
            if let Some(Some(next)) = starts.get(k + 1) {
                end = end.min(*next);
            }
            if end <= start {
                continue;
            }
            let mut reveal = config.reveal.clone().unwrap_or_else(|| {
                serde_json::from_value(json!({"by": "word", "mode": "highlight"})).expect("reveal")
            });
            reveal.sync = Some(RevealSync {
                of: Some(take.id.clone()),
                words: Some([*a as u32 + 1, *b as u32 + 1]),
                sentence: None,
                extra: Default::default(),
            });
            reveal.unit_times = None;
            reveal.unit_ends = None;
            let mut style = config.style.clone().unwrap_or_default();
            style.reveal = Some(reveal);
            top += 1;
            let value = json!({
                "id": id, "name": format!("{} caption {}", layer.name, k + 1),
                "sortIndex": top, "kind": "caption", "isEnabled": true,
                "startTime": us(start), "duration": us(end - start),
                "captionText": slice(timing, *a, *b),
                "captionStyle": style, "keyframes": [],
            });
            let Ok(mut caption) = serde_json::from_value::<ProjectLayer>(value) else {
                continue;
            };
            // Its words now, so the signature is of what it shows.
            let text = caption.caption_text.clone().unwrap_or_default();
            let sync = caption
                .caption_style
                .as_ref()
                .and_then(|s| s.reveal.as_ref())
                .and_then(|r| r.sync.clone())
                .expect("sync");
            if let Ok(found) = unit_times(&text, &sync, &take, caption.start_time) {
                if let Some(r) = caption
                    .caption_style
                    .as_mut()
                    .and_then(|s| s.reveal.as_mut())
                {
                    r.unit_times = Some(found.times.iter().copied().map(us).collect());
                    r.unit_ends = Some(found.ends.iter().copied().map(us).collect());
                }
            }
            let signed = serde_json::to_value(&caption).unwrap_or(Value::Null);
            caption.generated_by = Some(format!(
                "captions:{}:{}",
                layer.id,
                promo_model::musical::signature(&signed)
            ));
            new_layers.push(caption);
        }
    }
    if let Some(list) = meta.layers.as_mut() {
        for layer in list.iter_mut() {
            if let Some((_, n)) = made.iter().find(|(id, _)| *id == layer.id) {
                if let Some(c) = layer.captions.as_mut() {
                    c.made = Some(*n);
                }
            }
        }
        list.extend(new_layers);
    }
    problems
}

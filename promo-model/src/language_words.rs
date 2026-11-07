//! A narration's words in another language (core-private
//! docs/LOCALIZATION.md §3.4). Sentences are the anchor languages share;
//! words are not: index 12 is another word in German, and "Euclid" may not
//! be said at all. So when a language's take says its own words, every
//! reference to a word becomes the word in the same place in the same
//! sentence, or the same place in the whole take when the sentence counts
//! differ. The same word said in that span wins over the place, which is
//! what happens to a name. It all runs before timing, so the resolver only
//! ever sees a plain index.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::musical::{OneOrMany, SequenceEntry, TimeSpecs};
use crate::speech::{
    fold_word, is_speech_object, take_script, tokenize, During, RevealSync, Script, SpeechClock,
    SpeechPoint, SpeechTime, WordRef,
};
use crate::{ProjectLayer, ProjectLayerKind, ProjectMetadata};

/// One take's words in the source and in the language.
pub(crate) struct TakeMap {
    from: Script,
    to: Script,
}

impl TakeMap {
    /// None when either says no words: nothing to count on.
    pub(crate) fn new(source: &str, language: &str) -> Option<TakeMap> {
        let (from, to) = (tokenize(source), tokenize(language));
        (!from.words.is_empty() && !to.words.is_empty()).then_some(TakeMap { from, to })
    }

    fn same_sentences(&self) -> bool {
        self.from.sentences.len() == self.to.sentences.len()
    }

    /// The span holding source word `i`, there and here.
    fn spans(&self, i: usize) -> ([usize; 2], [usize; 2]) {
        let whole = |script: &Script| [0, script.words.len() - 1];
        let holding = self
            .from
            .sentences
            .iter()
            .position(|s| (s.words[0] as usize..=s.words[1] as usize).contains(&i));
        match holding.filter(|_| self.same_sentences()) {
            Some(n) => {
                let span =
                    |s: &crate::speech::ScriptSentence| [s.words[0] as usize, s.words[1] as usize];
                (span(&self.from.sentences[n]), span(&self.to.sentences[n]))
            }
            None => (whole(&self.from), whole(&self.to)),
        }
    }

    /// Source word `i` (0-based) here, and whether it is the same word.
    fn word(&self, i: usize) -> (usize, bool) {
        let ([a, b], [c, d]) = self.spans(i);
        let place = if b > a {
            (i - a) as f64 / (b - a) as f64
        } else {
            0.0
        };
        let position = c + (place * (d - c) as f64).round() as usize;
        let wanted = fold_word(&self.from.words[i].text);
        (c..=d)
            .filter(|k| fold_word(&self.to.words[*k].text) == wanted)
            .min_by_key(|k| (k.abs_diff(position), *k))
            .map_or((position, false), |k| (k, true))
    }

    /// The sentence (1-based) holding source sentence `n`'s first word,
    /// when the counts differ.
    fn sentence(&self, n: u32) -> Option<u32> {
        let first = self.from.sentences.get((n as usize).checked_sub(1)?)?.words[0];
        let (here, _) = self.word(first as usize);
        self.to
            .sentences
            .iter()
            .position(|s| (s.words[0] as usize..=s.words[1] as usize).contains(&here))
            .map(|k| k as u32 + 1)
    }

    fn hits(&self, text: &str) -> Vec<usize> {
        let wanted = fold_word(text);
        self.from
            .words
            .iter()
            .enumerate()
            .filter(|(_, w)| fold_word(&w.text) == wanted)
            .map(|(k, _)| k)
            .collect()
    }

    fn says_here(&self, text: &str) -> bool {
        let wanted = fold_word(text);
        self.to.words.iter().any(|w| fold_word(&w.text) == wanted)
    }

    /// Source index `n` (1-based) under a guard, by the resolver's rule: the
    /// nearest word the guard names when the index has moved off it.
    fn guarded(&self, n: u32, guard: Option<&str>) -> Option<usize> {
        let i = (n as usize).checked_sub(1)?;
        let at = self.from.words.get(i)?;
        match guard {
            Some(guard) if fold_word(&at.text) != fold_word(guard) => self
                .hits(guard)
                .into_iter()
                .min_by_key(|k| (k.abs_diff(i), *k)),
            _ => Some(i),
        }
    }

    /// Source word `i` here (1-based), with a note unless it is the same word.
    fn noted(&self, i: usize, whose: &str, notes: &mut Vec<String>) -> u32 {
        let (here, same) = self.word(i);
        if !same {
            notes.push(format!(
                "{whose}: word {} \u{201C}{}\u{201D} lands on word {} \u{201C}{}\u{201D}, by its \
                 place",
                i + 1,
                self.from.words[i].text,
                here + 1,
                self.to.words[here].text
            ));
        }
        here as u32 + 1
    }

    fn sentence_noted(&self, n: u32, whose: &str, notes: &mut Vec<String>) -> u32 {
        if self.same_sentences() {
            return n;
        }
        let Some(here) = self.sentence(n) else {
            return n;
        };
        notes.push(format!(
            "{whose}: sentence {n} lands on sentence {here}, by its place"
        ));
        here
    }

    fn span_noted(&self, [a, b]: [u32; 2], whose: &str, notes: &mut Vec<String>) -> [u32; 2] {
        let count = self.from.words.len();
        if a == 0 || a > b || b as usize > count {
            return [a, b];
        }
        let first = self.noted(a as usize - 1, whose, notes);
        let last = self.noted(b as usize - 1, whose, notes);
        [first, last.max(first)]
    }

    fn time(&self, value: &mut SpeechTime, whose: &str, notes: &mut Vec<String>) -> bool {
        let source = match &value.at {
            SpeechPoint::Word(n) => self.guarded(*n, value.guard.as_deref()),
            SpeechPoint::WordText { text, nth } => self.hits(text).get(*nth as usize - 1).copied(),
            SpeechPoint::Sentence(n) => {
                let here = self.sentence_noted(*n, whose, notes);
                let moved = here != *n;
                value.at = SpeechPoint::Sentence(here);
                return moved;
            }
            SpeechPoint::Snap { .. } => None,
        };
        let Some(i) = source else {
            return false;
        };
        value.at = SpeechPoint::Word(self.noted(i, whose, notes));
        value.guard = None;
        true
    }

    fn during(&self, during: &mut During, whose: &str, notes: &mut Vec<String>) {
        match &during.word {
            Some(WordRef::Index(n)) => {
                if let Some(i) = self.guarded(*n, None) {
                    during.word = Some(WordRef::Index(self.noted(i, whose, notes)));
                }
            }
            Some(WordRef::Text(text)) => {
                if let Some(i) = self.hits(text).first().copied() {
                    during.word = Some(WordRef::Index(self.noted(i, whose, notes)));
                }
            }
            None => {}
        }
        if let Some(span) = during.words {
            during.words = Some(self.span_noted(span, whose, notes));
        }
        if let Some(n) = during.sentence {
            during.sentence = Some(self.sentence_noted(n, whose, notes));
        }
    }

    fn sync(&self, sync: &mut RevealSync, whose: &str, notes: &mut Vec<String>) {
        if let Some(span) = sync.words {
            sync.words = Some(self.span_noted(span, whose, notes));
        }
        if let Some(n) = sync.sentence {
            sync.sentence = Some(self.sentence_noted(n, whose, notes));
        }
    }

    /// A generator's list of indices or texts. A text only the language says
    /// stays; one nobody says goes, and is named.
    fn list(&self, list: &[Value], whose: &str, notes: &mut Vec<String>) -> Vec<Value> {
        let mut out: Vec<Value> = Vec::new();
        let mut push = |item: Value| {
            if !out.contains(&item) {
                out.push(item);
            }
        };
        for item in list {
            match item {
                Value::Number(n) => match n.as_u64().and_then(|n| self.guarded(n as u32, None)) {
                    Some(i) => push(Value::from(self.noted(i, whose, notes))),
                    None => push(item.clone()),
                },
                Value::String(text) => {
                    let hits = self.hits(text);
                    if hits.is_empty() && self.says_here(text) {
                        push(item.clone());
                    } else if hits.is_empty() {
                        notes.push(format!(
                            "{whose}: nobody says \u{201C}{text}\u{201D}, so it is skipped"
                        ));
                    }
                    for i in hits {
                        push(Value::from(self.noted(i, whose, notes)));
                    }
                }
                other => push(other.clone()),
            }
        }
        out
    }
}

/// A scope's takes: what a value naming no take, or a layer, means there.
struct Scope<'a> {
    clock: Option<SpeechClock>,
    /// Each enabled layer that plays something, with what it plays.
    players: Vec<(String, String)>,
    /// The resources that say something.
    said: &'a BTreeSet<String>,
    maps: &'a BTreeMap<String, TakeMap>,
}

impl<'a> Scope<'a> {
    fn new(
        clock: Option<SpeechClock>,
        layers: &[ProjectLayer],
        said: &'a BTreeSet<String>,
        maps: &'a BTreeMap<String, TakeMap>,
    ) -> Self {
        fn walk(layers: &[ProjectLayer], out: &mut Vec<(String, String)>) {
            for layer in layers {
                if layer.is_enabled
                    && matches!(
                        layer.kind,
                        ProjectLayerKind::Audio | ProjectLayerKind::Video
                    )
                {
                    if let Some(id) = &layer.resource_id {
                        out.push((layer.id.clone(), id.clone()));
                    }
                }
                if let Some(members) = layer.members.as_deref() {
                    walk(members, out);
                }
            }
        }
        let mut players = Vec::new();
        walk(layers, &mut players);
        Scope {
            clock,
            players,
            said,
            maps,
        }
    }

    /// The take a value counts on, by the resolver's rule, when its words
    /// changed. A value that cannot find one is left for the resolver to
    /// say so.
    fn take(&self, own: Option<&SpeechClock>, of: Option<&str>) -> Option<&'a TakeMap> {
        let clock = own.or(self.clock.as_ref());
        let named = of
            .map(str::to_string)
            .or_else(|| clock.and_then(|c| c.layer.clone().or_else(|| c.resource_id.clone())))
            .or_else(|| {
                let takes: BTreeSet<&String> = self
                    .players
                    .iter()
                    .map(|(_, resource)| resource)
                    .filter(|resource| self.said.contains(*resource))
                    .collect();
                match takes.into_iter().collect::<Vec<_>>().as_slice() {
                    [one] => Some((*one).clone()),
                    _ => None,
                }
            })?;
        let resource = self
            .players
            .iter()
            .find(|(layer, _)| *layer == named)
            .map_or(&named, |(_, resource)| resource);
        self.maps.get(resource)
    }

    fn specs(
        &self,
        specs: &mut Option<TimeSpecs>,
        own: Option<&SpeechClock>,
        whose: &str,
        notes: &mut Vec<String>,
    ) {
        for (key, entry) in specs.iter_mut().flatten() {
            let Some(mut value) = entry.speech() else {
                continue;
            };
            let Some(map) = self.take(own, value.of.as_deref()) else {
                continue;
            };
            if map.time(&mut value, &format!("{whose} {key}"), notes) {
                entry.spec = Some(value.canonical_text());
                entry.label = value.label();
            }
        }
    }

    fn trigger(
        &self,
        trigger: &mut Option<Value>,
        own: Option<&SpeechClock>,
        whose: &str,
        notes: &mut Vec<String>,
    ) {
        let Some(object) = trigger.as_mut().and_then(Value::as_object_mut) else {
            return;
        };
        let of = object.get("of").and_then(Value::as_str).map(str::to_string);
        let Some(map) = self.take(own, of.as_deref()) else {
            return;
        };
        if let Some(Value::Array(list)) = object.get("words") {
            let list = map.list(list, whose, notes);
            object.insert("words".into(), Value::Array(list));
        }
    }

    fn range(
        &self,
        range: &mut Option<Value>,
        own: Option<&SpeechClock>,
        whose: &str,
        notes: &mut Vec<String>,
    ) {
        let Some(value) = range.as_mut() else {
            return;
        };
        let speech = value
            .as_object()
            .is_some_and(|o| o.contains_key("words") || o.contains_key("sentence"));
        if !speech {
            return;
        }
        let Ok(mut during) = serde_json::from_value::<During>(value.clone()) else {
            return;
        };
        let Some(map) = self.take(own, during.of.as_deref()) else {
            return;
        };
        map.during(&mut during, whose, notes);
        if let Ok(written) = serde_json::to_value(&during) {
            *value = written;
        }
    }

    fn sequence(&self, sequence: &mut Option<Vec<SequenceEntry>>, notes: &mut Vec<String>) {
        for entry in sequence.iter_mut().flatten() {
            let Some(object) = entry.until.as_ref().and_then(Value::as_object) else {
                continue;
            };
            if !is_speech_object(object) {
                continue;
            }
            let Ok(mut value) = SpeechTime::from_object(object) else {
                continue;
            };
            let Some(map) = self.take(None, value.of.as_deref()) else {
                continue;
            };
            let whose = format!("the sequence at \"{}\"", entry.layer);
            if map.time(&mut value, &whose, notes) {
                entry.until = Some(value.to_value());
            }
        }
    }

    fn flash(&self, flash: &mut Option<OneOrMany<crate::musical::Flash>>, notes: &mut Vec<String>) {
        let list: &mut [crate::musical::Flash] = match flash {
            Some(OneOrMany::Many(list)) => list,
            Some(OneOrMany::One(one)) => std::slice::from_mut(one),
            None => &mut [],
        };
        for flash in list {
            self.trigger(&mut flash.on, None, "a flash", notes);
            self.trigger(&mut flash.every, None, "a flash", notes);
            self.range(&mut flash.range, None, "a flash", notes);
        }
    }

    fn reveal(
        &self,
        reveal: Option<&mut crate::TextReveal>,
        own: Option<&SpeechClock>,
        whose: &str,
        notes: &mut Vec<String>,
    ) {
        let Some(sync) = reveal.and_then(|r| r.sync.as_mut()) else {
            return;
        };
        if let Some(map) = self.take(own, sync.of.as_deref()) {
            map.sync(sync, whose, notes);
        }
    }

    fn layers(&self, layers: &mut [ProjectLayer], notes: &mut Vec<String>) {
        for layer in layers {
            // A generator's own work is made again from these words.
            if layer.generated_by.is_some() {
                continue;
            }
            let whose = format!("\"{}\"", layer.name);
            let own = layer.speech_clock.clone();
            let own = own.as_ref();
            self.specs(&mut layer.time_specs, own, &whose, notes);
            for keyframe in &mut layer.keyframes {
                self.specs(&mut keyframe.time_specs, own, &whose, notes);
            }
            if let Some(during) = layer.during.as_mut() {
                if let Some(map) = self.take(own, during.of.as_deref()) {
                    map.during(during, &whose, notes);
                }
            }
            let pulses: &mut [crate::musical::Pulse] = match &mut layer.pulse {
                Some(OneOrMany::Many(list)) => list,
                Some(OneOrMany::One(one)) => std::slice::from_mut(one),
                None => &mut [],
            };
            for pulse in pulses {
                self.trigger(&mut pulse.on, own, &whose, notes);
                self.trigger(&mut pulse.every, own, &whose, notes);
                self.range(&mut pulse.range, own, &whose, notes);
            }
            let shakes: &mut [crate::musical::Shake] = match &mut layer.shake {
                Some(OneOrMany::Many(list)) => list,
                Some(OneOrMany::One(one)) => std::slice::from_mut(one),
                None => &mut [],
            };
            for shake in shakes {
                self.trigger(&mut shake.on, own, &whose, notes);
                self.trigger(&mut shake.every, own, &whose, notes);
                self.range(&mut shake.range, own, &whose, notes);
            }
            let style = layer.caption_style.as_mut();
            self.reveal(style.and_then(|s| s.reveal.as_mut()), own, &whose, notes);
            if let Some(members) = layer.members.as_mut() {
                self.layers(members, notes);
            }
        }
    }
}

/// What a captions generator's numbered pieces mean under other words: the
/// pieces a person deleted or edited are counted in the source's, and they
/// are not the same pieces here.
fn numbered_pieces(meta: &ProjectMetadata, scope: &Scope, notes: &mut Vec<String>) {
    let layers = meta.layers.as_deref().unwrap_or(&[]);
    for layer in layers {
        let Some(config) = layer.captions.as_ref() else {
            continue;
        };
        if scope.take(None, Some(&layer.id)).is_none() {
            continue;
        }
        let deleted = config.skip.as_ref().map_or(0, Vec::len);
        if deleted > 0 {
            notes.push(format!(
                "\"{}\": {deleted} caption(s) deleted in the source hide the same piece numbers \
                 here, which may be other words",
                layer.name
            ));
        }
        let prefix = crate::musical::caption_id(&layer.id, 0);
        let prefix = prefix.trim_end_matches('0');
        for edited in layers
            .iter()
            .filter(|l| l.id.starts_with(prefix) && l.generated_by.is_none())
        {
            notes.push(format!(
                "caption \"{}\" was edited, so it keeps its own text and times here, not the \
                 narration's",
                edited.name
            ));
        }
    }
}

/// Every reference to a word of a take in `maps`, rewritten in place. Each
/// reference that landed by position alone is a line in the answer.
pub(crate) fn follow(meta: &mut ProjectMetadata, maps: &BTreeMap<String, TakeMap>) -> Vec<String> {
    let mut notes = Vec::new();
    if maps.is_empty() {
        return notes;
    }
    let said: BTreeSet<String> = meta
        .resources
        .iter()
        .flatten()
        .filter(|r| take_script(r).is_some())
        .map(|r| r.id.clone())
        .collect();
    let scope = Scope::new(
        meta.composition_settings.speech_clock.clone(),
        meta.layers.as_deref().unwrap_or(&[]),
        &said,
        maps,
    );
    if let Some(layers) = meta.layers.as_mut() {
        scope.layers(layers, &mut notes);
    }
    numbered_pieces(meta, &scope, &mut notes);
    scope.sequence(&mut meta.composition_settings.sequence, &mut notes);
    scope.flash(&mut meta.composition_settings.flash, &mut notes);
    scope.reveal(
        meta.composition_settings.subtitle_reveal.as_mut(),
        None,
        "the project's caption reveal",
        &mut notes,
    );
    for resource in meta.resources.iter_mut().flatten() {
        let whose = format!("\"{}\"", resource.display_name);
        let style = resource.caption_style.as_mut();
        scope.reveal(
            style.and_then(|s| s.reveal.as_mut()),
            None,
            &whose,
            &mut notes,
        );
        if let Some(composition) = resource.composition.as_mut() {
            let inner = Scope::new(
                composition.speech_clock.clone(),
                &composition.layers,
                &said,
                maps,
            );
            inner.layers(&mut composition.layers, &mut notes);
            inner.sequence(&mut composition.sequence, &mut notes);
            inner.flash(&mut composition.flash, &mut notes);
        }
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    const EN: &str = "Euclid proved it long ago. Primes never end on the iPad.";
    const DE: &str =
        "Das bewies Euklid schon vor langer Zeit. Primzahlen enden nie auf dem iPad, sagte er.";

    fn map(a: &str, b: &str) -> TakeMap {
        TakeMap::new(a, b).unwrap()
    }

    #[test]
    fn a_word_keeps_its_place_in_its_sentence() {
        let map = map(EN, DE);
        // First and last words of a sentence stay first and last.
        assert_eq!(map.word(0), (0, false));
        assert_eq!(map.word(4), (6, false));
        assert_eq!(map.word(5), (7, false));
        // The middle of the second sentence is the middle of its translation.
        assert_eq!(map.word(7), (10, false));
    }

    #[test]
    fn the_same_word_wins_over_the_place() {
        let map = map(EN, DE);
        // "iPad" is said in the same sentence: exact, wherever it fell.
        assert_eq!(map.word(10), (12, true));
    }

    #[test]
    fn other_sentence_counts_use_the_whole_take() {
        let map = map(
            EN,
            "Euklid bewies es vor langer Zeit und Primzahlen enden nie.",
        );
        assert!(!map.same_sentences());
        assert_eq!(map.word(0), (0, false));
        assert_eq!(map.word(10), (9, false));
        assert_eq!(map.sentence(2), Some(1));
    }

    #[test]
    fn a_guard_finds_its_word_before_it_maps() {
        let map = map(EN, DE);
        assert_eq!(map.guarded(3, Some("Euclid")), Some(0));
        assert_eq!(map.guarded(3, Some("proved")), Some(1));
        assert_eq!(map.guarded(3, Some("nobody")), None);
        assert_eq!(map.guarded(99, None), None);
    }

    #[test]
    fn a_list_maps_every_occurrence_and_names_what_nobody_says() {
        let map = map(
            "Primes end. Primes never end.",
            "Primzahlen enden. Sie enden nie.",
        );
        let mut notes = Vec::new();
        let list = serde_json::json!(["Primes", "Euklid", 5, "nie"]);
        let out = map.list(list.as_array().unwrap(), "x", &mut notes);
        assert_eq!(
            out,
            vec![
                serde_json::json!(1),
                serde_json::json!(3),
                serde_json::json!(5),
                serde_json::json!("nie")
            ]
        );
        assert!(
            notes
                .iter()
                .any(|n| n.contains("nobody says") && n.contains("Euklid")),
            "{notes:?}"
        );
    }
}

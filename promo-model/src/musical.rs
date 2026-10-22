//! Musical time: a time field that says "4 beats" or "bar 9" instead of a
//! number of seconds (docs/BEAT-SYNC.md §1).
//!
//! The model's time fields stay plain `f64` answers — every renderer keeps
//! reading numbers. The musical value moves ASIDE at the JSON level:
//!
//! - [`lift`] runs inside `ProjectMetadata::from_json`, before serde. Where a
//!   listed time field holds an object, it moves that object into the
//!   entity's side map `timeSpecs` (a [`TimeSpec`]: the canonical spec text,
//!   the seconds it resolved to, the core's own label) and writes a
//!   placeholder `0` in its place.
//! - [`fold`] turns that "app form" back into the "file form": wherever the
//!   number still equals the entry's `seconds` (within [`EPSILON`]), the spec
//!   object goes back at the field; where the number moved, a person edited
//!   that one value and the number stands. `timeSpecs` never reaches a file.
//!
//! `ProjectMetadata::to_json` folds; `to_app_json` does not — that is the
//! form the apps, the FFI and the editor's undo snapshots carry, so an
//! edit's history keeps the resolved seconds beside each spec.
//!
//! The beat-sync fields that sit beside the time values — the clock, the
//! grid, `sequence`, `after`, the generators and resource provenance — are
//! typed here too. Slice 1 parses and keeps them; resolution acts on them
//! from slice 2 on.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::{ProjectLayer, ProjectLayerKeyframe, ProjectMetadata};

/// How close a number must stay to its spec's resolved seconds to count as
/// unedited — the apps' `PromoTiming.detachEditedLayers` epsilon.
pub const EPSILON: f64 = 1e-4;

/// The reader that understands musical values, `sequence`, `after` and the
/// beat generators: an older reader decodes `startTime` as a number and
/// cannot open a file with an object there, or would edit around a derived
/// start and contradict it.
pub const MUSICAL_READER: i64 = 49;

/// The layer fields that take a musical value, as `timeSpecs` keys.
pub const LAYER_TIME_FIELDS: &[&str] = &[
    "startTime",
    "duration",
    "fadeIn",
    "fadeOut",
    "transitionIn.duration",
    "transitionOut.duration",
];

/// The keyframe fields that take a musical value, as `timeSpecs` keys.
pub const KEYFRAME_TIME_FIELDS: &[&str] = &["time", "transitionDuration"];

// ---------------------------------------------------------------- the value

/// A grid unit a `snap` lands on. `downbeat` and `bar` mean the same point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapUnit {
    Beat,
    Downbeat,
    Bar,
}

impl SnapUnit {
    pub fn name(self) -> &'static str {
        match self {
            SnapUnit::Beat => "beat",
            SnapUnit::Downbeat => "downbeat",
            SnapUnit::Bar => "bar",
        }
    }

    pub fn parse(text: &str) -> Option<SnapUnit> {
        Some(match text {
            "beat" => SnapUnit::Beat,
            "downbeat" => SnapUnit::Downbeat,
            "bar" => SnapUnit::Bar,
            _ => return None,
        })
    }
}

/// A time on the beat grid: a length (`beats`, `bars`) or a position
/// (`beat`, `bar`, 1-based, or `snap` to the grid point nearest `near`
/// seconds).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MusicalTime {
    Beats(f64),
    Bars(f64),
    Beat(f64),
    Bar(f64),
    Snap { to: SnapUnit, near: f64 },
}

const MUSICAL_KEYS: &[&str] = &["beats", "bars", "beat", "bar", "snap", "near"];

impl MusicalTime {
    /// The strict decode: exactly one of `beats`, `bars`, `beat` or `bar`, or
    /// exactly `snap` with `near`; finite numbers, lengths at least 0; any
    /// other key refused by name. The error names no path — the caller
    /// prefixes its own.
    pub fn from_object(object: &Map<String, Value>) -> Result<MusicalTime, String> {
        if let Some(key) = object.keys().find(|k| !MUSICAL_KEYS.contains(&k.as_str())) {
            return Err(format!("unknown key \"{key}\""));
        }
        let number = |key: &str| -> Result<Option<f64>, String> {
            match object.get(key) {
                None => Ok(None),
                Some(value) => match value.as_f64() {
                    Some(n) if n.is_finite() => Ok(Some(n)),
                    _ => Err(format!("\"{key}\" must be a finite number")),
                },
            }
        };
        let units: Vec<&str> = ["beats", "bars", "beat", "bar"]
            .into_iter()
            .filter(|k| object.contains_key(*k))
            .collect();
        let shape = "needs exactly one of \"beats\", \"bars\", \"beat\" or \"bar\", \
                     or \"snap\" with \"near\"";
        if let Some(snap) = object.get("snap") {
            if !units.is_empty() {
                return Err(shape.into());
            }
            let to = snap
                .as_str()
                .and_then(SnapUnit::parse)
                .ok_or("\"snap\" must be \"beat\", \"downbeat\" or \"bar\"")?;
            let near = number("near")?.ok_or(shape)?;
            return Ok(MusicalTime::Snap { to, near });
        }
        if object.contains_key("near") || units.len() != 1 {
            return Err(shape.into());
        }
        let unit = units[0];
        let n = number(unit)?.expect("present");
        if (unit == "beats" || unit == "bars") && n < 0.0 {
            return Err(format!("\"{unit}\" must be at least 0"));
        }
        Ok(match unit {
            "beats" => MusicalTime::Beats(n),
            "bars" => MusicalTime::Bars(n),
            "beat" => MusicalTime::Beat(n),
            _ => MusicalTime::Bar(n),
        })
    }

    /// A length (`beats`, `bars`) rather than a point on the grid.
    pub fn is_length(&self) -> bool {
        matches!(self, MusicalTime::Beats(_) | MusicalTime::Bars(_))
    }

    /// The canonical object: its one key, and an integral number written
    /// as an integer, so a hand-written file is byte-stable through a save.
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        match *self {
            MusicalTime::Beats(n) => {
                map.insert("beats".into(), canonical_number(n));
            }
            MusicalTime::Bars(n) => {
                map.insert("bars".into(), canonical_number(n));
            }
            MusicalTime::Beat(n) => {
                map.insert("beat".into(), canonical_number(n));
            }
            MusicalTime::Bar(n) => {
                map.insert("bar".into(), canonical_number(n));
            }
            MusicalTime::Snap { to, near } => {
                map.insert("snap".into(), Value::from(to.name()));
                map.insert("near".into(), canonical_number(near));
            }
        }
        Value::Object(map)
    }

    /// The canonical JSON text — what `timeSpecs.spec` holds.
    pub fn canonical_text(&self) -> String {
        self.to_value().to_string()
    }

    /// The core's own wording, so a host never formats a beat count.
    pub fn label(&self) -> String {
        let plural = |n: f64, one: &str, many: &str| {
            format!("{} {}", plain(n), if n == 1.0 { one } else { many })
        };
        match *self {
            MusicalTime::Beats(n) => plural(n, "beat", "beats"),
            MusicalTime::Bars(n) => plural(n, "bar", "bars"),
            MusicalTime::Beat(n) => format!("beat {}", plain(n)),
            MusicalTime::Bar(n) => format!("bar {}", plain(n)),
            MusicalTime::Snap { to, near } => {
                format!("nearest {} to {} s", to.name(), plain(near))
            }
        }
    }
}

fn canonical_number(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9.0e15 {
        Value::from(n as i64)
    } else {
        Value::from(n)
    }
}

fn plain(n: f64) -> String {
    canonical_number(n).to_string()
}

impl Serialize for MusicalTime {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(s)
    }
}

impl<'de> Deserialize<'de> for MusicalTime {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Value::deserialize(d)? {
            Value::Object(object) => {
                MusicalTime::from_object(&object).map_err(serde::de::Error::custom)
            }
            _ => Err(serde::de::Error::custom("a musical time is an object")),
        }
    }
}

impl schemars::JsonSchema for MusicalTime {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "MusicalTime".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "properties": {
                "beats": { "type": "number", "minimum": 0 },
                "bars": { "type": "number", "minimum": 0 },
                "beat": { "type": "number" },
                "bar": { "type": "number" },
                "snap": { "type": "string", "enum": ["beat", "downbeat", "bar"] },
                "near": { "type": "number" }
            },
            "additionalProperties": false
        })
    }
}

/// A time as a field may hold it: seconds, or a musical value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TimeValue {
    Seconds(f64),
    Musical(MusicalTime),
}

impl Serialize for TimeValue {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            TimeValue::Seconds(n) => n.serialize(s),
            TimeValue::Musical(m) => m.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for TimeValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Value::deserialize(d)? {
            Value::Number(n) => match n.as_f64() {
                Some(n) if n.is_finite() => Ok(TimeValue::Seconds(n)),
                _ => Err(serde::de::Error::custom("seconds must be finite")),
            },
            Value::Object(object) => MusicalTime::from_object(&object)
                .map(TimeValue::Musical)
                .map_err(serde::de::Error::custom),
            _ => Err(serde::de::Error::custom(
                "a time is a number of seconds or a musical object",
            )),
        }
    }
}

impl schemars::JsonSchema for TimeValue {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "TimeValue".into()
    }
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let musical = generator.subschema_for::<MusicalTime>();
        schemars::json_schema!({ "oneOf": [{ "type": "number" }, musical] })
    }
}

/// What an editor writes at a time field (`SetTime`): seconds, a musical
/// value, or a speech value (docs/SPEECH-SYNC.md §6) — where the other
/// time values (`after.offset`, sequence lengths) take no speech.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldTime {
    Seconds(f64),
    Musical(MusicalTime),
    Speech(crate::speech::SpeechTime),
}

impl FieldTime {
    /// The `timeSpecs` entry for a value (none for seconds), resolving for
    /// now to `seconds`.
    pub fn spec(&self, seconds: f64) -> Option<TimeSpec> {
        match self {
            FieldTime::Seconds(_) => None,
            FieldTime::Musical(m) => Some(TimeSpec::musical(m, seconds)),
            FieldTime::Speech(v) => Some(TimeSpec::speech_value(v, seconds)),
        }
    }
}

impl schemars::JsonSchema for FieldTime {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "FieldTime".into()
    }
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let musical = generator.subschema_for::<MusicalTime>();
        let speech = generator.subschema_for::<crate::speech::SpeechTime>();
        schemars::json_schema!({ "oneOf": [{ "type": "number" }, musical, speech] })
    }
}

impl Serialize for FieldTime {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            FieldTime::Seconds(n) => n.serialize(s),
            FieldTime::Musical(m) => m.serialize(s),
            FieldTime::Speech(v) => v.to_value().serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for FieldTime {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Value::deserialize(d)? {
            Value::Number(n) => match n.as_f64() {
                Some(n) if n.is_finite() => Ok(FieldTime::Seconds(n)),
                _ => Err(serde::de::Error::custom("seconds must be finite")),
            },
            Value::Object(object) if crate::speech::is_speech_object(&object) => {
                crate::speech::SpeechTime::from_object(&object)
                    .map(FieldTime::Speech)
                    .map_err(serde::de::Error::custom)
            }
            Value::Object(object) => MusicalTime::from_object(&object)
                .map(FieldTime::Musical)
                .map_err(serde::de::Error::custom),
            _ => Err(serde::de::Error::custom(
                "a time is seconds, a musical object or a speech object",
            )),
        }
    }
}

/// The schema of a model time field: what the FILE may hold there (seconds
/// or a musical object) while the struct holds the resolved number. Used
/// through `#[schemars(with = …)]` only.
pub struct SecondsOrMusical;

impl schemars::JsonSchema for SecondsOrMusical {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "FieldTime".into()
    }
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // Seconds, a musical value, or a speech value (docs/SPEECH-SYNC.md §6).
        FieldTime::json_schema(generator)
    }
}

// ---------------------------------------------------------------- the side map

/// What a time field said before resolution put a number there: the spec
/// (canonical JSON text, opaque to the hosts), or the rule that derives it
/// (`sequence`, `after`); the seconds it resolved to; the core's label.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimeSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    pub seconds: f64,
    #[serde(default)]
    pub label: String,
}

impl TimeSpec {
    /// The entry for a musical value that resolved to `seconds`.
    pub fn musical(value: &MusicalTime, seconds: f64) -> TimeSpec {
        TimeSpec {
            spec: Some(value.canonical_text()),
            rule: None,
            seconds,
            label: value.label(),
        }
    }

    /// Whether the spec still stands at a field holding `number`: the
    /// number is the one it resolved to, so nobody edited it.
    pub fn holds(&self, number: Option<f64>) -> bool {
        self.spec.is_some() && self.stands(number)
    }

    /// Whether the field still holds the seconds this entry resolved to —
    /// for a spec or a rule alike.
    pub fn stands(&self, number: Option<f64>) -> bool {
        number.is_some_and(|n| (n - self.seconds).abs() <= EPSILON)
    }

    /// The entry for a number a rule derived (`sequence`, `after`).
    pub fn derived(rule: &str, seconds: f64) -> TimeSpec {
        TimeSpec {
            spec: None,
            rule: Some(rule.to_string()),
            seconds,
            label: match rule {
                "sequence" => "in the sequence".into(),
                "after" => "after a layer".into(),
                other => other.into(),
            },
        }
    }

    /// The entry for a speech value (docs/SPEECH-SYNC.md §6) that resolved
    /// to `seconds`.
    pub fn speech_value(value: &crate::speech::SpeechTime, seconds: f64) -> TimeSpec {
        TimeSpec {
            spec: Some(value.canonical_text()),
            rule: None,
            seconds,
            label: value.label(),
        }
    }

    /// The spec as a speech value — `None` for a musical one, a rule entry
    /// or unreadable text.
    pub fn speech(&self) -> Option<crate::speech::SpeechTime> {
        let text = self.spec.as_deref()?;
        match serde_json::from_str::<Value>(text).ok()? {
            Value::Object(object) if crate::speech::is_speech_object(&object) => {
                crate::speech::SpeechTime::from_object(&object).ok()
            }
            _ => None,
        }
    }

    /// The spec text as the object a file holds — musical or speech alike.
    pub fn spec_object(&self) -> Option<Value> {
        let value = serde_json::from_str::<Value>(self.spec.as_deref()?).ok()?;
        value.is_object().then_some(value)
    }

    /// The spec, decoded as a musical value — `None` for a rule entry, a
    /// speech value or unreadable text.
    pub fn value(&self) -> Option<MusicalTime> {
        let text = self.spec.as_deref()?;
        match serde_json::from_str::<Value>(text).ok()? {
            Value::Object(object) => MusicalTime::from_object(&object).ok(),
            _ => None,
        }
    }
}

/// An entity's side map, field → entry.
pub type TimeSpecs = BTreeMap<String, TimeSpec>;

/// The number a layer holds at a `timeSpecs` key.
pub fn layer_number(layer: &ProjectLayer, key: &str) -> Option<f64> {
    match key {
        "startTime" => Some(layer.start_time),
        "duration" => layer.duration,
        "fadeIn" => layer.fade_in,
        "fadeOut" => layer.fade_out,
        "transitionIn.duration" => layer.transition_in.as_ref().map(|t| t.duration),
        "transitionOut.duration" => layer.transition_out.as_ref().map(|t| t.duration),
        _ => None,
    }
}

/// The number a keyframe holds at a `timeSpecs` key.
pub fn keyframe_number(keyframe: &ProjectLayerKeyframe, key: &str) -> Option<f64> {
    match key {
        "time" => Some(keyframe.time),
        "transitionDuration" => Some(keyframe.transition_duration),
        _ => None,
    }
}

/// Writes `number` at a layer's `timeSpecs` key. False when the field
/// cannot hold it (no such key, or a transition the layer does not have).
pub fn set_layer_number(layer: &mut ProjectLayer, key: &str, number: f64) -> bool {
    match key {
        "startTime" => layer.start_time = number,
        "duration" => layer.duration = Some(number),
        "fadeIn" => layer.fade_in = Some(number),
        "fadeOut" => layer.fade_out = Some(number),
        "transitionIn.duration" => match layer.transition_in.as_mut() {
            Some(t) => t.duration = number,
            None => return false,
        },
        "transitionOut.duration" => match layer.transition_out.as_mut() {
            Some(t) => t.duration = number,
            None => return false,
        },
        _ => return false,
    }
    true
}

/// Writes `number` at a keyframe's `timeSpecs` key.
pub fn set_keyframe_number(keyframe: &mut ProjectLayerKeyframe, key: &str, number: f64) -> bool {
    match key {
        "time" => keyframe.time = number,
        "transitionDuration" => keyframe.transition_duration = number,
        _ => return false,
    }
    true
}

/// Forgets what `key` said: the field now holds plain seconds. What every
/// command that sets a time field calls, so a headless edit to the
/// placeholder number of an unresolved file does not fold back.
///
/// A DERIVED entry (`sequence`, `after`) stays: it is not a value to
/// forget but a rule's answer, and the next resolution compares the new
/// number against it — a moved start detaches its layer from the rule.
pub fn forget(specs: &mut Option<TimeSpecs>, key: &str) {
    if let Some(map) = specs.as_mut() {
        if map.get(key).is_some_and(|e| e.rule.is_some()) {
            return;
        }
        map.remove(key);
        if map.is_empty() {
            *specs = None;
        }
    }
}

/// Whether a fold keeps any musical value of this layer or its keyframes.
pub fn layer_keeps_musical(layer: &ProjectLayer) -> bool {
    let layer_holds = layer
        .time_specs
        .iter()
        .flatten()
        .any(|(key, entry)| entry.holds(layer_number(layer, key)));
    layer_holds
        || layer.keyframes.iter().any(|k| {
            k.time_specs
                .iter()
                .flatten()
                .any(|(key, entry)| entry.holds(keyframe_number(k, key)))
        })
}

/// Whether any entity of the project carries a side map at all — the
/// encoder's cue that the file form differs from the app form.
pub fn carries_specs(meta: &ProjectMetadata) -> bool {
    let generated = |k: &ProjectLayerKeyframe| k.auto.as_deref().is_some_and(is_generator_tag);
    crate::nesting::all_layers(meta).iter().any(|l| {
        // Stage members are in the walk, after their stage.
        l.time_specs.is_some()
            || l.generated_by.is_some()
            || l.keyframes
                .iter()
                .any(|k| k.time_specs.is_some() || generated(k))
    }) || meta
        .resources
        .iter()
        .flatten()
        .any(|r| r.generated_by.is_some())
        || carries_derived(meta)
}

/// Whether the app form holds what speech sync derives and a save drops: a
/// reveal's `unitTimes`, a captions generator's `made`.
fn carries_derived(meta: &ProjectMetadata) -> bool {
    let timed = |style: &Option<crate::SubtitleStyle>| {
        style
            .as_ref()
            .and_then(|s| s.reveal.as_ref())
            .is_some_and(|r| r.unit_times.is_some() || r.unit_ends.is_some())
    };
    crate::nesting::all_layers(meta).iter().any(|l| {
        timed(&l.caption_style)
            || l.reveal_times.is_some()
            || l.captions.as_ref().is_some_and(|c| c.made.is_some())
    }) || meta
        .resources
        .iter()
        .flatten()
        .any(|r| timed(&r.caption_style))
}

/// Drops what speech sync derives from the file form: every `unitTimes`
/// and `unitEnds`, and a captions generator's `made`.
fn strip_derived(value: &mut Value) -> bool {
    let mut changed = false;
    match value {
        Value::Object(map) => {
            changed |= map.remove("unitTimes").is_some();
            changed |= map.remove("unitEnds").is_some();
            changed |= map.remove("revealTimes").is_some();
            if let Some(Value::Object(captions)) = map.get_mut("captions") {
                changed |= captions.remove("made").is_some();
            }
            for v in map.values_mut() {
                changed |= strip_derived(v);
            }
        }
        Value::Array(items) => {
            for v in items {
                changed |= strip_derived(v);
            }
        }
        _ => {}
    }
    changed
}

// ---------------------------------------------------------------- lift and fold

#[derive(Clone, Copy, PartialEq)]
enum Entity {
    Layer,
    Keyframe,
}

/// What [`each_entity`] calls for every layer and keyframe object.
type Visit<'v> = dyn FnMut(Entity, &mut Map<String, Value>, &str) -> Result<(), String> + 'v;

/// Every layer and keyframe object in the document, with its path for
/// messages: the project's layers, stage members (recursively) and each
/// composition resource's layers.
fn each_entity(root: &mut Value, visit: &mut Visit) -> Result<(), String> {
    fn label(item: &Value, index: usize) -> String {
        item.get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| index.to_string())
    }
    fn layers(list: &mut Value, at: &str, visit: &mut Visit) -> Result<(), String> {
        let Some(items) = list.as_array_mut() else {
            return Ok(());
        };
        for (index, item) in items.iter_mut().enumerate() {
            let here = format!("{at}[{}]", label(item, index));
            let Some(layer) = item.as_object_mut() else {
                continue;
            };
            visit(Entity::Layer, layer, &here)?;
            if let Some(Value::Array(keyframes)) = layer.get_mut("keyframes") {
                for (k, keyframe) in keyframes.iter_mut().enumerate() {
                    let there = format!("{here}.keyframes[{}]", label(keyframe, k));
                    if let Some(object) = keyframe.as_object_mut() {
                        visit(Entity::Keyframe, object, &there)?;
                    }
                }
            }
            if let Some(members) = layer.get_mut("members") {
                layers(members, &format!("{here}.members"), visit)?;
            }
        }
        Ok(())
    }
    let Some(top) = root.as_object_mut() else {
        return Ok(());
    };
    if let Some(list) = top.get_mut("layers") {
        layers(list, "layers", visit)?;
    }
    if let Some(Value::Array(resources)) = top.get_mut("resources") {
        for (index, resource) in resources.iter_mut().enumerate() {
            let here = format!("resources[{}]", label(resource, index));
            if let Some(list) = resource
                .get_mut("composition")
                .and_then(|c| c.get_mut("layers"))
            {
                layers(list, &format!("{here}.composition.layers"), visit)?;
            }
        }
    }
    Ok(())
}

/// The object holding a `timeSpecs` key's field, and the field's own name:
/// `transitionIn.duration` lives in the `transitionIn` object.
fn slot<'a>(
    entity: &'a mut Map<String, Value>,
    key: &'a str,
) -> Option<(&'a mut Map<String, Value>, &'a str)> {
    match key.split_once('.') {
        Some((parent, leaf)) => entity
            .get_mut(parent)
            .and_then(Value::as_object_mut)
            .map(|object| (object, leaf)),
        None => Some((entity, key)),
    }
}

fn fields_of(entity: Entity) -> &'static [&'static str] {
    match entity {
        Entity::Layer => LAYER_TIME_FIELDS,
        Entity::Keyframe => KEYFRAME_TIME_FIELDS,
    }
}

/// Moves every musical value in `root` aside into its entity's `timeSpecs`
/// (seconds 0, the placeholder `0` at the field) — the app form. Strict: a
/// malformed value is an error naming its path, for example
/// `layers[s1].duration: unknown key "beets"`. True when anything moved.
pub fn lift(root: &mut Value) -> Result<bool, String> {
    let mut lifted = false;
    each_entity(root, &mut |entity, object, at| {
        let mut found: Vec<(&str, TimeSpec)> = Vec::new();
        for key in fields_of(entity) {
            let Some((holder, leaf)) = slot(object, key) else {
                continue;
            };
            if let Some(Value::Object(spec)) = holder.get(leaf) {
                let entry = if crate::speech::is_speech_object(spec) {
                    let value = crate::speech::SpeechTime::from_object(spec)
                        .map_err(|e| format!("{at}.{key}: {e}"))?;
                    TimeSpec::speech_value(&value, 0.0)
                } else {
                    let value =
                        MusicalTime::from_object(spec).map_err(|e| format!("{at}.{key}: {e}"))?;
                    TimeSpec::musical(&value, 0.0)
                };
                holder.insert(leaf.to_string(), Value::from(0));
                found.push((key, entry));
            }
        }
        if found.is_empty() {
            return Ok(());
        }
        lifted = true;
        let specs = object
            .entry("timeSpecs")
            .or_insert_with(|| Value::Object(Map::new()));
        if !specs.is_object() {
            *specs = Value::Object(Map::new());
        }
        let specs = specs.as_object_mut().expect("an object");
        for (key, entry) in found {
            let entry = serde_json::to_value(entry).expect("encodes");
            specs.insert(key.to_string(), entry);
        }
        Ok(())
    })?;
    Ok(lifted)
}

/// Lifts one layer object (and its keyframes) — for an edit that patches a
/// layer with a musical value.
pub fn lift_layer(layer: &mut Value) -> Result<bool, String> {
    let mut root = serde_json::json!({ "layers": [Value::Null] });
    root["layers"][0] = std::mem::take(layer);
    let lifted = lift(&mut root);
    *layer = std::mem::take(&mut root["layers"][0]);
    lifted
}

/// The app form back into the file form: every spec whose field still holds
/// its resolved seconds goes back at the field; an edited value keeps its
/// number, and only that value is flattened. Then `timeSpecs` goes. True
/// when anything changed.
pub fn fold(root: &mut Value) -> bool {
    let mut changed = false;
    // Sequenced layers someone moved or resized, by scope: the sequence
    // lives on the scope, beside the layers.
    let mut detached: Vec<(Option<String>, Detach)> = Vec::new();
    let _ = each_entity(root, &mut |entity, object, at| {
        let Some(specs) = object.remove("timeSpecs") else {
            return Ok(());
        };
        changed = true;
        let Ok(specs) = serde_json::from_value::<TimeSpecs>(specs) else {
            return Ok(());
        };
        if entity == Entity::Layer {
            let scope = scope_of(at);
            let id = object
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let number = |object: &Map<String, Value>, key: &str| {
                slot_ref(object, key).and_then(Value::as_f64)
            };
            let start_moved = specs
                .get("startTime")
                .filter(|e| e.rule.is_some())
                .map(|e| {
                    (
                        e.rule.clone().unwrap_or_default(),
                        !e.stands(number(object, "startTime")),
                    )
                });
            if let Some((rule, true)) = &start_moved {
                // A hand-placed start: the rule that placed it goes.
                if rule == "after" {
                    object.remove("after");
                } else if rule == "sequence" {
                    detached.push((scope.clone(), Detach::Leave(id.clone())));
                } else if rule == "during" {
                    object.remove("during");
                }
            }
            // `during` sets both edges: a hand move of either leaves it,
            // and the numbers stand (docs/SPEECH-SYNC.md §6.2).
            if specs
                .get("duration")
                .filter(|e| e.rule.as_deref() == Some("during"))
                .is_some_and(|e| !e.stands(number(object, "duration")))
            {
                object.remove("during");
            }
            let start_stood = !matches!(start_moved, Some((_, true)));
            if let Some(entry) = specs
                .get("duration")
                .filter(|e| e.rule.as_deref() == Some("sequence"))
            {
                let now = number(object, "duration");
                if start_stood && !entry.stands(now) {
                    if let Some(seconds) = now {
                        detached.push((scope, Detach::Seconds(id, seconds)));
                    }
                }
            }
        }
        for (key, entry) in &specs {
            let Some(value) = entry.spec_object() else {
                continue;
            };
            let Some((holder, leaf)) = slot(object, key) else {
                continue;
            };
            let number = holder.get(leaf).and_then(Value::as_f64);
            if entry.holds(number) {
                holder.insert(leaf.to_string(), value);
            }
        }
        Ok(())
    });
    apply_detaches(root, detached);
    let stripped = strip_generated(root);
    let derived = strip_derived(root);
    changed || stripped || derived
}

/// The tag on a keyframe a beat generator wrote: `auto` = `beat:<signature>`.
pub const BEAT_TAG: &str = "beat:";

/// The tag on a keyframe a generator firing on a take's words wrote
/// (docs/SPEECH-SYNC.md §7.3): `auto` = `speech:<signature>`.
pub const SPEECH_TAG: &str = "speech:";

/// Whether an `auto` tag is a beat or speech generator's.
pub fn is_generator_tag(auto: &str) -> bool {
    auto.starts_with(BEAT_TAG) || auto.starts_with(SPEECH_TAG)
}

/// A generated entity's signature: 16 hex digits of a 64-bit FNV-1a over
/// its JSON as written — numbers quantized to 1e-5, so a value that went
/// through another encoder and back hashes the same — leaving out what is
/// identity or bookkeeping rather than a value a person edits (`id`,
/// `auto`, `timeSpecs`, `generatedBy`, a layer's `sortIndex`).
pub fn signature(value: &Value) -> String {
    fn canonical(value: &Value, top: bool, out: &mut String) {
        match value {
            Value::Object(map) => {
                out.push('{');
                for (key, v) in map {
                    if top
                        && ["id", "auto", "timeSpecs", "generatedBy", "sortIndex"]
                            .contains(&key.as_str())
                    {
                        continue;
                    }
                    // Derived on every resolve and dropped by a save: never
                    // a person's edit.
                    if key == "unitTimes" || key == "unitEnds" || key == "revealTimes" {
                        continue;
                    }
                    out.push_str(key);
                    out.push(':');
                    canonical(v, false, out);
                    out.push(',');
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for v in items {
                    canonical(v, false, out);
                    out.push(',');
                }
                out.push(']');
            }
            Value::Number(n) => {
                let q = (n.as_f64().unwrap_or(0.0) * 1e5).round() as i64;
                out.push_str(&q.to_string());
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut text = String::new();
    canonical(value, true, &mut text);
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Whether a keyframe (as JSON) is a beat generator's, as it wrote it.
pub fn is_unedited_beat_keyframe(keyframe: &Value) -> Option<bool> {
    let auto = keyframe.get("auto")?.as_str()?;
    let tag = auto
        .strip_prefix(BEAT_TAG)
        .or_else(|| auto.strip_prefix(SPEECH_TAG))?;
    Some(tag == signature(keyframe))
}

/// Whether a layer or resource (as JSON) is a beat generator's, as it wrote
/// it: `generatedBy` = `<generator>:<n>:<signature>`.
pub fn is_unedited_generated(entity: &Value) -> Option<bool> {
    let tag = entity.get("generatedBy")?.as_str()?;
    let sig = tag.rsplit(':').next()?;
    Some(sig == signature(entity))
}

/// The flash generator a `generatedBy` names: its index.
pub fn flash_index(generated_by: &str) -> Option<usize> {
    let mut parts = generated_by.split(':');
    (parts.next()? == "flash").then_some(())?;
    parts.next()?.parse().ok()
}

/// Beat-generated content out of the file form: an unedited generated
/// keyframe, layer or resource goes (it regenerates on the next load); an
/// edited one stays as the person's, its tag gone — and an edited or
/// deleted flash layer takes its generator with it, so the edit sticks.
pub fn strip_generated(root: &mut Value) -> bool {
    let mut changed = false;
    // Keyframes, wherever layers live.
    let _ = each_entity(root, &mut |entity, object, _| {
        if entity != Entity::Layer {
            return Ok(());
        }
        if let Some(Value::Array(keyframes)) = object.get_mut("keyframes") {
            let before = keyframes.len();
            keyframes.retain(|k| is_unedited_beat_keyframe(k) != Some(true));
            changed |= keyframes.len() != before;
            for keyframe in keyframes.iter_mut() {
                if is_unedited_beat_keyframe(keyframe) == Some(false) {
                    keyframe.as_object_mut().map(|o| o.remove("auto"));
                    changed = true;
                }
            }
        }
        Ok(())
    });
    // Generated layers and resources, per scope.
    let mut retired: Vec<(Option<String>, usize)> = Vec::new();
    let mut scope_lists: Vec<(Option<String>, &mut Vec<Value>)> = Vec::new();
    let Some(top) = root.as_object_mut() else {
        return changed;
    };
    let mut resource_flash: Vec<(Option<String>, usize, bool)> = Vec::new();
    if let Some(Value::Array(resources)) = top.get_mut("resources") {
        for resource in resources.iter_mut() {
            if let Some(n) = resource
                .get("generatedBy")
                .and_then(Value::as_str)
                .and_then(flash_index)
            {
                resource_flash.push((None, n, is_unedited_generated(resource) == Some(true)));
            }
        }
    }
    if let Some(Value::Array(layers)) = top.get_mut("layers") {
        retire_deleted_captions(layers);
        scope_lists.push((None, layers));
    }
    for (scope, layers) in scope_lists {
        let mut seen = Vec::new();
        layers.retain_mut(|layer| {
            let Some(tag) = layer.get("generatedBy").and_then(Value::as_str) else {
                return true;
            };
            changed = true;
            let n = flash_index(tag);
            if let Some(n) = n {
                seen.push(n);
            }
            if is_unedited_generated(layer) == Some(true) {
                return false;
            }
            // A person's now: the tag goes, and so does its generator.
            if let Some(n) = n {
                retired.push((scope.clone(), n));
            }
            layer.as_object_mut().map(|o| o.remove("generatedBy"));
            true
        });
        // A generated resource whose layer is gone: the layer was deleted.
        for (_, n, _) in resource_flash.iter().filter(|(s, _, _)| *s == scope) {
            if !seen.contains(n) {
                retired.push((scope.clone(), *n));
            }
        }
    }
    if let Some(Value::Array(resources)) = top.get_mut("resources") {
        resources.retain_mut(|resource| {
            let Some(tag) = resource.get("generatedBy").and_then(Value::as_str) else {
                return true;
            };
            changed = true;
            if is_unedited_generated(resource) == Some(true) {
                return false;
            }
            let _ = tag;
            resource.as_object_mut().map(|o| o.remove("generatedBy"));
            true
        });
    }
    if retired.is_empty() {
        return changed;
    }
    let mut indices: Vec<usize> = retired.into_iter().map(|(_, n)| n).collect();
    indices.sort_unstable();
    indices.dedup();
    if let Some(settings) = top
        .get_mut("compositionSettings")
        .and_then(Value::as_object_mut)
    {
        match settings.get_mut("flash") {
            Some(Value::Array(list)) => {
                for n in indices.iter().rev() {
                    if *n < list.len() {
                        list.remove(*n);
                    }
                }
                if list.is_empty() {
                    settings.remove("flash");
                }
            }
            Some(_) if indices.contains(&0) => {
                settings.remove("flash");
            }
            _ => {}
        }
    }
    true
}

/// A captions generator's pieces a person deleted (docs/SPEECH-SYNC.md
/// §7.2): of the `made` pieces, each whose layer is no longer in the scope
/// goes into `skip`, so the delete sticks.
fn retire_deleted_captions(layers: &mut [Value]) {
    let ids: std::collections::HashSet<String> = layers
        .iter()
        .filter_map(|l| l.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    for layer in layers.iter_mut() {
        let Some(id) = layer.get("id").and_then(Value::as_str).map(str::to_string) else {
            continue;
        };
        let Some(captions) = layer.get_mut("captions").and_then(Value::as_object_mut) else {
            continue;
        };
        let Some(made) = captions.get("made").and_then(Value::as_u64) else {
            continue;
        };
        let mut skip: Vec<u64> = captions
            .get("skip")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default();
        for k in 0..made {
            if !skip.contains(&k) && !ids.contains(&caption_id(&id, k as usize)) {
                skip.push(k);
            }
        }
        skip.sort_unstable();
        if !skip.is_empty() {
            captions.insert("skip".into(), serde_json::json!(skip));
        }
    }
}

/// The id of piece `k` a captions generator on layer `layer` makes.
pub fn caption_id(layer: &str, k: usize) -> String {
    format!("speech-captions-{layer}-{k}")
}

/// Replaces `into`'s beat-generated content with `from`'s (the same
/// document, resolved): generated keyframes per layer by id path, and
/// generated layers and resources — leaving every other key of `into` as
/// it is.
pub fn copy_generated(from: &Value, into: &mut Value) {
    let mut generated: std::collections::HashMap<String, Vec<Value>> = Default::default();
    let mut source = from.clone();
    let _ = each_entity(&mut source, &mut |entity, object, at| {
        if entity == Entity::Layer {
            let keys: Vec<Value> = object
                .get("keyframes")
                .and_then(Value::as_array)
                .map(|ks| {
                    ks.iter()
                        .filter(|k| is_unedited_beat_keyframe(k).is_some())
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            generated.insert(at.to_string(), keys);
        }
        Ok(())
    });
    let _ = each_entity(into, &mut |entity, object, at| {
        if entity != Entity::Layer {
            return Ok(());
        }
        let Some(keys) = generated.get(at) else {
            return Ok(());
        };
        if let Some(Value::Array(keyframes)) = object.get_mut("keyframes") {
            keyframes.retain(|k| is_unedited_beat_keyframe(k).is_none());
            keyframes.extend(keys.iter().cloned());
        }
        Ok(())
    });
    for list in ["layers", "resources"] {
        let made: Vec<Value> = from
            .get(list)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter(|v| v.get("generatedBy").is_some())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if let Some(Value::Array(items)) = into.get_mut(list) {
            items.retain(|v| v.get("generatedBy").is_none());
            items.extend(made);
        } else if !made.is_empty() {
            into[list] = Value::Array(made);
        }
    }
}

/// What a hand edit does to a sequence.
enum Detach {
    /// The member's start moved: it leaves the sequence.
    Leave(String),
    /// Only its length moved: its entry becomes that many seconds.
    Seconds(String, f64),
}

/// The scope a layer path lives in: `None` for the project's layers, the
/// composition resource's id for its layers.
fn scope_of(at: &str) -> Option<String> {
    let rest = at.strip_prefix("resources[")?;
    Some(rest[..rest.find(']')?].to_string())
}

fn slot_ref<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    match key.split_once('.') {
        Some((parent, leaf)) => object.get(parent)?.get(leaf),
        None => object.get(key),
    }
}

/// Applies the sequence detaches fold found.
fn apply_detaches(root: &mut Value, detached: Vec<(Option<String>, Detach)>) {
    for (scope, detach) in detached {
        let list = match &scope {
            None => root
                .get_mut("compositionSettings")
                .and_then(|s| s.get_mut("sequence")),
            Some(id) => root
                .get_mut("resources")
                .and_then(Value::as_array_mut)
                .and_then(|r| {
                    r.iter_mut()
                        .find(|r| r.get("id").and_then(Value::as_str) == Some(id.as_str()))
                })
                .and_then(|r| r.get_mut("composition"))
                .and_then(|c| c.get_mut("sequence")),
        };
        let Some(Value::Array(entries)) = list else {
            continue;
        };
        let named = |e: &Value, id: &str| e.get("layer").and_then(Value::as_str) == Some(id);
        // A `per` entry's member moved or resized by hand leaves it.
        let member = match &detach {
            Detach::Leave(id) | Detach::Seconds(id, _) => id.clone(),
        };
        for entry in entries.iter_mut() {
            if let Some(Value::Array(layers)) = entry.get_mut("layers") {
                layers.retain(|l| l.as_str() != Some(member.as_str()));
            }
        }
        match detach {
            Detach::Leave(id) => entries.retain(|e| !named(e, &id)),
            Detach::Seconds(id, seconds) => {
                for entry in entries.iter_mut().filter(|e| named(e, &id)) {
                    *entry = serde_json::json!({ "layer": id, "seconds": seconds });
                }
            }
        }
    }
}

/// Copies what speech sync derives (docs/SPEECH-SYNC.md §7) from `from` —
/// the same document, resolved — into `into`: each caption reveal's
/// `unitTimes` and `unitEnds` (layers by id path, caption resources by
/// id), a layer's `revealTimes` when its synced reveal is shared, and a
/// captions generator's `made`. What a host that keeps the
/// file's own text beside the resolved numbers needs so its renderer walks
/// the words.
pub fn copy_derived(from: &Value, into: &mut Value) {
    fn take(object: &Map<String, Value>) -> Map<String, Value> {
        let mut out = Map::new();
        if let Some(reveal) = object
            .get("captionStyle")
            .and_then(|s| s.get("reveal"))
            .and_then(Value::as_object)
        {
            for key in ["unitTimes", "unitEnds"] {
                if let Some(v) = reveal.get(key) {
                    out.insert(key.to_string(), v.clone());
                }
            }
        }
        if let Some(made) = object.get("captions").and_then(|c| c.get("made")) {
            out.insert("made".into(), made.clone());
        }
        // A shared synced reveal's word times sit on the layer itself.
        if let Some(times) = object.get("revealTimes") {
            out.insert("revealTimes".into(), times.clone());
        }
        out
    }
    fn put(object: &mut Map<String, Value>, found: &Map<String, Value>) {
        for (key, value) in found {
            if key == "made" {
                if let Some(Value::Object(c)) = object.get_mut("captions") {
                    c.insert(key.clone(), value.clone());
                }
            } else if key == "revealTimes" {
                object.insert(key.clone(), value.clone());
            } else if let Some(Value::Object(reveal)) = object
                .get_mut("captionStyle")
                .and_then(|s| s.get_mut("reveal"))
            {
                reveal.insert(key.clone(), value.clone());
            }
        }
    }
    let mut found: std::collections::HashMap<String, Map<String, Value>> = Default::default();
    let mut source = from.clone();
    let _ = each_entity(&mut source, &mut |entity, object, at| {
        if entity == Entity::Layer {
            let derived = take(object);
            if !derived.is_empty() {
                found.insert(at.to_string(), derived);
            }
        }
        Ok(())
    });
    let _ = each_entity(into, &mut |entity, object, at| {
        if entity == Entity::Layer {
            if let Some(derived) = found.get(at) {
                put(object, derived);
            }
        }
        Ok(())
    });
    let resources: std::collections::HashMap<String, Map<String, Value>> = from
        .get("resources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let object = r.as_object()?;
            let derived = take(object);
            let id = object.get("id")?.as_str()?.to_string();
            (!derived.is_empty()).then_some((id, derived))
        })
        .collect();
    if let Some(Value::Array(list)) = into.get_mut("resources") {
        for r in list.iter_mut() {
            let Some(object) = r.as_object_mut() else {
                continue;
            };
            let Some(id) = object.get("id").and_then(Value::as_str).map(str::to_string) else {
                continue;
            };
            if let Some(derived) = resources.get(&id) {
                put(object, derived);
            }
        }
    }
}

/// Copies every time field and side map of `from` — the same document,
/// resolved — into `into`, entity by entity (layers and keyframes by id
/// path), leaving every other key of `into` as it is. What lets a host hand
/// back the file with resolved numbers and nothing else touched: keys
/// below the bags, which a trip through the model would not keep, stay.
pub fn copy_times(from: &Value, into: &mut Value) {
    let mut found: std::collections::HashMap<String, Map<String, Value>> = Default::default();
    let mut source = from.clone();
    let _ = each_entity(&mut source, &mut |entity, object, at| {
        let mut fields = Map::new();
        for key in fields_of(entity).iter().chain(["timeSpecs"].iter()) {
            let value = match slot(object, key) {
                Some((holder, leaf)) => holder.get(leaf).cloned().unwrap_or(Value::Null),
                None => continue,
            };
            fields.insert((*key).to_string(), value);
        }
        found.insert(at.to_string(), fields);
        Ok(())
    });
    let _ = each_entity(into, &mut |_, object, at| {
        let Some(fields) = found.get(at) else {
            return Ok(());
        };
        for (key, value) in fields {
            let Some((holder, leaf)) = slot(object, key) else {
                continue;
            };
            if value.is_null() {
                holder.remove(leaf);
            } else {
                holder.insert(leaf.to_string(), value.clone());
            }
        }
        Ok(())
    });
}

// ---------------------------------------------------------------- the new fields

/// Which audio's grid drives musical values, and where on the output
/// timeline its source second 0 sits. On `compositionSettings`, on a
/// composition (its own clock), or on a layer as an override.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BeatClock {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "resourceID"
    )]
    pub resource_id: Option<String>,
    /// Seconds only. Absent: the clock follows the first enabled audio
    /// layer that plays the resource.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<f64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A beat grid stated by hand on an audio resource: no detection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BeatGrid {
    pub bpm: f64,
    /// The first downbeat, in source seconds. Absent means 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_signature: Option<[u32; 2]>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The beat tracker's version (`promo_media::beats`): bumped whenever the
/// analysis of the same samples could change. A cached analysis from an
/// older one is measured again.
pub const TRACKER_VERSION: u32 = 3;

/// A beat tracker's answer for one audio file (docs/BEAT-SYNC.md §3.3): the
/// cache file's shape, the CLI's `promo beats --json` and the MCP
/// `promo_beats` answer. Every time is in the audio's own seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BeatAnalysis {
    pub tracker_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<BeatSource>,
    /// The tempo hint it was measured with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bpm_hint: Option<f64>,
    pub duration: f64,
    pub bpm: f64,
    /// The first downbeat.
    pub offset: f64,
    pub time_signature: [u32; 2],
    /// `uniform` when the beats fit one steady tempo exactly (the beats are
    /// then that arithmetic grid), else `tracked`.
    pub grid: String,
    pub confidence: f64,
    pub beats: Vec<f64>,
    pub downbeats: Vec<f64>,
    pub sections: Vec<BeatSection>,
}

/// The file an analysis is of: its name, the first 32 hex digits of the
/// SHA-256 of its bytes (the cache key), its size.
pub type BeatSource = crate::media_cache::MediaSource;

/// A stretch of bars with one character.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BeatSection {
    pub index: u32,
    /// `intro`, `outro`, `drop`, or `section` — best effort.
    pub label: String,
    pub start: f64,
    pub end: f64,
    /// 1-based.
    pub start_bar: u32,
    pub bars: u32,
    /// 0 (the quietest section) to 1 (the loudest).
    pub energy: f64,
}

impl BeatAnalysis {
    /// The analysis as a stated grid — what resolution counts beats on.
    pub fn grid(&self) -> BeatGrid {
        BeatGrid {
            bpm: self.bpm,
            offset: Some(self.offset),
            time_signature: Some(self.time_signature),
            extra: Map::new(),
        }
    }
}

/// Where a resource came from — the tool, model, licence, prompt and cost
/// — for credits and AI labels. Every field optional.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One member of a `sequence`: the layer, and its length — `beats`, `bars`,
/// `seconds` or a time value; none keeps the layer's own duration. Or a
/// `per` entry (docs/SPEECH-SYNC.md §7.3): `layers`, one per sentence of
/// the take `of`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SequenceEntry {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub layer: String,
    /// The layers of a `per` entry, in order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layers: Option<Vec<String>>,
    /// The entry ends at a position — musical (`{"bar": 9}`) or speech
    /// (`{"sentence": 3, "of": "intro"}`) — and the next starts there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<Value>,
    /// `sentence`: one of `layers` per sentence of the take `of`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub of: Option<String>,
    /// Seconds each cut comes before its sentence (default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead: Option<f64>,
    /// Seconds the last slide holds past the last sentence (default 0.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<f64>,
    /// `beat`, `downbeat` or `bar`: each cut onto the nearest one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantize: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beats: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bars: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<TimeValue>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A layer that starts when another ends: its id, or the id with an offset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum After {
    Layer(String),
    Spec(AfterSpec),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AfterSpec {
    pub layer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<TimeValue>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A generator field's value: one object, or a list of them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum OneOrMany<T> {
    Many(Vec<T>),
    One(T),
}

/// A punch on every grid point: viewport, placement or opacity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Pulse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub property: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decay: Option<TimeValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A shake on every grid point.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Shake {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amplitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decay: Option<TimeValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A full-canvas flash on every grid point (on the composition).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Flash {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_hex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<TimeValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

// ---------------------------------------------------------------- the rung

/// Whether the project needs [`MUSICAL_READER`]: a musical value a save
/// keeps, a `sequence` or `after`, or a beat generator.
pub fn needs_musical_reader(meta: &ProjectMetadata) -> bool {
    let layers = crate::nesting::all_layers(meta);
    let settings = &meta.composition_settings;
    layers.iter().any(|l| {
        layer_keeps_musical(l) || l.after.is_some() || l.pulse.is_some() || l.shake.is_some()
    }) || settings.sequence.is_some()
        || settings.flash.is_some()
        || meta.resources.iter().flatten().any(|r| {
            r.composition
                .as_ref()
                .is_some_and(|c| c.sequence.is_some() || c.flash.is_some())
        })
}

/// Whether the project carries beat fields an older lossless reader may
/// simply ignore: a clock, a stated grid, provenance (rung 48).
pub fn carries_ignorable_beat_fields(meta: &ProjectMetadata) -> bool {
    meta.composition_settings.beat_clock.is_some()
        || crate::nesting::all_layers(meta)
            .iter()
            .any(|l| l.beat_clock.is_some())
        || meta.resources.iter().flatten().any(|r| {
            r.beat_grid.is_some()
                || r.bpm_hint.is_some()
                || r.provenance.is_some()
                || r.composition
                    .as_ref()
                    .is_some_and(|c| c.beat_clock.is_some())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(value: Value) -> Result<MusicalTime, String> {
        MusicalTime::from_object(value.as_object().unwrap())
    }

    /// The app keeps the file's own text and takes only what sync derives:
    /// a shared reveal's times on the layer come across, in a composition
    /// too, and the file form never had them.
    #[test]
    fn a_shared_reveals_times_reach_the_file_form() {
        let times = json!({"times": [0.5, 1.0], "ends": [0.9, 1.4]});
        let resolved = json!({
            "layers": [{"id": "cap", "revealTimes": times}],
            "resources": [{"id": "comp", "composition": {"layers": [
                {"id": "inner", "revealTimes": times}
            ]}}]
        });
        let mut file = json!({
            "layers": [{"id": "cap", "x": 1}],
            "resources": [{"id": "comp", "composition": {"layers": [{"id": "inner"}]}}]
        });
        copy_derived(&resolved, &mut file);
        assert_eq!(file["layers"][0]["revealTimes"], times);
        assert_eq!(file["layers"][0]["x"], 1);
        assert_eq!(
            file["resources"][0]["composition"]["layers"][0]["revealTimes"],
            times
        );
    }

    #[test]
    fn the_decode_is_strict_and_the_encode_canonical() {
        assert_eq!(parse(json!({"beats": 4})), Ok(MusicalTime::Beats(4.0)));
        assert_eq!(parse(json!({"bar": 5.0})), Ok(MusicalTime::Bar(5.0)));
        assert_eq!(
            parse(json!({"snap": "downbeat", "near": 12.3})),
            Ok(MusicalTime::Snap {
                to: SnapUnit::Downbeat,
                near: 12.3
            })
        );
        for bad in [
            json!({}),
            json!({"beats": 1, "bars": 1}),
            json!({"beats": -1}),
            json!({"snap": "beat"}),
            json!({"snap": "measure", "near": 1}),
            json!({"beat": 3, "near": 1}),
            json!({"beets": 2}),
            json!({"beats": "two"}),
        ] {
            assert!(parse(bad.clone()).is_err(), "{bad} decoded");
        }
        assert_eq!(MusicalTime::Beats(4.0).canonical_text(), r#"{"beats":4}"#);
        assert_eq!(MusicalTime::Beats(0.5).canonical_text(), r#"{"beats":0.5}"#);
        assert_eq!(MusicalTime::Beats(1.0).label(), "1 beat");
        assert_eq!(MusicalTime::Bars(2.0).label(), "2 bars");
        assert_eq!(MusicalTime::Beat(17.0).label(), "beat 17");
    }
}

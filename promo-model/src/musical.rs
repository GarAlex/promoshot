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

    fn parse(text: &str) -> Option<SnapUnit> {
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

/// The schema of a model time field: what the FILE may hold there (seconds
/// or a musical object) while the struct holds the resolved number. Used
/// through `#[schemars(with = …)]` only.
pub struct SecondsOrMusical;

impl schemars::JsonSchema for SecondsOrMusical {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "TimeValue".into()
    }
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        TimeValue::json_schema(generator)
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
        self.spec.is_some() && number.is_some_and(|n| (n - self.seconds).abs() <= EPSILON)
    }

    /// The spec, decoded — `None` for a rule entry or unreadable text.
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
pub fn forget(specs: &mut Option<TimeSpecs>, key: &str) {
    if let Some(map) = specs.as_mut() {
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
    crate::nesting::all_layers(meta).iter().any(|l| {
        // Stage members are in the walk, after their stage.
        l.time_specs.is_some() || l.keyframes.iter().any(|k| k.time_specs.is_some())
    })
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
        let mut found: Vec<(&str, MusicalTime)> = Vec::new();
        for key in fields_of(entity) {
            let Some((holder, leaf)) = slot(object, key) else {
                continue;
            };
            if let Some(Value::Object(spec)) = holder.get(leaf) {
                let value =
                    MusicalTime::from_object(spec).map_err(|e| format!("{at}.{key}: {e}"))?;
                holder.insert(leaf.to_string(), Value::from(0));
                found.push((key, value));
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
        for (key, value) in found {
            let entry = serde_json::to_value(TimeSpec::musical(&value, 0.0)).expect("encodes");
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
    let _ = each_entity(root, &mut |_, object, _| {
        let Some(specs) = object.remove("timeSpecs") else {
            return Ok(());
        };
        changed = true;
        let Ok(specs) = serde_json::from_value::<TimeSpecs>(specs) else {
            return Ok(());
        };
        for (key, entry) in &specs {
            let Some(value) = entry.value() else {
                continue;
            };
            let Some((holder, leaf)) = slot(object, key) else {
                continue;
            };
            let number = holder.get(leaf).and_then(Value::as_f64);
            if entry.holds(number) {
                holder.insert(leaf.to_string(), value.to_value());
            }
        }
        Ok(())
    });
    changed
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
/// `seconds` or a time value; none keeps the layer's own duration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SequenceEntry {
    pub layer: String,
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

//! The core's wizard held to the app's (review 2026-09-27, P3-46, F10).
//!
//! The wizard existed twice: the Mac and iOS app's `setupStarterLayers`,
//! which people use, and `promo_editor::author`, which the headless servers
//! and the CLI run for agents — with no test comparing them. The app cannot
//! be run from here, so its answers are fixtures: the app's
//! `WizardParityTests.testPrintsTheFixturesForTheCore` prints, per case,
//! the spec this wizard is asked for and the app's project in comparable
//! form (`tests/wizard/<case>.json`). This test authors each spec and holds
//! the result to it in the same form: the core's own encoding, every id
//! replaced by a label for what it names, the clock's stamps dropped.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use promo_model::ProjectMetadata;
use serde_json::{Map, Value};

/// One project in comparable form — the app test's `shape`, rule for rule:
/// layers labelled by position, resources in the order the layers first
/// use them (then the rest in file order), `addedAt`/`createdAt`/
/// `updatedAt` and keyframe ids dropped, the project's id and name dropped.
fn shape(json: &str) -> Value {
    let meta = ProjectMetadata::from_json(json).expect("parses");
    let mut doc: Value = serde_json::from_str(&meta.to_json().unwrap()).unwrap();
    let mut layers: Vec<Value> = doc["layers"].as_array().cloned().unwrap_or_default();
    layers.sort_by_key(|l| l["sortIndex"].as_i64().unwrap_or(0));
    let mut labels: HashMap<String, String> = HashMap::new();
    for (index, layer) in layers.iter().enumerate() {
        if let Some(id) = layer["id"].as_str() {
            labels.insert(id.to_string(), format!("layer{index}"));
        }
    }
    let resources: Vec<Value> = doc["resources"].as_array().cloned().unwrap_or_default();
    let by_id: HashMap<String, Value> = resources
        .iter()
        .filter_map(|r| r["id"].as_str().map(|id| (id.to_string(), r.clone())))
        .collect();
    let mut next = 0;
    fn walk_uses(
        node: &Value,
        by_id: &HashMap<String, Value>,
        labels: &mut HashMap<String, String>,
        next: &mut usize,
    ) {
        match node {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                for key in keys {
                    let value = &map[key];
                    if key == "resourceID" || key.ends_with("ResourceID") {
                        if let Some(id) = value.as_str() {
                            if !labels.contains_key(id) {
                                labels.insert(id.to_string(), format!("res{next}"));
                                *next += 1;
                            }
                            if let Some(resource) = by_id.get(id) {
                                walk_uses(resource, by_id, labels, next);
                            }
                        }
                    } else if key != "id" {
                        walk_uses(value, by_id, labels, next);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk_uses(item, by_id, labels, next);
                }
            }
            _ => {}
        }
    }
    for layer in &layers {
        walk_uses(layer, &by_id, &mut labels, &mut next);
    }
    for resource in &resources {
        if let Some(id) = resource["id"].as_str() {
            if !labels.contains_key(id) {
                labels.insert(id.to_string(), format!("res{next}"));
                next += 1;
            }
        }
    }
    fn relabel(node: &Value, labels: &HashMap<String, String>) -> Value {
        match node {
            Value::Object(map) => {
                let mut out = Map::new();
                for (key, value) in map {
                    if ["addedAt", "createdAt", "updatedAt"].contains(&key.as_str()) {
                        continue;
                    }
                    if key == "id" && map.contains_key("time") {
                        continue;
                    }
                    // A layer inside a composition: its id names nothing
                    // the show points at, and the app's is random.
                    if key == "id"
                        && map.contains_key("startTime")
                        && value.as_str().is_some_and(|id| !labels.contains_key(id))
                    {
                        continue;
                    }
                    out.insert(key.clone(), relabel(value, labels));
                }
                Value::Object(out)
            }
            Value::Array(items) => Value::Array(items.iter().map(|i| relabel(i, labels)).collect()),
            Value::String(s) => labels
                .get(s)
                .map(|l| Value::String(l.clone()))
                .unwrap_or_else(|| node.clone()),
            Value::Number(n) => number(n.as_f64().unwrap_or(0.0)),
            other => other.clone(),
        }
    }
    let object = doc.as_object_mut().unwrap();
    object.remove("id");
    object.remove("name");
    object.insert("layers".into(), Value::Array(layers));
    let mut shaped = relabel(&doc, &labels);
    let mut resources: Vec<Value> = shaped["resources"].as_array().cloned().unwrap_or_default();
    resources.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    shaped["resources"] = Value::Array(resources);
    shaped
}

/// Every number as a float, rounded to a micro-unit: the app's printer
/// writes 72.0 as 72 and 0.7 as 0.69999999999999996.
fn number(value: f64) -> Value {
    let rounded = (value * 1e6).round() / 1e6;
    serde_json::Number::from_f64(rounded)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

/// Numbers as floats, and a key holding `[]` or `null` as absent — the
/// format reads the three alike, and the two writers spell "none"
/// differently.
fn numbers(node: &Value) -> Value {
    match node {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(_, v)| !v.is_null() && v.as_array().is_none_or(|a| !a.is_empty()))
                .map(|(k, v)| (k.clone(), numbers(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(numbers).collect()),
        Value::Number(n) => number(n.as_f64().unwrap_or(0.0)),
        other => other.clone(),
    }
}

/// What a wizard decides, and only that: the save's stamps and the host's
/// state are dropped, then numbers and empties evened out.
fn comparable(doc: &Value) -> Value {
    let mut doc = doc.clone();
    if let Some(object) = doc.as_object_mut() {
        for stamp in ["minReaderVersion", "formatVersion", "state"] {
            object.remove(stamp);
        }
    }
    numbers(&doc)
}

/// The first place two documents differ, as a path.
fn first_difference(a: &Value, b: &Value, at: String) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let keys: std::collections::BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            for key in keys {
                let path = format!("{at}.{key}");
                match (x.get(key), y.get(key)) {
                    (Some(p), Some(q)) => {
                        if let Some(d) = first_difference(p, q, path) {
                            return Some(d);
                        }
                    }
                    (p, q) => {
                        return Some(format!(
                            "{path}: app {} / core {}",
                            p.map(|v| v.to_string()).unwrap_or("absent".into()),
                            q.map(|v| v.to_string()).unwrap_or("absent".into())
                        ))
                    }
                }
            }
            None
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                return Some(format!("{at}: app has {} items, core {}", x.len(), y.len()));
            }
            x.iter()
                .zip(y)
                .enumerate()
                .find_map(|(i, (p, q))| first_difference(p, q, format!("{at}[{i}]")))
        }
        (p, q) if p == q => None,
        (p, q) => Some(format!("{at}: app {p} / core {q}")),
    }
}

#[test]
fn the_core_wizard_answers_as_the_apps_does() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/wizard");
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .expect("tests/wizard")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    entries.sort();
    assert!(!entries.is_empty(), "no fixtures in {}", dir.display());
    let mut failures: BTreeMap<String, String> = BTreeMap::new();
    for path in &entries {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let fixture: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let spec: promo_editor::author::AuthorSpec =
            serde_json::from_value(fixture["spec"].clone()).expect("spec");
        let authored = match promo_editor::author::author(&spec) {
            Ok(json) => json,
            Err(e) => {
                failures.insert(name, format!("the core refused it: {e}"));
                continue;
            }
        };
        let core = comparable(&shape(&authored));
        let app = comparable(&fixture["expected"]);
        // WIZARD_DUMP=<dir> writes both sides for a whole-document diff.
        if let Some(dump) = std::env::var_os("WIZARD_DUMP") {
            let dump = Path::new(&dump);
            std::fs::create_dir_all(dump).unwrap();
            for (side, doc) in [("app", &app), ("core", &core)] {
                std::fs::write(
                    dump.join(format!("{name}.{side}.json")),
                    serde_json::to_string_pretty(doc).unwrap(),
                )
                .unwrap();
            }
        }
        if let Some(difference) = first_difference(&app, &core, String::new()) {
            failures.insert(name, difference);
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} shows differ from the app's:\n{}",
        failures.len(),
        entries.len(),
        failures
            .iter()
            .map(|(name, why)| format!("  {name}: {why}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

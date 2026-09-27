//! One answer to "will this project render as written?", for every door.
//!
//! `promo validate`, the headless MCP server (which runs it) and the app's
//! `promo_validate` (through the FFI) each assembled their own list, and
//! all of them led with "ok" — beside a layer whose file was missing, a
//! layer-level `opacity` nothing reads, a placement 0.42 px tall. An agent
//! reads the first word and moves on (review 2026-09-27, P1-17–19: an
//! agent's stress test through the CLI hit all three and reported four
//! "engine bugs"). So every finding carries a severity:
//!
//! - **breaks**: something the file says will not show — a field nothing
//!   reads, a value that does nothing where it is, media that is not
//!   there. The headline is `NOT OK`.
//! - **warning**: the project renders, but the renderer adjusts something
//!   the file says (a clamp, a window slid back inside its source), or the
//!   file could say it better (a legacy form, an undeclared reader
//!   version).
//!
//! The headline wording lives here and only here; the app hands its own
//! findings in ([`Report::merge_foreign`]) rather than composing a second
//! headline.

use crate::validate;
use promo_model::{PathStep, ProjectLayer, ProjectMetadata, UnreadKey};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Something written will not show, or does nothing where it is.
    Breaks,
    /// It renders; the renderer adjusts it, or it could be said better.
    Warning,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Breaks => "breaks",
            Severity::Warning => "warning",
        }
    }

    pub fn parse(raw: &str) -> Option<Severity> {
        match raw {
            "breaks" => Some(Severity::Breaks),
            "warning" => Some(Severity::Warning),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub severity: Severity,
    pub message: String,
    /// For a key nothing reads: where it sits, as a shape with array
    /// indices collapsed and the key last — `layers[].keyframes[].opacty`.
    /// A caller with a list of its own (the app's round trip) matches on
    /// this rather than on prose.
    pub shape: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    pub findings: Vec<Finding>,
}

impl Report {
    pub fn warn(&mut self, message: impl Into<String>) {
        self.findings.push(Finding {
            severity: Severity::Warning,
            message: message.into(),
            shape: None,
        });
    }

    pub fn breaks(&mut self, message: impl Into<String>) {
        self.findings.push(Finding {
            severity: Severity::Breaks,
            message: message.into(),
            shape: None,
        });
    }

    /// Every message, breaks and warnings alike, in the order found.
    pub fn messages(&self) -> Vec<String> {
        self.findings.iter().map(|f| f.message.clone()).collect()
    }

    fn of(&self, severity: Severity) -> Vec<&str> {
        self.findings
            .iter()
            .filter(|f| f.severity == severity)
            .map(|f| f.message.as_str())
            .collect()
    }

    /// Nothing will fail to show: no finding is a break.
    pub fn renders_as_written(&self) -> bool {
        !self.findings.iter().any(|f| f.severity == Severity::Breaks)
    }

    /// Nothing to report at all.
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    /// Findings another reader made (the app's own decode and round trip),
    /// folded in so ONE headline covers both lists. A finding whose `shape`
    /// names a key this report already named is left out: the core's line
    /// says the same thing, with a suggestion.
    pub fn merge_foreign(&mut self, foreign: Vec<Finding>) {
        let named: BTreeSet<String> = self
            .findings
            .iter()
            .filter_map(|f| f.shape.clone())
            .collect();
        for finding in foreign {
            if finding.shape.as_ref().is_some_and(|s| named.contains(s)) {
                continue;
            }
            self.findings.push(finding);
        }
    }

    /// The report as an author reads it. The first word is the verdict:
    /// `ok` only when everything written will show.
    pub fn text(&self) -> String {
        let breaks = self.of(Severity::Breaks);
        let warnings = self.of(Severity::Warning);
        if breaks.is_empty() && warnings.is_empty() {
            return "ok — nothing the renderer would quietly correct".into();
        }
        let mut out = String::new();
        if breaks.is_empty() {
            out.push_str(&format!(
                "ok — the project renders, with {} warning(s):",
                warnings.len()
            ));
        } else {
            out.push_str(&format!(
                "NOT OK — {} thing(s) in this project will not render or have no effect:",
                breaks.len()
            ));
            for line in &breaks {
                out.push_str(&format!("\n  - {line}"));
            }
            if !warnings.is_empty() {
                out.push_str(&format!("\n\nand {} warning(s):", warnings.len()));
            }
        }
        for line in &warnings {
            out.push_str(&format!("\n  - {line}"));
        }
        out
    }

    /// `ok` is "nothing to report", `renders` is "no breaks"; `text` is the
    /// same words [`Report::text`] prints, for a caller that shows them.
    pub fn json(&self) -> Value {
        json!({
            "ok": self.is_clean(),
            "renders": self.renders_as_written(),
            "breaks": self.of(Severity::Breaks),
            "warnings": self.of(Severity::Warning),
            "findings": self.findings.iter().map(|f| {
                let mut entry = json!({"severity": f.severity.as_str(), "message": f.message});
                if let Some(shape) = &f.shape {
                    entry["shape"] = Value::String(shape.clone());
                }
                entry
            }).collect::<Vec<_>>(),
            "text": self.text(),
        })
    }
}

/// What a caller can show besides the decoded document.
#[derive(Debug, Clone, Copy, Default)]
pub struct Context<'a> {
    /// The raw `metadata.json`: keys the model does not read are only
    /// visible here (the decode drops most of them).
    pub json: Option<&'a str>,
    /// The names in `Resources/` and `Images/`, when the caller can see the
    /// folder: a layer whose file is not there renders nothing.
    pub listing: Option<&'a [String]>,
    /// Captions measured against the frame (issue #9). Text layout, so the
    /// editor's per-redraw banner leaves it off; every validate turns it on.
    pub layout: bool,
}

/// The whole report for `meta`: the document's own findings, then what the
/// context makes visible.
pub fn report(meta: &ProjectMetadata, context: &Context) -> Report {
    let mut out = validate::findings(meta);
    if let Some(json) = context.json {
        unread_keys(meta, json, &mut out);
    }
    if let Some(listing) = context.listing {
        missing_media(meta, listing, &mut out);
    }
    if context.layout {
        for line in crate::layout_check::layout_warnings(meta) {
            out.warn(line);
        }
    }
    out
}

/// The report for a file as written — what `validate` answers, through the
/// CLI and through the FFI alike: the file decoded, its attachments
/// resolved (so the checks see the numbers a render uses), what that
/// resolution and the nesting walk found, then [`report`] with the raw
/// text, the folder listing when there is one, and the captions measured.
pub fn file_report(json: &str, listing: Option<&[String]>) -> Result<Report, String> {
    let mut meta = ProjectMetadata::from_json(json).map_err(|e| e.to_string())?;
    let mut out = Report::default();
    for problem in crate::resolve_attachments(&mut meta) {
        out.warn(problem.to_string());
    }
    for problem in promo_model::nesting::problems(&meta) {
        // A composition inside itself, or nested past the cap, cannot be
        // drawn at all; the rest (a nested layer naming nothing) are the
        // layer's own problem, which the media checks name as a break.
        if problem.contains("contains itself") || problem.contains("nests deeper than") {
            out.breaks(problem);
        } else {
            out.warn(problem);
        }
    }
    let rest = report(
        &meta,
        &Context {
            json: Some(json),
            listing,
            layout: true,
        },
    );
    out.findings.extend(rest.findings);
    Ok(out)
}

/// The object a key sits in, as far as the bags go: whose bag keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holder {
    Project,
    TopLayer,
    Layer,
    Resource,
    Other,
}

fn holder(path: &[PathStep]) -> Holder {
    use PathStep::{Index, Key};
    match path {
        [] => Holder::Project,
        [Key(k), Index(_)] if k == "layers" => Holder::TopLayer,
        [Key(k), Index(_)] if k == "resources" => Holder::Resource,
        [.., Key(k), Index(_)] if k == "layers" || k == "members" => Holder::Layer,
        _ => Holder::Other,
    }
}

/// Keys a bag keeps that someone DOES read: the apps (a narration's
/// `speech`, a library reference, entry and exit transitions, the audio
/// focus), the author tools (`handles`), an editor (`$schema`). Derived
/// from the Swift model's coding keys against this one on 2026-09-27; the
/// app's suite encodes its own projects through this report and fails if
/// a key it writes is called unread.
fn read_by_someone(holder: Holder, key: &str) -> bool {
    match holder {
        Holder::Project => matches!(key, "$schema" | "handles"),
        Holder::TopLayer | Holder::Layer => {
            matches!(key, "entryTransition" | "exitTransition" | "isAudioFocused")
        }
        Holder::Resource => matches!(
            key,
            "libraryID" | "motionKeyframes" | "origin" | "speech" | "themePlateID"
        ),
        Holder::Other => false,
    }
}

fn unread_keys(meta: &ProjectMetadata, json: &str, out: &mut Report) {
    let Ok(keys) = ProjectMetadata::unread_keys(json) else {
        return;
    };
    let schema = SchemaIndex::get();
    for unread in keys {
        let holder = holder(&unread.path);
        if read_by_someone(holder, &unread.key) {
            continue;
        }
        // Named already, in words written for exactly this mistake.
        let named = match holder {
            Holder::TopLayer => validate::LAYER_WRONG_LEVEL.contains(&unread.key.as_str()),
            Holder::Resource => validate::RESOURCE_WRONG_LEVEL.contains(&unread.key.as_str()),
            _ => false,
        };
        if named {
            continue;
        }
        let (hint, shape) = schema.hint(&unread);
        out.findings.push(Finding {
            severity: Severity::Breaks,
            message: format!(
                "{}: \"{}\" is not read here, so it has no effect{hint}",
                describe(meta, &unread.path),
                unread.key
            ),
            shape: Some(shape),
        });
    }
}

/// A path in the words an author uses: the layer's name, the keyframe's
/// time, the resource's name — then whatever is left, dotted.
fn describe(meta: &ProjectMetadata, path: &[PathStep]) -> String {
    use PathStep::{Index, Key};
    let layers = meta.layers.as_deref().unwrap_or(&[]);
    let resources = meta.resources.as_deref().unwrap_or(&[]);
    let mut named = String::new();
    let mut rest: &[PathStep] = path;
    let mut layer: Option<&ProjectLayer> = None;
    match path {
        [Key(k), Index(i), tail @ ..] if k == "layers" => {
            if let Some(found) = layers.get(*i) {
                named = format!("layer \"{}\"", found.name);
                layer = Some(found);
                rest = tail;
            }
        }
        [Key(k), Index(i), tail @ ..] if k == "resources" => {
            if let Some(resource) = resources.get(*i) {
                named = format!("{} \"{}\"", resource.kind.as_str(), resource.display_name);
                rest = tail;
                if let [Key(c), Key(l), Index(j), inner @ ..] = tail {
                    let nested = resource.composition.as_ref().and_then(|c| c.layers.get(*j));
                    if let (true, Some(found)) = (c == "composition" && l == "layers", nested) {
                        named = format!(
                            "layer \"{}\" in composition \"{}\"",
                            found.name, resource.display_name
                        );
                        layer = Some(found);
                        rest = inner;
                    }
                }
            }
        }
        _ => {}
    }
    while let Some(current) = layer {
        match rest {
            [Key(k), Index(j), tail @ ..] if k == "members" => {
                let Some(member) = current.members.as_ref().and_then(|m| m.get(*j)) else {
                    break;
                };
                named = format!("member \"{}\" of stage \"{}\"", member.name, current.name);
                layer = Some(member);
                rest = tail;
            }
            [Key(k), Index(j), tail @ ..] if k == "keyframes" => {
                if let Some(keyframe) = current.keyframes.get(*j) {
                    named = format!("{named} keyframe at {}s", keyframe.time);
                    rest = tail;
                }
                break;
            }
            _ => break,
        }
    }
    let tail = UnreadKey {
        path: rest.to_vec(),
        key: String::new(),
    }
    .location();
    match (named.is_empty(), tail.is_empty()) {
        (true, true) => "the project".into(),
        (true, false) => tail,
        (false, true) => named,
        (false, false) => format!("{named}, {tail}"),
    }
}

/// The wire schema, walked once: what each object reads, and every place
/// a name is read — so a key nothing reads can be answered with the one
/// that was meant, or with where that key IS read.
struct SchemaIndex {
    root: Value,
    /// Property name → every shape it is read at (`layers[].keyframes[].opacity`).
    read_at: HashMap<String, Vec<String>>,
}

impl SchemaIndex {
    fn get() -> &'static SchemaIndex {
        static INDEX: OnceLock<SchemaIndex> = OnceLock::new();
        INDEX.get_or_init(|| {
            let root = promo_model::wire_schema();
            let mut read_at = HashMap::new();
            let mut seen = Vec::new();
            index(&root, &root, "", 0, &mut seen, &mut read_at);
            for shapes in read_at.values_mut() {
                shapes.sort_by_key(|s| (s.len(), s.clone()));
                shapes.dedup();
            }
            SchemaIndex { root, read_at }
        })
    }

    /// The branches a node stands for: references followed, unions opened.
    fn branches<'a>(&'a self, node: &'a Value, out: &mut Vec<&'a Value>, depth: usize) {
        if depth > 8 {
            return;
        }
        if let Some(name) = node.get("$ref").and_then(Value::as_str) {
            if let Some(def) = self
                .root
                .get("$defs")
                .and_then(|d| d.get(name.trim_start_matches("#/$defs/")))
            {
                self.branches(def, out, depth + 1);
            }
            return;
        }
        let mut union = false;
        for key in ["anyOf", "oneOf", "allOf"] {
            if let Some(options) = node.get(key).and_then(Value::as_array) {
                union = true;
                for option in options {
                    self.branches(option, out, depth + 1);
                }
            }
        }
        if !union || node.get("properties").is_some() {
            out.push(node);
        }
    }

    fn step<'a>(&'a self, node: &'a Value, step: &PathStep) -> Option<(&'a Value, String)> {
        let mut options = Vec::new();
        self.branches(node, &mut options, 0);
        match step {
            PathStep::Index(_) => options
                .iter()
                .find_map(|n| n.get("items"))
                .map(|n| (n, "[]".to_string())),
            PathStep::Key(key) => options
                .iter()
                .find_map(|n| n.get("properties").and_then(|p| p.get(key)))
                .map(|n| (n, key.clone()))
                .or_else(|| {
                    options
                        .iter()
                        .find_map(|n| n.get("additionalProperties").filter(|v| v.is_object()))
                        .map(|n| (n, "<name>".to_string()))
                }),
        }
    }

    /// The node a path leads to and its shape, when the schema knows it.
    fn locate(&self, path: &[PathStep]) -> (Option<&Value>, String) {
        let mut node = &self.root;
        let mut shape = String::new();
        for (i, step) in path.iter().enumerate() {
            match self.step(node, step) {
                Some((next, part)) => {
                    node = next;
                    if part != "[]" && !shape.is_empty() {
                        shape.push('.');
                    }
                    shape.push_str(&part);
                }
                None => {
                    // Off the schema's map (a free-form value): the shape
                    // carries the rest as written.
                    let rest = UnreadKey {
                        path: path[i..].to_vec(),
                        key: String::new(),
                    }
                    .location();
                    let collapsed: String = collapse_indices(&rest);
                    if !shape.is_empty() && !collapsed.starts_with('[') {
                        shape.push('.');
                    }
                    shape.push_str(&collapsed);
                    return (None, shape);
                }
            }
        }
        (Some(node), shape)
    }

    fn properties(&self, node: &Value) -> BTreeSet<String> {
        let mut options = Vec::new();
        self.branches(node, &mut options, 0);
        options
            .iter()
            .filter_map(|n| n.get("properties").and_then(Value::as_object))
            .flat_map(|p| p.keys().cloned())
            .collect()
    }

    /// ` — did you mean "fontSize"?`, ` — it is read at keyframes[].opacity`,
    /// or nothing; plus the finding's shape.
    fn hint(&self, unread: &UnreadKey) -> (String, String) {
        let (node, holder_shape) = self.locate(&unread.path);
        let shape = if holder_shape.is_empty() {
            unread.key.clone()
        } else {
            format!("{holder_shape}.{}", unread.key)
        };
        let valid = node.map(|n| self.properties(n)).unwrap_or_default();
        // Where the same word IS read under this object. A stage member or
        // a nested composition's layer is a layer like any other, but the
        // index walks each type once, under `layers[]` — so a layer holder
        // also answers relative to that.
        let mut prefixes = Vec::new();
        if !holder_shape.is_empty() {
            prefixes.push(format!("{holder_shape}."));
        }
        if holder_shape.ends_with("members[]")
            || (holder_shape.ends_with("layers[]") && holder_shape != "layers[]")
        {
            prefixes.push("layers[].".to_string());
        }
        let shapes = self.read_at.get(&unread.key).cloned().unwrap_or_default();
        let mut relative: Vec<String> = Vec::new();
        for s in &shapes {
            if let Some(tail) = prefixes.iter().find_map(|p| s.strip_prefix(p.as_str())) {
                if !relative.iter().any(|r| r == tail) {
                    relative.push(tail.to_string());
                }
            }
        }
        relative.truncate(2);
        let absolute: Vec<String> = shapes.iter().take(2).cloned().collect();
        // A keyframe field written on its layer — `opacity`, `zoom`,
        // `camera` one level too high — is the classic misplacement, and
        // saying where it IS read beats any near-spelling.
        if relative.iter().any(|r| r.starts_with("keyframes[].")) {
            return (
                format!(" — it is read at {}", relative.join(" and ")),
                shape,
            );
        }
        if let Some((meant, settings_name)) = suggestion(&valid, &unread.key) {
            // A caption style spelling a composition default: say where
            // the name written IS read, since it is a real name.
            let also = match settings_name {
                true => absolute
                    .iter()
                    .find(|s| s.starts_with("compositionSettings."))
                    .map(|s| format!(" (\"{}\" is the composition's default, {s})", unread.key))
                    .unwrap_or_default(),
                false => String::new(),
            };
            return (format!(" — did you mean \"{meant}\"?{also}"), shape);
        }
        if !relative.is_empty() {
            return (
                format!(" — it is read at {}", relative.join(" and ")),
                shape,
            );
        }
        if !absolute.is_empty() {
            return (
                format!(" — it is read at {}", absolute.join(" and ")),
                shape,
            );
        }
        (String::new(), shape)
    }
}

fn collapse_indices(text: &str) -> String {
    let mut out = String::new();
    let mut in_index = false;
    for c in text.chars() {
        match c {
            '[' => {
                in_index = true;
                out.push_str("[]");
            }
            ']' => in_index = false,
            _ if in_index => {}
            _ => out.push(c),
        }
    }
    out
}

fn index(
    root: &Value,
    node: &Value,
    shape: &str,
    depth: usize,
    seen: &mut Vec<String>,
    read_at: &mut HashMap<String, Vec<String>>,
) {
    if depth > 24 {
        return;
    }
    if let Some(reference) = node.get("$ref").and_then(Value::as_str) {
        let name = reference.trim_start_matches("#/$defs/").to_string();
        if seen.contains(&name) {
            return;
        }
        if let Some(def) = root.get("$defs").and_then(|d| d.get(&name)) {
            seen.push(name);
            index(root, def, shape, depth + 1, seen, read_at);
            seen.pop();
        }
        return;
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(options) = node.get(key).and_then(Value::as_array) {
            for option in options {
                index(root, option, shape, depth + 1, seen, read_at);
            }
        }
    }
    if let Some(properties) = node.get("properties").and_then(Value::as_object) {
        for (name, child) in properties {
            let here = if shape.is_empty() {
                name.clone()
            } else {
                format!("{shape}.{name}")
            };
            read_at.entry(name.clone()).or_default().push(here.clone());
            index(root, child, &here, depth + 1, seen, read_at);
        }
    }
    if let Some(items) = node.get("items") {
        index(root, items, &format!("{shape}[]"), depth + 1, seen, read_at);
    }
    if let Some(values) = node.get("additionalProperties").filter(|v| v.is_object()) {
        index(
            root,
            values,
            &format!("{shape}.<name>"),
            depth + 1,
            seen,
            read_at,
        );
    }
}

/// The name that was meant, from the names this object reads — and
/// whether it was found by reading the key as a composition default's
/// `subtitle…` spelling.
fn suggestion(valid: &BTreeSet<String>, key: &str) -> Option<(String, bool)> {
    if let Some(same) = valid
        .iter()
        .find(|v| v.eq_ignore_ascii_case(key) && *v != key)
    {
        return Some((same.clone(), false));
    }
    let upper_first = |s: &str| {
        let mut chars = s.chars();
        chars
            .next()
            .map(|c| c.to_ascii_uppercase().to_string() + chars.as_str())
            .unwrap_or_default()
    };
    let lower_first = |s: &str| {
        let mut chars = s.chars();
        chars
            .next()
            .map(|c| c.to_ascii_lowercase().to_string() + chars.as_str())
            .unwrap_or_default()
    };
    // The composition's caption defaults are spelt `subtitle…`; a caption's
    // own style spells the same things without the prefix, and a style
    // carrying `subtitleFontSize` rendered at the default size in three
    // shipped demos.
    let mut spellings = vec![key.to_string()];
    if let Some(rest) = key.strip_prefix("subtitle").filter(|r| !r.is_empty()) {
        spellings.push(lower_first(rest));
    }
    for spelling in &spellings {
        let settings_name = spelling != key;
        if settings_name && valid.contains(spelling) {
            return Some((spelling.clone(), true));
        }
        let flag = format!("is{}", upper_first(spelling));
        if valid.contains(&flag) {
            return Some((flag, settings_name));
        }
        if spelling.len() >= 4 {
            let tail = upper_first(spelling);
            if let Some(longer) = valid
                .iter()
                .filter(|v| v.ends_with(&tail) && v.len() > tail.len())
                .min_by_key(|v| (v.len(), (*v).clone()))
            {
                return Some((longer.clone(), settings_name));
            }
        }
    }
    let limit = if key.len() >= 8 { 2 } else { 1 };
    valid
        .iter()
        .map(|v| (edit_distance(&v.to_lowercase(), &key.to_lowercase()), v))
        .filter(|(d, _)| *d <= limit)
        .min()
        .map(|(_, v)| (v.clone(), false))
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            current.push(
                (previous[j] + cost)
                    .min(previous[j + 1] + 1)
                    .min(current[j] + 1),
            );
        }
        previous = current;
    }
    previous[b.len()]
}

/// A layer whose file is not in the folder renders nothing; so does one
/// naming a resource the project does not have, and a keyframe swapping to
/// one. Only with a listing: the document alone cannot know.
fn missing_media(meta: &ProjectMetadata, listing: &[String], out: &mut Report) {
    let resolved = promo_model::effective_resources(meta, listing);
    let find = |id: &str| resolved.iter().find(|r| r.resource.id == id);
    for layer in promo_model::nesting::all_layers(meta) {
        if let Some(id) = layer.resource_id.as_deref() {
            match find(id) {
                None => out.breaks(format!(
                    "layer \"{}\" will not render — its resourceID \"{id}\" names no resource \
                     in the project",
                    layer.name
                )),
                Some(r) if r.is_missing() && r.resource.recipe.is_none() => out.breaks(format!(
                    "layer \"{}\" will not render — its file \"{}\" is not in Resources/",
                    layer.name, r.resource.filename
                )),
                Some(_) => {}
            }
        }
        for keyframe in &layer.keyframes {
            let Some(id) = keyframe.resource_id.as_deref() else {
                continue;
            };
            match find(id) {
                None => out.breaks(format!(
                    "layer \"{}\" keyframe at {}s swaps to \"{id}\", which names no resource \
                     in the project — the swap shows nothing",
                    layer.name, keyframe.time
                )),
                Some(r) if r.is_missing() && r.resource.recipe.is_none() => out.breaks(format!(
                    "layer \"{}\" keyframe at {}s swaps to \"{}\", whose file \"{}\" is not \
                         in Resources/ — the swap shows nothing",
                    layer.name, keyframe.time, r.resource.display_name, r.resource.filename
                )),
                Some(_) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project wrapped around the given layers and resources, on a
    /// 1000x500 canvas.
    fn project(layers: &str, resources: &str) -> String {
        format!(
            r#"{{"id":"P","name":"t","createdAt":0,"state":"recorded","minReaderVersion":47,
                "trimStart":0,"trimEnd":4,"videoDuration":4,"subtitles":[],
                "compositionSettings":{{"canvasWidth":1000,"canvasHeight":500}},
                "resources":[{resources}],"layers":[{layers}]}}"#
        )
    }

    const IMAGE: &str = r#"{"id":"I","kind":"image","filename":"a.png","displayName":"swatch","addedAt":0,
        "pixelWidth":100,"pixelHeight":100,"imageCuts":[],"disabledAudioTrackIndices":[]}"#;

    fn layer(extra: &str, keyframes: &str) -> String {
        numbered_layer("L", extra, keyframes)
    }

    fn numbered_layer(id: &str, extra: &str, keyframes: &str) -> String {
        format!(
            r#"{{"id":"{id}","name":"card","sortIndex":0,"kind":"image","isEnabled":true,
                "startTime":0,"duration":4,"resourceID":"I"{extra},"keyframes":[{keyframes}]}}"#
        )
    }

    fn run(json: &str, listing: Option<&[String]>) -> Report {
        let meta = ProjectMetadata::from_json(json).expect("decodes");
        report(
            &meta,
            &Context {
                json: Some(json),
                listing,
                layout: false,
            },
        )
    }

    fn breaks(report: &Report) -> Vec<String> {
        report
            .findings
            .iter()
            .filter(|f| f.severity == Severity::Breaks)
            .map(|f| f.message.clone())
            .collect()
    }

    #[test]
    fn a_project_with_nothing_wrong_says_ok() {
        let json = project(&layer("", ""), IMAGE);
        let listing = vec!["a.png".to_string()];
        let report = run(&json, Some(&listing));
        assert!(report.is_clean(), "{}", report.text());
        assert_eq!(
            report.text(),
            "ok — nothing the renderer would quietly correct"
        );
        assert_eq!(report.json()["ok"], true);
        assert_eq!(report.json()["renders"], true);
    }

    /// Every no-op an agent's CLI stress test met (review 2026-09-27) is a
    /// break, and a break leads the answer with NOT OK — the answer used to
    /// start "ok" beside every one of them.
    #[test]
    fn every_no_op_the_stress_test_met_leads_with_not_ok() {
        let caption = r#"{"id":"C","kind":"caption","filename":"","displayName":"title","addedAt":0,
            "imageCuts":[],"disabledAudioTrackIndices":[],"captionText":"Hi",
            "captionStyle":{"subtitleFontSize":64,"bold":true,"colorHex":"FFFFFF"}}"#;
        let layers = [
            // A layer-level opacity: read on keyframes only.
            numbered_layer("L1", r#","opacity":0.5"#, ""),
            // A typo on a keyframe, which has no bag: serde drops it.
            numbered_layer(
                "L2",
                "",
                r#"{"id":"K1","time":0,"transitionDuration":0,"opacty":0.2}"#,
            ),
            // Sub-pixel sizes (fractions meant as percentages), zoom and
            // shifts beside a placement that sizes.
            numbered_layer(
                "L3",
                "",
                r#"{"id":"K2","time":0,"transitionDuration":0,"zoom":0.5,"horizontalShift":10,
                    "placement":{"height":0.42}}"#,
            ),
        ]
        .join(",")
            + r#",{"id":"T","name":"title","sortIndex":1,"kind":"caption","isEnabled":true,
                "startTime":0,"duration":4,"resourceID":"C","keyframes":[]}"#;
        let image = IMAGE.replace(r#""a.png""#, r#""Resources/a.png""#);
        let json = project(&layers, &format!("{image},{caption}"));
        let report = run(&json, None);
        let text = report.text();
        assert!(text.starts_with("NOT OK — "), "{text}");
        let found = breaks(&report);
        for expected in [
            r#"layer "card": "opacity" is not read here, so it has no effect — it is read at keyframes[].opacity"#,
            r#""opacty" is not read here, so it has no effect — did you mean "opacity"?"#,
            r#"caption "title", captionStyle: "subtitleFontSize" is not read here, so it has no effect — did you mean "fontSize"? ("subtitleFontSize" is the composition's default, compositionSettings.subtitleFontSize)"#,
            r#""bold" is not read here, so it has no effect — did you mean "isBold"?"#,
            r#""colorHex" is not read here, so it has no effect — did you mean "textColorHex"?"#,
            "placement height 0.42 is in canvas pixels",
            "for 42% of the canvas write 210",
            "zoom beside a placement that sets height does nothing",
            "horizontalShift/verticalShift beside a placement do nothing",
            r#"filename "Resources/a.png" names a folder"#,
        ] {
            assert!(
                found.iter().any(|line| line.contains(expected)),
                "missing {expected:?} in:\n{text}"
            );
        }
        // The keyframe typo names its layer and moment.
        assert!(
            found
                .iter()
                .any(|l| l.starts_with(r#"layer "card" keyframe at 0s: "opacty""#)),
            "{text}"
        );
        assert_eq!(report.json()["renders"], false);
    }

    /// A placement that only positions keeps the keyframe's own zoom — the
    /// engine reads it — so it is not called a no-op; the shifts are.
    #[test]
    fn a_position_only_placement_keeps_its_zoom() {
        let json = project(
            &layer(
                "",
                r#"{"id":"K","time":0,"transitionDuration":0,"zoom":0.5,
                    "placement":{"anchor":"bottomRight"}}"#,
            ),
            IMAGE,
        );
        let report = run(&json, None);
        assert!(report.is_clean(), "{}", report.text());
    }

    /// What the apps and the author tools read out of the bags is not
    /// "unread": a narration's speech, entry and exit transitions, the
    /// author's handles, an editor's $schema.
    #[test]
    fn keys_the_apps_read_are_not_called_unread() {
        let json = project(
            &layer(
                r#","entryTransition":{"kind":"fade"},"isAudioFocused":true"#,
                "",
            ),
            &IMAGE.replace(
                r#""imageCuts":[]"#,
                r#""imageCuts":[],"speech":{"text":"hi"},"libraryID":"X""#,
            ),
        )
        .replacen(
            r#"{"id":"P","#,
            r#"{"$schema":"x","handles":{"L":"card"},"id":"P","#,
            1,
        );
        let report = run(&json, None);
        assert!(report.is_clean(), "{}", report.text());
    }

    /// A warning alone still answers ok: the project renders, adjusted.
    #[test]
    fn warnings_alone_still_say_ok() {
        let json = project(
            &layer(
                "",
                r#"{"id":"K","time":0,"transitionDuration":0,"shutter":1.5}"#,
            ),
            IMAGE,
        );
        let report = run(&json, None);
        assert!(report.renders_as_written());
        assert!(
            report
                .text()
                .starts_with("ok — the project renders, with 1 warning(s):"),
            "{}",
            report.text()
        );
    }

    /// Missing media needs the folder: with a listing that lacks the file
    /// the layer is a break, with one that has it there is nothing to say.
    #[test]
    fn missing_media_is_a_break_when_the_folder_is_known() {
        let json = project(&layer("", ""), IMAGE);
        let empty: Vec<String> = Vec::new();
        let report = run(&json, Some(&empty));
        assert_eq!(
            breaks(&report),
            [r#"layer "card" will not render — its file "a.png" is not in Resources/"#]
        );
        let present = vec!["a.png".to_string()];
        assert!(run(&json, Some(&present)).is_clean());
    }

    /// The app's own round trip names keys too; one the core already named
    /// is left out, anything else joins the one list under one headline.
    #[test]
    fn a_foreign_finding_the_core_already_made_is_left_out() {
        let json = project(&layer(r#","opacity":0.5"#, ""), IMAGE);
        let mut report = run(&json, None);
        assert_eq!(
            report.findings[0].shape.as_deref(),
            Some("layers[].opacity")
        );
        report.merge_foreign(vec![
            Finding {
                severity: Severity::Breaks,
                message: "the app drops layers[].opacity".into(),
                shape: Some("layers[].opacity".into()),
            },
            Finding {
                severity: Severity::Breaks,
                message: "the app drops layers[].sparkle".into(),
                shape: Some("layers[].sparkle".into()),
            },
        ]);
        let text = report.text();
        assert!(text.starts_with("NOT OK — 2 thing(s)"), "{text}");
        assert!(!text.contains("drops layers[].opacity"), "{text}");
        assert!(text.contains("drops layers[].sparkle"), "{text}");
    }

    /// A caption's zoom is its font size in points, and a caption placed by
    /// its style is not moved by shifts.
    #[test]
    fn a_caption_names_what_its_keyframes_cannot_do() {
        let caption = r#"{"id":"C","kind":"caption","filename":"","displayName":"title","addedAt":0,
            "imageCuts":[],"disabledAudioTrackIndices":[],"captionText":"Hi",
            "captionStyle":{"placement":{"anchor":"top"}}}"#;
        let layers = r#"{"id":"T","name":"title","sortIndex":0,"kind":"caption","isEnabled":true,
            "startTime":0,"duration":4,"resourceID":"C","keyframes":[
              {"id":"K","time":0,"transitionDuration":0,"zoom":1.2,"verticalShift":40}]}"#;
        let report = run(&project(layers, caption), None);
        let found = breaks(&report);
        assert!(
            found
                .iter()
                .any(|l| l.contains("zoom 1.2 draws the words 1.2 pt tall")),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .any(|l| l.contains("verticalShift moves a caption through its top margin")),
            "{found:?}"
        );
    }

    /// The app pins a new caption keyframe's margins to the style's own;
    /// on a placed caption those say nothing, and are no finding.
    #[test]
    fn margins_equal_to_the_base_style_are_no_finding() {
        let caption = r#"{"id":"C","kind":"caption","filename":"","displayName":"title","addedAt":0,
            "imageCuts":[],"disabledAudioTrackIndices":[],"captionText":"Hi",
            "captionStyle":{"placement":{"anchor":"top"},"leftMargin":120,"verticalMargin":80}}"#;
        let layers = r#"{"id":"T","name":"title","sortIndex":0,"kind":"caption","isEnabled":true,
            "startTime":0,"duration":4,"resourceID":"C","keyframes":[
              {"id":"K","time":1,"transitionDuration":0,"zoom":48,"verticalShift":80,"horizontalShift":120}]}"#;
        let report = run(&project(layers, caption), None);
        assert!(report.is_clean(), "{}", report.text());
    }

    /// A caption's keyframes place its box now (anchor and offset): that
    /// is no finding at all, and a size on such a rule is a break — a
    /// caption is as big as its words.
    #[test]
    fn a_caption_placed_by_keyframes_is_fine_and_its_sizes_are_not() {
        let caption = r#"{"id":"C","kind":"caption","filename":"","displayName":"title","addedAt":0,
            "imageCuts":[],"disabledAudioTrackIndices":[],"captionText":"Hi"}"#;
        let layers = |rule: &str| {
            format!(
                r#"{{"id":"T","name":"title","sortIndex":0,"kind":"caption","isEnabled":true,
                "startTime":0,"duration":4,"resourceID":"C","keyframes":[
                  {{"id":"K","time":0,"transitionDuration":0,"placement":{rule}}}]}}"#
            )
        };
        let moving = run(
            &project(&layers(r#"{"anchor":"topLeft","offset":[10,10]}"#), caption),
            None,
        );
        assert!(moving.is_clean(), "{}", moving.text());
        let sized = run(
            &project(&layers(r#"{"anchor":"top","height":80}"#), caption),
            None,
        );
        assert!(
            breaks(&sized)
                .iter()
                .any(|l| l.contains("a caption's placement reads anchor and offset only")),
            "{}",
            sized.text()
        );
    }
}

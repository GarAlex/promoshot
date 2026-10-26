//! Generated layers (core-private docs/MIDI.md §7): one track of a MIDI
//! block made by ACE-Step's `lego` task, which hears the rest of the
//! block's mix and adds a layer to it. The MIDI stays the timing
//! backbone; the layer is a stored file in `Resources/.midi/` that every
//! host mixes the same way ([`crate::render::render_mix`]), so only making
//! it needs ACE-Step.
//!
//! This is the client for ACE-Step's REST API (`acestep-api`): `POST
//! /release_task` (multipart, the context take as `src_audio`), `POST
//! /query_result` until the task ends, then `GET` the audio it names.
//! Blocking, with a timeout, a poll interval and a cancel hook, so the
//! CLI, the headless MCP server and the apps (over promo-ffi, on a thread
//! of their own) share one implementation. The endpoint and optional key
//! come from `PROMOSHOT_ACESTEP_URL` / `PROMOSHOT_ACESTEP_KEY` or the
//! caller. PromoShot never installs or runs ACE-Step.
//!
//! The request, the context's length, the cut and the gate follow the
//! experiment (2026-10-26, ACE-Step 1.5 base on MLX): lego returns the
//! layer alone, starting at the context's sample 0, its length cut to a
//! multiple of [`LATENT_FRAMES`]; it needs ~30 s of context (an 8 s loop
//! as is comes back as noise), so a loop is tiled to [`LOOP_PERIODS`]
//! periods and the third is kept, its last [`SEAM_CROSSFADE_MS`] crossed
//! into its own pre-roll. The gate ([`crate::layer::gate`]) refuses a
//! percussive layer off the sixteenth grid and a tonal one that misses the
//! chords or sits out bars; the kept layer is loudness-normalised.

use std::path::Path;
use std::time::{Duration, Instant};

use promo_model::midi::{self, MidiBlock};
use serde_json::{json, Map, Value};

use crate::render::{self, Bank, Stereo, SAMPLE_RATE};
use crate::score;

/// Where ACE-Step's API answers when nothing says otherwise.
pub const DEFAULT_URL: &str = "http://localhost:8001";
pub const URL_ENV: &str = "PROMOSHOT_ACESTEP_URL";
pub const KEY_ENV: &str = "PROMOSHOT_ACESTEP_KEY";
/// The provider and task a generated track records.
pub const PROVIDER: &str = "ace-step";
pub const TASK: &str = "lego";
/// The ACE-Step project, for the setup help.
pub const REPOSITORY: &str = "https://github.com/ace-step/ACE-Step-1.5";

/// The equal-power crossfade at a loop layer's seam, ms: 50 removed every
/// click in the experiment's takes (10–25 did not).
pub const SEAM_CROSSFADE_MS: f64 = 50.0;
/// The context lego needs, seconds: under it a take is padded by tiling
/// (8 s: noise; 16 s: half the takes usable; 32 s: all four right).
pub const MIN_CONTEXT_SECONDS: f64 = 30.0;
/// A loop's context is the loop this many times, and the kept period is
/// [`LOOP_PERIOD_KEPT`] (0-based): not the first (late entries, silence)
/// nor the last (it fades out), and with a pre-roll for the seam.
pub const LOOP_PERIODS: usize = 4;
pub const LOOP_PERIOD_KEPT: usize = 2;
/// Lego's output is cut to a multiple of one latent (40 ms at 48 kHz),
/// from the end: the context is padded with silence up to one and the
/// answer trimmed back.
pub const LATENT_FRAMES: usize = 1920;
/// The DiT the experiment used (and lego needs: the turbo model is not it).
pub const DEFAULT_MODEL: &str = "acestep-v15-base";
pub const INFERENCE_STEPS: u32 = 50;
pub const GUIDANCE_SCALE: f64 = 7.0;
pub const SHIFT: f64 = 3.0;
/// ~30 s per 32 s of audio warm, plus ~24 s to load the model the first time.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(900);
pub const DEFAULT_POLL: Duration = Duration::from_secs(2);

/// The error a cancelled generation ends with.
pub const CANCELLED: &str = "generation cancelled";

/// An ACE-Step API endpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct Endpoint {
    /// No trailing slash.
    pub url: String,
    pub key: Option<String>,
}

impl Endpoint {
    /// `url` / `key` when given, else `PROMOSHOT_ACESTEP_URL` /
    /// `PROMOSHOT_ACESTEP_KEY`, else [`DEFAULT_URL`] and no key.
    pub fn resolve(url: Option<&str>, key: Option<&str>) -> Endpoint {
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let url = url
            .map(str::to_string)
            .filter(|u| !u.trim().is_empty())
            .or_else(|| env(URL_ENV))
            .unwrap_or_else(|| DEFAULT_URL.to_string());
        let key = key
            .map(str::to_string)
            .filter(|k| !k.trim().is_empty())
            .or_else(|| env(KEY_ENV));
        Endpoint {
            url: url.trim().trim_end_matches('/').to_string(),
            key: key.map(|k| k.trim().to_string()),
        }
    }

    fn auth(&self, request: ureq::Request) -> ureq::Request {
        match &self.key {
            Some(key) => request.set("Authorization", &format!("Bearer {key}")),
            None => request,
        }
    }
}

/// What to generate.
#[derive(Debug, Clone)]
pub struct Request {
    /// The track's name: a new generated track, or the one to regenerate.
    pub track: String,
    /// What to play ("warm Rhodes comping"); a new track needs one, a
    /// regenerate keeps its own.
    pub caption: Option<String>,
    /// None: a fresh seed (recorded, so the layer can be made again).
    pub seed: Option<i64>,
    /// Bars, 1-based, `to` exclusive; None keeps the track's own, else the
    /// whole take.
    pub range: Option<(f64, f64)>,
    /// The DiT model; None is [`DEFAULT_MODEL`].
    pub model: Option<String>,
    /// ACE-Step's `track_name` ([`crate::layer::TRACK_NAMES`]); None keeps
    /// the track's own, else follows the caption.
    pub track_name: Option<String>,
    pub timeout: Duration,
    pub poll: Duration,
}

impl Request {
    pub fn new(track: &str) -> Request {
        Request {
            track: track.to_string(),
            caption: None,
            seed: None,
            range: None,
            model: None,
            track_name: None,
            timeout: DEFAULT_TIMEOUT,
            poll: DEFAULT_POLL,
        }
    }
}

/// Where a generation is, for a progress line.
pub trait Progress {
    /// A new phase: `rendering`, `uploading`, `queued`, `generating`,
    /// `downloading`, `aligning`, `mixing`.
    fn phase(&mut self, phase: &str);
    /// True once the caller wants it stopped; checked between steps and
    /// while waiting.
    fn cancelled(&self) -> bool {
        false
    }
}

/// No progress, no cancel.
pub struct Quiet;
impl Progress for Quiet {
    fn phase(&mut self, _: &str) {}
}

/// What a generation made.
#[derive(Debug, Clone)]
pub struct Generated {
    pub resource: String,
    pub track: String,
    /// In `Resources/.midi/`.
    pub file: String,
    pub caption: String,
    pub seed: i64,
    pub model: Option<String>,
    pub range: Option<(f64, f64)>,
    pub track_name: String,
    pub keyscale: Option<String>,
    /// What the gate measured (it kept the layer).
    pub gate: crate::layer::Gate,
    /// Wall-clock seconds, upload to download.
    pub seconds: f64,
    pub created: bool,
}

impl Generated {
    pub fn to_json(&self) -> Value {
        json!({
            "resource": self.resource, "track": self.track, "file": self.file,
            "caption": self.caption, "seed": self.seed, "model": self.model,
            "range": self.range.map(|(from, to)| json!({ "from": bar(from), "to": bar(to) })),
            "trackName": self.track_name, "keyscale": self.keyscale,
            "gate": self.gate.to_json(),
            "seconds": (self.seconds * 10.0).round() / 10.0,
            "created": self.created,
        })
    }

    pub fn report(&self) -> String {
        format!(
            "{}: {} {} layer \"{}\" → Resources/.midi/{} (seed {}, {:.1} s); {}",
            self.resource,
            if self.created {
                "generated"
            } else {
                "regenerated"
            },
            self.track_name,
            self.track,
            self.file,
            self.seed,
            self.seconds,
            self.gate.summary()
        )
    }
}

// ------------------------------------------------------------------ status

/// Whether ACE-Step answers at `endpoint`: `{url, ok, service, version,
/// models: [name], defaultModel}`. The error says how to start it.
pub fn status(endpoint: &Endpoint, timeout: Duration) -> Result<Value, String> {
    let agent = agent(timeout);
    let health = get_json(&agent, endpoint, "/health")?;
    let data = &health["data"];
    let mut out = json!({
        "url": endpoint.url, "ok": data["status"].as_str().is_none_or(|s| s == "ok"),
        "service": data["service"], "version": data["version"],
    });
    if let Ok(models) = get_json(&agent, endpoint, "/v1/models") {
        out["models"] = json!(models["data"]["models"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| m["name"].as_str())
            .collect::<Vec<_>>());
        out["defaultModel"] = models["data"]["default_model"].clone();
    }
    Ok(out)
}

fn not_answering(endpoint: &Endpoint, why: &str) -> String {
    format!(
        "ACE-Step is not answering at {} ({why}). PromoShot does not install it: set it up from \
         {REPOSITORY} (download the main model bundle once, plus the base DiT), start its API \
         server with the base model — `ACESTEP_CONFIG_PATH={DEFAULT_MODEL} acestep-api`, port \
         8001 — and set {URL_ENV} if it listens elsewhere",
        endpoint.url
    )
}

// ---------------------------------------------------------------- generate

/// Generates (or regenerates) the layer `request.track` of the MIDI
/// resource `resource_id` in `doc`, writes its file into
/// `resources_dir/.midi/`, records it on the track, then settles the take
/// with it mixed in. On any failure — refused for drift, cancelled, the
/// server's error — `doc` and the project's files are as they were.
pub fn generate_doc(
    doc: &mut Value,
    resources_dir: &Path,
    resource_id: &str,
    request: &Request,
    endpoint: &Endpoint,
    bank: &Bank,
    progress: &mut dyn Progress,
) -> Result<(Generated, crate::Settled), String> {
    let index = doc["resources"]
        .as_array()
        .and_then(|l| l.iter().position(|r| r["id"] == resource_id))
        .ok_or_else(|| format!("no resource `{resource_id}` in the project"))?;
    let resource = doc["resources"][index].clone();
    let value = resource
        .get(midi::KEY)
        .filter(|v| v.is_object())
        .ok_or_else(|| format!("{resource_id}: the resource has no `midi` block"))?
        .clone();
    let block = MidiBlock::from_value(&value).map_err(|e| format!("{resource_id}: {e}"))?;
    if block.source() == "effect" {
        return Err(format!(
            "{resource_id}: a sound effect takes no generated layer — use a MIDI loop or song"
        ));
    }
    let name = request.track.trim();
    if name.is_empty() {
        return Err("name the track to generate (`track`)".into());
    }
    let tracks: Vec<Value> = value["tracks"].as_array().cloned().unwrap_or_default();
    let existing = tracks.iter().position(|t| t["name"] == name);
    let held: Map<String, Value> = match existing {
        Some(i) => match tracks[i].get("generated").and_then(Value::as_object) {
            Some(g) => g.clone(),
            None => {
                return Err(format!(
                    "{resource_id}: track \"{name}\" is a MIDI track — name a new track or a \
                     generated one"
                ))
            }
        },
        None => Map::new(),
    };
    let caption = request
        .caption
        .clone()
        .filter(|c| !c.trim().is_empty())
        .or_else(|| {
            held.get("caption")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .ok_or_else(|| {
            format!("a new generated layer needs a caption — what \"{name}\" should play")
        })?;
    let range = request.range.or_else(|| {
        let r = held.get("range")?;
        Some((r["from"].as_f64()?, r["to"].as_f64()?))
    });
    if let Some((from, to)) = range {
        if !(from.is_finite() && to.is_finite() && from >= 1.0 && to > from) {
            return Err(format!(
                "range {from}–{to}: bars are 1-based and `to` (exclusive) comes after `from`"
            ));
        }
    }
    let seed = request.seed.unwrap_or_else(fresh_seed);
    let model = request
        .model
        .clone()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let track_name: String = match request.track_name.clone().or_else(|| {
        held.get("trackName")
            .and_then(Value::as_str)
            .map(str::to_string)
    }) {
        Some(t) => {
            let t = t.trim().to_ascii_lowercase();
            if !crate::layer::TRACK_NAMES.contains(&t.as_str()) {
                return Err(format!(
                    "track name \"{t}\": ACE-Step's lego knows {}",
                    crate::layer::TRACK_NAMES.join(", ")
                ));
            }
            t
        }
        None => crate::layer::track_name_for(&caption, name).to_string(),
    };

    // The context: the block without its generated tracks, as it sounds.
    progress.phase("rendering");
    let mid = slice_one_mid(&block, resources_dir)?;
    let mut heard_value = value.clone();
    if let Some(list) = heard_value["tracks"].as_array_mut() {
        list.retain(|t| t.get("generated").is_none_or(Value::is_null));
    }
    let heard = MidiBlock::from_value(&heard_value).map_err(|e| format!("{resource_id}: {e}"))?;
    let score = score::expand(&heard, mid.as_deref()).map_err(|e| format!("{resource_id}: {e}"))?;
    if score.tracks.iter().all(|t| t.notes == 0 || t.muted) {
        return Err(format!(
            "{resource_id}: the MIDI plays nothing for the layer to hear — add notes first"
        ));
    }
    let take = render::render(&score, bank).map_err(|e| format!("{resource_id}: {e}"))?;
    let frames = take.frames();
    let take_seconds = frames as f64 / f64::from(SAMPLE_RATE);
    // Lego needs ~30 s to hear: a loop is tiled and a middle period kept;
    // a short one-shot is tiled too and its first copy kept.
    let (periods, kept) = if score.looped {
        let periods = LOOP_PERIODS.max((MIN_CONTEXT_SECONDS / take_seconds).ceil() as usize);
        if take_seconds * periods as f64 > 600.0 {
            (2, 1)
        } else {
            (periods, LOOP_PERIOD_KEPT)
        }
    } else {
        (
            (MIN_CONTEXT_SECONDS / take_seconds).ceil().max(1.0) as usize,
            0,
        )
    };
    let mut context = Stereo {
        left: take.left.repeat(periods),
        right: take.right.repeat(periods),
    };
    let padded = context.frames().div_ceil(LATENT_FRAMES) * LATENT_FRAMES;
    context.left.resize(padded, 0.0);
    context.right.resize(padded, 0.0);
    // The range in the take's seconds.
    let span = match range {
        None => (0.0, take_seconds),
        Some((from, to)) => {
            let at = |bar: f64| {
                (score.map.seconds((bar - 1.0) * score.bar_beats) - score.start_seconds)
                    .clamp(0.0, take_seconds)
            };
            let (a, b) = (at(from), at(to));
            if b - a < 0.05 {
                return Err(format!(
                    "range {from}–{to} is outside the take (it plays {:.2} s)",
                    take_seconds
                ));
            }
            (a, b)
        }
    };
    let keyscale = heard
        .extra
        .get("key")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| key_scale(&heard));
    if progress.cancelled() {
        return Err(CANCELLED.into());
    }

    // ACE-Step.
    let started = Instant::now();
    let agent = agent(request.timeout);
    let fields = form_fields(&FormInput {
        caption: &caption,
        track_name: &track_name,
        bpm: score.bpm,
        key: keyscale.clone(),
        meter: score.time_signature,
        seed,
        model: &model,
    });
    progress.phase("uploading");
    let task = release_task(&agent, endpoint, &fields, &render::stereo_wav(&context))?;
    progress.phase("queued");
    let (file_url, dit_model) = wait(&agent, endpoint, &task, request, started, progress)?;
    if progress.cancelled() {
        return Err(CANCELLED.into());
    }
    progress.phase("downloading");
    let audio = download(&agent, endpoint, &file_url)?;
    let made = render::read_stereo(&audio)
        .map_err(|e| format!("ACE-Step's audio: {e} (ask it for audio_format=wav)"))?;
    let seconds = started.elapsed().as_secs_f64();

    // The layer: one take long, aligned at the take's start.
    progress.phase("aligning");
    let mut layer = assemble(&made, frames, kept, score.looped);
    mask(&mut layer, span, take_seconds);
    // A loop's onsets are counted over every period but the last (the
    // grid repeats with them); a one-shot's over the layer itself.
    let (rhythm, rhythm_span) = if score.looped && periods > 1 {
        let n = (periods - 1) * frames;
        let mut wide = Stereo {
            left: made.left.iter().copied().take(n).collect(),
            right: made.right.iter().copied().take(n).collect(),
        };
        wide.left.resize(n, 0.0);
        wide.right.resize(n, 0.0);
        let full = range.is_none();
        (
            wide,
            if full {
                (0.0, n as f64 / f64::from(SAMPLE_RATE))
            } else {
                (
                    kept as f64 * take_seconds + span.0,
                    kept as f64 * take_seconds + span.1,
                )
            },
        )
    } else {
        (layer.clone(), span)
    };
    let gate = crate::layer::gate(
        &layer,
        &rhythm,
        rhythm_span,
        &score,
        &heard,
        &track_name,
        span,
    );
    if let Some(why) = &gate.refused {
        return Err(why.clone());
    }
    crate::layer::normalise(&mut layer, crate::layer::target_lufs(&track_name));
    if progress.cancelled() {
        return Err(CANCELLED.into());
    }

    // Kept: the file, the track's record, the take.
    progress.phase("mixing");
    let file = held
        .get("file")
        .and_then(Value::as_str)
        .filter(|f| plain(f))
        .map(str::to_string)
        .unwrap_or_else(|| layer_name(&resource, name, &tracks));
    let path = crate::settle::mid_path(resources_dir, &file);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let before_file = std::fs::read(&path).ok();
    crate::sounds::write_atomic(&path, &render::stereo_wav(&layer))?;
    let mut record = held.clone();
    record.insert("provider".into(), json!(PROVIDER));
    record.insert("task".into(), json!(TASK));
    record.insert("caption".into(), json!(caption));
    record.insert(
        "model".into(),
        json!(dit_model.clone().unwrap_or_else(|| model.clone())),
    );
    record.insert("trackName".into(), json!(track_name));
    match &keyscale {
        Some(k) => record.insert("keyscale".into(), json!(k)),
        None => record.remove("keyscale"),
    };
    record.insert("seed".into(), json!(seed));
    match range {
        Some((from, to)) => {
            record.insert("range".into(), json!({ "from": bar(from), "to": bar(to) }))
        }
        None => record.remove("range"),
    };
    record.insert("file".into(), json!(file));
    record.insert(
        "contextHash".into(),
        json!(midi::context_hash(&value, mid.as_deref())),
    );
    record.insert("status".into(), json!(midi::STATUS_CURRENT));
    record.remove("alignment");
    record.insert("gate".into(), gate.to_json());
    let before = doc.clone();
    {
        let list = doc["resources"][index][midi::KEY]
            .as_object_mut()
            .ok_or("the midi block is not an object")?
            .entry("tracks")
            .or_insert_with(|| json!([]));
        let list = list.as_array_mut().ok_or("midi.tracks is not a list")?;
        match existing {
            Some(i) => list[i]["generated"] = Value::Object(record),
            None => list.push(json!({ "name": name, "generated": record })),
        }
    }
    let settled = match crate::settle_doc(doc, resources_dir, resource_id, bank) {
        Ok(settled) => settled,
        Err(e) => {
            *doc = before;
            match before_file {
                Some(bytes) => {
                    let _ = crate::sounds::write_atomic(&path, &bytes);
                }
                None => {
                    let _ = std::fs::remove_file(&path);
                }
            }
            return Err(e);
        }
    };
    Ok((
        Generated {
            resource: resource_id.to_string(),
            track: name.to_string(),
            file,
            caption,
            seed,
            model: Some(dit_model.unwrap_or(model)),
            range,
            track_name,
            keyscale,
            gate,
            seconds,
            created: existing.is_none(),
        },
        settled,
    ))
}

/// [`generate_doc`] on the project at `project_dir`: its metadata.json
/// read raw, generated, settled, stamped and written back atomically.
pub fn generate(
    project_dir: &Path,
    resource_id: &str,
    request: &Request,
    endpoint: &Endpoint,
    bank: &Bank,
    progress: &mut dyn Progress,
) -> Result<(Generated, crate::Settled), String> {
    let meta_path = project_dir.join("metadata.json");
    let text = std::fs::read_to_string(&meta_path)
        .map_err(|_| format!("no metadata.json in {}", project_dir.display()))?;
    let mut doc: Value = serde_json::from_str(&text).map_err(|e| format!("decode: {e}"))?;
    let out = generate_doc(
        &mut doc,
        &project_dir.join("Resources"),
        resource_id,
        request,
        endpoint,
        bank,
        progress,
    )?;
    crate::settle::stamp(&mut doc);
    let text = serde_json::to_string(&doc).map_err(|e| e.to_string())?;
    crate::sounds::write_atomic(&meta_path, text.as_bytes())?;
    Ok(out)
}

/// A bar number as JSON: whole bars as integers (what the apps read).
fn bar(x: f64) -> Value {
    if x.fract() == 0.0 && x.abs() < 1e9 {
        json!(x as i64)
    } else {
        json!(x)
    }
}

fn plain(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && !name.starts_with('.')
        && name != ".."
}

/// A slice-1 file block's `.mid` (the context hash and the expansion read it).
fn slice_one_mid(block: &MidiBlock, resources_dir: &Path) -> Result<Option<Vec<u8>>, String> {
    if block.source() != "file" || score::has_notes(block) {
        return Ok(None);
    }
    let name = block
        .file
        .as_deref()
        .or(block.original.as_deref())
        .ok_or("a file source names no `file`")?;
    if !plain(name) {
        return Err(format!("midi.file \"{name}\" must be a plain file name"));
    }
    let path = crate::settle::mid_path(resources_dir, name);
    std::fs::read(&path)
        .map(Some)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// "Groove · Rhodes.wav": the resource's display name (or id) and the
/// track's, made a plain name no other track of the block uses.
fn layer_name(resource: &Value, track: &str, tracks: &[Value]) -> String {
    let clean = |s: &str| -> String {
        s.trim()
            .trim_start_matches('.')
            .chars()
            .map(|c| {
                if matches!(c, '/' | '\\' | ':') {
                    '-'
                } else {
                    c
                }
            })
            .collect()
    };
    let owner = clean(
        resource["displayName"]
            .as_str()
            .filter(|d| !d.trim().is_empty())
            .or(resource["id"].as_str())
            .unwrap_or("MIDI"),
    );
    let stem = format!("{owner} · {}", clean(track));
    let taken = |n: &str| {
        tracks
            .iter()
            .any(|t| t["generated"]["file"].as_str() == Some(n))
    };
    let mut name = format!("{stem}.wav");
    let mut n = 2;
    while taken(&name) || !plain(&name) {
        name = format!("{stem} {n}.wav");
        n += 1;
        if n > 999 {
            return format!("layer-{}.wav", fresh_seed().unsigned_abs());
        }
    }
    name
}

fn fresh_seed() -> i64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mixed = (nanos as u64) ^ (u64::from(std::process::id()) << 32);
    // ACE-Step takes 32-bit seeds.
    i64::from((mixed ^ (mixed >> 29)) as u32 & 0x7FFF_FFFF)
}

// -------------------------------------------------------------- the layer

/// The model's audio cut to one take of `frames`, sample 0 at the take's
/// start: period `kept` of the tiled context. A loop's last
/// [`SEAM_CROSSFADE_MS`] are crossed (equal power) into the period's own
/// pre-roll — the audio just before it, which runs on into its first
/// sample — so the wrap continues exactly. A loop kept from period 0 has
/// no pre-roll and crosses into its own head instead.
pub fn assemble(made: &Stereo, frames: usize, kept: usize, looped: bool) -> Stereo {
    let at = |x: &[f32], i: usize| x.get(i).copied().unwrap_or(0.0);
    let start = kept * frames;
    let mut layer = Stereo {
        left: (0..frames).map(|i| at(&made.left, start + i)).collect(),
        right: (0..frames).map(|i| at(&made.right, start + i)).collect(),
    };
    if !looped {
        return layer;
    }
    let fade =
        ((SEAM_CROSSFADE_MS / 1000.0 * f64::from(SAMPLE_RATE)).round() as usize).min(frames / 2);
    for i in 0..fade {
        let theta = (i as f64 + 0.5) / fade as f64 * std::f64::consts::FRAC_PI_2;
        let (out, inn) = (theta.cos() as f32, theta.sin() as f32);
        let k = frames - fade + i;
        let (pl, pr) = if start >= fade {
            (
                at(&made.left, start - fade + i),
                at(&made.right, start - fade + i),
            )
        } else {
            (at(&made.left, start + i), at(&made.right, start + i))
        };
        layer.left[k] = layer.left[k] * out + pl * inn;
        layer.right[k] = layer.right[k] * out + pr * inn;
    }
    layer
}

/// Silence outside the range (take seconds), with a short equal-power
/// edge so a layer that starts at bar 3 does not click in.
fn mask(layer: &mut Stereo, span: (f64, f64), take_seconds: f64) {
    if span.0 <= 0.0 && span.1 >= take_seconds {
        return;
    }
    let rate = f64::from(SAMPLE_RATE);
    let (a, b) = ((span.0 * rate) as usize, (span.1 * rate) as usize);
    let edge = (SEAM_CROSSFADE_MS / 1000.0 * rate) as usize;
    for i in 0..layer.frames() {
        let g = if i < a || i >= b {
            0.0
        } else if a > 0 && i < a + edge {
            ((i - a) as f64 / edge as f64 * std::f64::consts::FRAC_PI_2).sin()
        } else if i + edge > b && b < layer.frames() {
            ((b - i) as f64 / edge as f64 * std::f64::consts::FRAC_PI_2).sin()
        } else {
            1.0
        };
        layer.left[i] *= g as f32;
        layer.right[i] *= g as f32;
    }
}

/// The key the MIDI is in, for ACE-Step's `key_scale` ("A minor"):
/// Krumhansl–Kessler profiles against the pitched notes' durations. None
/// when there are no pitched notes.
pub fn key_scale(block: &MidiBlock) -> Option<String> {
    const MAJOR: [f64; 12] = [
        6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
    ];
    const MINOR: [f64; 12] = [
        6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
    ];
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
    ];
    let mut weights = [0.0f64; 12];
    for track in block.tracks.iter().flatten() {
        if track.is_drums() || track.is_generated() {
            continue;
        }
        for n in track.notes.iter().flatten() {
            if n.d.is_finite() && n.d > 0.0 {
                weights[usize::from(n.n % 12)] += n.d.min(16.0);
            }
        }
        for lane in track.steps.iter().flat_map(|s| s.lanes.iter()) {
            weights[usize::from(lane.note % 12)] += lane.on.len() as f64 * 0.25;
        }
    }
    if weights.iter().all(|w| *w <= 0.0) {
        return None;
    }
    let correlate = |profile: &[f64; 12], tonic: usize| {
        let mean_w = weights.iter().sum::<f64>() / 12.0;
        let mean_p = profile.iter().sum::<f64>() / 12.0;
        let (mut num, mut dw, mut dp) = (0.0, 0.0, 0.0);
        for i in 0..12 {
            let w = weights[(i + tonic) % 12] - mean_w;
            let p = profile[i] - mean_p;
            num += w * p;
            dw += w * w;
            dp += p * p;
        }
        if dw <= 0.0 || dp <= 0.0 {
            0.0
        } else {
            num / (dw * dp).sqrt()
        }
    };
    let mut best = (f64::NEG_INFINITY, String::new());
    for (tonic, note) in NAMES.iter().enumerate() {
        for (profile, mode) in [(&MAJOR, "major"), (&MINOR, "minor")] {
            let r = correlate(profile, tonic);
            if r > best.0 + 1e-12 {
                best = (r, format!("{note} {mode}"));
            }
        }
    }
    Some(best.1)
}

// ------------------------------------------------------------------- HTTP

struct FormInput<'a> {
    caption: &'a str,
    track_name: &'a str,
    bpm: f64,
    key: Option<String>,
    meter: [u32; 2],
    seed: i64,
    model: &'a str,
}

/// The `release_task` fields for a lego layer over the uploaded context —
/// the request the experiment found working, in one place. Lego locks the
/// duration to the context and builds its instruction from `track_name`,
/// so neither `audio_duration` nor `instruction` is sent.
fn form_fields(input: &FormInput) -> Vec<(&'static str, String)> {
    let mut fields = vec![
        ("task_type", TASK.to_string()),
        ("track_name", input.track_name.to_string()),
        ("prompt", input.caption.to_string()),
        ("lyrics", "[Instrumental]".into()),
        ("vocal_language", "en".into()),
        (
            "bpm",
            format!("{}", input.bpm.round().clamp(30.0, 300.0) as i64),
        ),
        ("timesignature", input.meter[0].to_string()),
        ("model", input.model.to_string()),
        ("thinking", "false".into()),
        ("use_cot_caption", "false".into()),
        ("use_cot_language", "false".into()),
        ("use_random_seed", "false".into()),
        ("seed", input.seed.to_string()),
        ("batch_size", "1".into()),
        ("inference_steps", INFERENCE_STEPS.to_string()),
        ("guidance_scale", format!("{GUIDANCE_SCALE:.1}")),
        ("shift", format!("{SHIFT:.1}")),
        ("repainting_start", "0".into()),
        ("repainting_end", "-1".into()),
        ("audio_format", "wav".into()),
    ];
    if let Some(key) = &input.key {
        fields.push(("keyscale", key.clone()));
    }
    fields
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(timeout.min(Duration::from_secs(120)))
        .timeout_write(Duration::from_secs(300))
        .build()
}

/// The endpoint's words for a failed call — never the request, which may
/// carry the key.
fn failure(endpoint: &Endpoint, path: &str, e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, response) => {
            let body = response.into_string().unwrap_or_default();
            let detail = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| {
                    v["detail"]
                        .as_str()
                        .or(v["error"].as_str())
                        .map(str::to_string)
                })
                .unwrap_or(body);
            let hint = if code == 401 {
                format!(" — set the key ({KEY_ENV}, or Settings ▸ Music ▸ ACE-Step)")
            } else {
                String::new()
            };
            format!("ACE-Step {path} answered {code}: {}{hint}", detail.trim())
        }
        other => not_answering(endpoint, &other.to_string()),
    }
}

fn get_json(agent: &ureq::Agent, endpoint: &Endpoint, path: &str) -> Result<Value, String> {
    let response = endpoint
        .auth(agent.get(&format!("{}{path}", endpoint.url)))
        .call()
        .map_err(|e| failure(endpoint, path, e))?;
    let text = response.into_string().map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| format!("ACE-Step {path}: {e}"))
}

/// The wrapper's `data`, or its `error`.
fn data(answer: Value, path: &str) -> Result<Value, String> {
    if let Some(error) = answer["error"].as_str().filter(|e| !e.is_empty()) {
        return Err(format!("ACE-Step {path}: {error}"));
    }
    if answer["code"].as_i64().is_some_and(|c| c != 200) {
        return Err(format!("ACE-Step {path} answered code {}", answer["code"]));
    }
    Ok(answer["data"].clone())
}

fn release_task(
    agent: &ureq::Agent,
    endpoint: &Endpoint,
    fields: &[(&str, String)],
    context_wav: &[u8],
) -> Result<String, String> {
    let boundary = format!("promoshot-{:016x}", fresh_seed() as u64 ^ 0x5eed_1e60);
    let mut body = Vec::with_capacity(context_wav.len() + 4096);
    for (name, value) in fields {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"src_audio\"; \
             filename=\"context.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(context_wav);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let path = "/release_task";
    let response = endpoint
        .auth(agent.post(&format!("{}{path}", endpoint.url)))
        .set(
            "Content-Type",
            &format!("multipart/form-data; boundary={boundary}"),
        )
        .send_bytes(&body)
        .map_err(|e| failure(endpoint, path, e))?;
    let text = response.into_string().map_err(|e| e.to_string())?;
    let answer: Value = serde_json::from_str(&text).map_err(|e| format!("ACE-Step {path}: {e}"))?;
    let data = data(answer, path)?;
    data["task_id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("ACE-Step {path} gave no task_id"))
}

/// Polls `query_result` until the task ends: the audio's URL and the DiT
/// model the result names.
fn wait(
    agent: &ureq::Agent,
    endpoint: &Endpoint,
    task: &str,
    request: &Request,
    started: Instant,
    progress: &mut dyn Progress,
) -> Result<(String, Option<String>), String> {
    let path = "/query_result";
    let mut generating = false;
    loop {
        if progress.cancelled() {
            return Err(CANCELLED.into());
        }
        if started.elapsed() > request.timeout {
            return Err(format!(
                "ACE-Step did not finish within {:.0} s (task {task})",
                request.timeout.as_secs_f64()
            ));
        }
        let response = endpoint
            .auth(agent.post(&format!("{}{path}", endpoint.url)))
            .set("Content-Type", "application/json")
            .send_string(&json!({ "task_id_list": [task] }).to_string())
            .map_err(|e| failure(endpoint, path, e))?;
        let text = response.into_string().map_err(|e| e.to_string())?;
        let answer: Value =
            serde_json::from_str(&text).map_err(|e| format!("ACE-Step {path}: {e}"))?;
        let data = data(answer, path)?;
        let entry = data
            .as_array()
            .and_then(|l| l.iter().find(|t| t["task_id"] == task).or(l.first()))
            .cloned()
            .unwrap_or(Value::Null);
        let results = match &entry["result"] {
            Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
            other => other.clone(),
        };
        let first = results
            .as_array()
            .and_then(|l| l.first())
            .cloned()
            .unwrap_or(results.clone());
        match entry["status"].as_i64() {
            Some(1) => {
                let file = first["file"]
                    .as_str()
                    .filter(|f| !f.is_empty())
                    .ok_or_else(|| format!("ACE-Step finished task {task} with no audio file"))?;
                let model = first["dit_model"].as_str().map(str::to_string);
                return Ok((file.to_string(), model));
            }
            Some(2) => {
                let why = first["error"]
                    .as_str()
                    .or(entry["error"].as_str())
                    .or(first["generation_info"].as_str())
                    .unwrap_or("no reason given");
                return Err(format!("ACE-Step failed the generation: {why}"));
            }
            _ => {
                if !generating {
                    generating = true;
                    progress.phase("generating");
                }
            }
        }
        // Sleep in slices, so a cancel lands quickly.
        let until = Instant::now() + request.poll;
        while Instant::now() < until {
            if progress.cancelled() {
                return Err(CANCELLED.into());
            }
            std::thread::sleep((until - Instant::now()).min(Duration::from_millis(25)));
        }
    }
}

fn download(agent: &ureq::Agent, endpoint: &Endpoint, file: &str) -> Result<Vec<u8>, String> {
    let url = if file.starts_with("http://") || file.starts_with("https://") {
        file.to_string()
    } else {
        format!("{}/{}", endpoint.url, file.trim_start_matches('/'))
    };
    let response = endpoint
        .auth(agent.get(&url))
        .call()
        .map_err(|e| failure(endpoint, "/v1/audio", e))?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut response.into_reader(), &mut bytes)
        .map_err(|e| format!("ACE-Step audio: {e}"))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize, f: impl Fn(usize) -> f32) -> Stereo {
        let x: Vec<f32> = (0..frames).map(f).collect();
        Stereo {
            left: x.clone(),
            right: x,
        }
    }

    /// Four periods, the third kept: its last sample comes from the second
    /// period's end — what runs on into the third's start.
    #[test]
    fn a_loop_layer_seams_onto_its_pre_roll() {
        let frames = 4800;
        let made = tone(frames * 4, |i| i as f32);
        let layer = assemble(&made, frames, 2, true);
        assert_eq!(layer.frames(), frames);
        assert_eq!(layer.left[0], (2 * frames) as f32, "the third period");
        let last = *layer.left.last().unwrap();
        assert!((last - (2 * frames - 1) as f32).abs() < 40.0, "{last}");
        let one_shot = assemble(&made, 100, 0, false);
        assert_eq!(
            one_shot.left,
            (0..100).map(|i| i as f32).collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_key_follows_the_notes() {
        let block = MidiBlock::from_value(&json!({ "tracks": [{ "notes": [
            { "t": 0, "d": 2, "n": 57 }, { "t": 2, "d": 1, "n": 60 }, { "t": 3, "d": 1, "n": 64 },
            { "t": 4, "d": 2, "n": 57 }, { "t": 6, "d": 1, "n": 62 }, { "t": 7, "d": 1, "n": 65 } ] },
            { "bank": "drums", "notes": [{ "t": 0, "d": 1, "n": 36 }] }] }))
        .unwrap();
        assert_eq!(key_scale(&block).as_deref(), Some("A minor"));
        assert_eq!(key_scale(&MidiBlock::default()), None);
    }

    #[test]
    fn the_endpoint_prefers_what_it_is_given() {
        let e = Endpoint::resolve(Some("http://h:9/ "), Some(" k "));
        assert_eq!(e.url, "http://h:9");
        assert_eq!(e.key.as_deref(), Some("k"));
    }
}

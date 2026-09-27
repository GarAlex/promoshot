//! Wizard authoring: media and choices in, a complete project out.
//!
//! This is the Windows twin of macapp's `ProjectStore.setupStarterLayers`
//! (its `SlideshowDraft.timeline` is the shape's one statement, ported here
//! rule for rule) — in the core so the Windows wizard, the CLI and MCP all
//! author the SAME show instead of three implementations agreeing by hand.
//! The host stages the media (copies files, reads pixel sizes and clip
//! lengths — I/O is the host's); this arranges.
//!
//! V1 scope, stated rather than silent (the Mac behaviors not yet ported):
//! library background plates and their paired themes, narration drafts and
//! OCR fill, the animated three-layer App Store listing (this builds the
//! per-slide shape the narrated listing uses), evidence-based device-frame
//! gating (V1 frames every image slide), and the alternating-turn /
//! emphasis arrangements. Each lands here, never in a front end.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::listing;

fn default_slide_duration() -> f64 {
    3.0
}
fn default_transition_duration() -> f64 {
    0.5
}
fn default_kind() -> String {
    "classic".into()
}
fn default_transition() -> String {
    "crossfade".into()
}
fn default_direction() -> String {
    "rightToLeft".into()
}
fn default_sizing() -> String {
    "fit".into()
}
fn default_device() -> String {
    "iPhone".into()
}
fn default_framing() -> String {
    "flat".into()
}
fn default_background() -> String {
    "16213E".into()
}
fn default_orientation() -> String {
    "original".into()
}
fn default_evidence() -> String {
    "unknown".into()
}
fn default_arrangement() -> String {
    "centred".into()
}
/// A body by default for a host that says nothing: the drawn frame is the
/// app's own default (flat, legible, and its theme resolves the frame's
/// `@edge`), and the app always says which it wants; a headless project
/// has no theme, and the validator calls a frame the legacy form of a body.
fn default_body() -> String {
    "model".into()
}
fn default_angle() -> String {
    "quarterLeft".into()
}
fn default_change() -> String {
    "screen".into()
}
fn default_provider() -> String {
    "openai".into()
}
fn default_voice() -> String {
    "alloy".into()
}

/// A picture may stand in a device frame unless the bytes say a camera
/// took it — a screenshot re-exported from a design tool has neither
/// provenance nor EXIF, and is still a screenshot. A recorded origin of a
/// capture settles it.
fn may_wear_a_device_frame(slide: &AuthorSlide) -> bool {
    match slide.origin.as_deref() {
        Some("screenRecording") | Some("screenshot") => true,
        _ => slide.evidence != "camera",
    }
}

/// The ramp a keyframe the wizard states carries when nothing moves into
/// it — the app's keyframe default, so both wizards write one file.
const KEYFRAME_RAMP: f64 = 0.5;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorSlide {
    /// Already staged into the project's Resources/ by the host.
    pub filename: String,
    /// The id the host already gave the staged resource — the apps stage
    /// before they author; absent, the wizard mints one.
    #[serde(default)]
    pub resource_id: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    /// "image" | "video"
    pub kind: String,
    #[serde(default)]
    pub pixel_width: Option<f64>,
    #[serde(default)]
    pub pixel_height: Option<f64>,
    /// Seconds on screen. A clip's is its file length — settled by the
    /// host at staging, because the file is the answer.
    #[serde(default = "default_slide_duration")]
    pub duration: f64,
    /// How long the NEXT slide takes to arrive over this one.
    #[serde(default = "default_transition_duration")]
    pub transition_duration: f64,
    #[serde(default)]
    pub caption: String,
    #[serde(default)]
    pub looped: bool,
    /// How the picture is turned before it is shown (`original`,
    /// `rotated90`, …) — the orientation the host read at staging.
    #[serde(default = "default_orientation")]
    pub image_orientation: String,
    /// What the bytes say about where the picture came from, read at
    /// staging: `camera` keeps it out of a device frame; `screenshot` and
    /// `unknown` do not (absence of evidence is not evidence of absence).
    #[serde(default = "default_evidence")]
    pub evidence: String,
    /// The resource's recorded origin, when the host knows one
    /// (`screenshot`, `screenRecording`, `imported`).
    #[serde(default)]
    pub origin: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorSpec {
    pub name: String,
    /// "classic" | "carousel" | "appStore"
    #[serde(default = "default_kind")]
    pub kind: String,
    /// "none" | "crossfade" | "wipe" | "slide" | "push" | "scale"
    #[serde(default = "default_transition")]
    pub transition: String,
    /// "left" | "right" | "top" | "bottom"; absent = the kind's default.
    #[serde(default)]
    pub transition_edge: Option<String>,
    /// Carousel: "rightToLeft" | "leftToRight"
    #[serde(default = "default_direction")]
    pub direction: String,
    /// "fit" | "fill"
    #[serde(default = "default_sizing")]
    pub sizing: String,
    /// App Store: "iPhone" | "iPad" | "mac"
    #[serde(default = "default_device")]
    pub device: String,
    /// App Store: "flat" | "angled"
    #[serde(default = "default_framing")]
    pub framing: String,
    #[serde(default)]
    pub canvas_width: Option<f64>,
    #[serde(default)]
    pub canvas_height: Option<f64>,
    #[serde(default = "default_background")]
    pub background_color_hex: String,
    /// App Store: "centred" | "sides" | "stacked" — how the shots sit
    /// across the frame, one pattern for the set.
    #[serde(default = "default_arrangement")]
    pub arrangement: String,
    /// App Store: turn the device a little further each shot, alternating.
    #[serde(default)]
    pub turns: bool,
    /// App Store: the finish the slab or the body wears; absent is the
    /// device's own.
    #[serde(default)]
    pub material: Option<String>,
    /// App Store: "model" — a 3D body with the shot on its screen, the
    /// default — or "slab", the drawn 2.5D frame the app offers.
    #[serde(default = "default_body")]
    pub body: String,
    /// App Store 3D: the camera preset ("straight", "quarterLeft",
    /// "quarterRight", "hero", "above").
    #[serde(default = "default_angle")]
    pub angle: String,
    /// App Store 3D: what changes between shots — "screen" (one body, the
    /// shots a reel on its screen) or "device" (a body per shot).
    #[serde(default = "default_change")]
    pub change: String,
    /// App Store 3D: the device model the host linked from its library, as
    /// a resource. Absent, the body is the engine's own device recipe.
    #[serde(default)]
    pub device_model: Option<Value>,
    /// Give every slide a DRAFT narration: an audio resource with an empty
    /// speech script and a layer over the slide's window, timed by rules
    /// so a sound's length re-times the show when it lands.
    #[serde(default)]
    pub narration: bool,
    /// The narration drafts' provider and voice — the person's defaults,
    /// which only the host knows.
    #[serde(default = "default_provider")]
    pub narration_provider: String,
    #[serde(default = "default_voice")]
    pub narration_voice: String,
    /// Unix seconds; the host's clock, passed in so authoring stays pure.
    pub created_at: f64,
    pub slides: Vec<AuthorSlide>,
}

/// Deterministic per document: unique is the requirement, and reproducible
/// authoring is a property tests can hold onto. Shaped like the UUIDs the
/// rest of the format carries.
struct Ids {
    stamp: u64,
    next: u32,
}

impl Ids {
    fn new(created_at: f64) -> Self {
        Self {
            stamp: (created_at * 1000.0).abs() as u64,
            next: 0,
        }
    }
    fn take(&mut self) -> String {
        self.next += 1;
        format!(
            "{:08X}-{:04X}-4{:03X}-8{:03X}-{:012X}",
            self.next,
            (self.stamp >> 48) as u16,
            ((self.stamp >> 36) & 0xFFF) as u16,
            ((self.stamp >> 24) & 0xFFF) as u16,
            self.stamp & 0xFF_FFFF_FFFF
        )
    }
}

fn transition_kind(transition: &str) -> Option<&'static str> {
    match transition {
        "crossfade" => Some("fade"),
        "wipe" => Some("wipe"),
        "slide" => Some("slide"),
        "push" => Some("push"),
        "scale" => Some("scale"),
        // Rung 25's kinds: they ride the image effects, and the app's
        // wizard offers every one.
        "blurDissolve" => Some("blurDissolve"),
        "zoom" => Some("zoom"),
        "flash" => Some("flash"),
        "glitch" => Some("glitch"),
        "dip" => Some("dip"),
        _ => None,
    }
}

fn uses_edge(kind: &str) -> bool {
    matches!(kind, "wipe" | "slide" | "push")
}

/// The kind's own default edge — the same answers `LayerTransition::edge`
/// resolves to, so an absent choice here and an absent field there agree.
fn default_edge(kind: &str) -> &'static str {
    match kind {
        "slide" => "bottom",
        "push" => "right",
        _ => "left",
    }
}

fn opposite(edge: &str) -> &'static str {
    match edge {
        "right" => "left",
        "top" => "bottom",
        "bottom" => "top",
        _ => "right",
    }
}

/// How long slide `index` takes to arrive over the one before it: zero for
/// the first and for hard cuts, else the outgoing slide's ramp clamped to
/// both slides' own lengths.
fn crossfade(spec: &AuthorSpec, index: usize) -> f64 {
    let slides: Vec<&AuthorSlide> = spec.slides.iter().collect();
    crossfade_over(spec, &slides, index)
}

fn crossfade_over(spec: &AuthorSpec, slides: &[&AuthorSlide], index: usize) -> f64 {
    if spec.transition == "none" || index == 0 || index >= slides.len() {
        return 0.0;
    }
    let outgoing = slides[index - 1];
    outgoing
        .transition_duration
        .min(outgoing.duration)
        .min(slides[index].duration)
        .max(0.0)
}

/// What the slide at `index` does as the next arrives: only a push moves
/// it, leaving by the far side — the shove.
fn exit_transition(spec: &AuthorSpec, slides: &[&AuthorSlide], index: usize) -> Option<Value> {
    if spec.transition != "push" || index + 1 >= slides.len() {
        return None;
    }
    let seconds = crossfade_over(spec, slides, index + 1);
    if seconds <= 0.0 {
        return None;
    }
    let edge = spec
        .transition_edge
        .as_deref()
        .unwrap_or_else(|| default_edge("push"));
    Some(json!({"kind": "push", "from": opposite(edge), "duration": seconds}))
}

/// Where each slide of a classic show sits: start, span, and the
/// transitions it arrives and leaves with — SlideshowDraft.timeline.
fn classic_timeline(
    spec: &AuthorSpec,
    slides: &[&AuthorSlide],
) -> Vec<(f64, f64, Option<Value>, Option<Value>)> {
    let mut cursor = 0.0f64;
    let mut out = Vec::with_capacity(slides.len());
    for (index, slide) in slides.iter().enumerate() {
        let overlap = crossfade_over(spec, slides, index);
        out.push((
            (cursor - overlap).max(0.0),
            slide.duration + overlap.min(cursor),
            entry_transition(spec, slides, index),
            exit_transition(spec, slides, index),
        ));
        cursor += slide.duration;
    }
    out
}

/// A layer's edge transitions as the classic show writes them: a plain
/// dissolve as the one-number shorthand, anything else whole.
fn dress_transitions(layer: &mut Value, entry: &Option<Value>, exit: &Option<Value>) {
    if let Some(entry) = entry {
        if entry["kind"] == "fade" && entry.get("from").is_none() {
            layer["fadeIn"] = entry["duration"].clone();
        } else {
            layer["transitionIn"] = entry.clone();
        }
    }
    if let Some(exit) = exit {
        layer["transitionOut"] = exit.clone();
    }
}

/// The transition the slide at `index` of `slides` arrives with: nil for
/// the first and for hard cuts; an edge only for the kinds that have one.
fn entry_transition(spec: &AuthorSpec, slides: &[&AuthorSlide], index: usize) -> Option<Value> {
    let kind = transition_kind(&spec.transition)?;
    if index == 0 {
        return None;
    }
    let seconds = crossfade_over(spec, slides, index);
    if seconds <= 0.0 {
        return None;
    }
    let mut transition = json!({"kind": kind, "duration": seconds});
    if uses_edge(kind) {
        transition["from"] = json!(spec
            .transition_edge
            .as_deref()
            .unwrap_or_else(|| default_edge(kind)));
    }
    Some(transition)
}

/// The wizard's caption typography. A store shot's headline sits in the
/// band above the device (a top margin) — its size from the chosen canvas,
/// its shadow from the device's own, as the app's wizard writes it; every
/// other show's caption is a lower third — anchored to the bottom by rule,
/// lifted clear of the edge, stroked so it reads over any picture — and
/// re-resolves if the canvas is re-stamped for another size.
fn caption_style(app_store: bool, device: &listing::Device, canvas_w: f64, canvas_h: f64) -> Value {
    if app_store {
        let font = device.caption_font_size(canvas_w);
        let shadow = device.caption_font_size(device.canvas.0);
        return json!({
            "fontSize": font,
            "isBold": true,
            "alignment": "center",
            "backgroundOpacity": 0.0,
            "shadowOpacity": 0.35,
            "shadowRadius": shadow * 0.22,
            "shadowOffset": [0.0, shadow * 0.08],
            "verticalMargin": canvas_h * 0.06,
            "leftMargin": canvas_w * 0.08,
            "rightMargin": canvas_w * 0.08,
        });
    }
    let font = canvas_h * 0.055;
    json!({
        "fontSize": font,
        "isBold": true,
        "alignment": "center",
        "backgroundOpacity": 0.0,
        "shadowOpacity": 0.35,
        "shadowRadius": font * 0.22,
        "shadowOffset": [0.0, font * 0.08],
        "leftMargin": canvas_w * 0.08,
        "rightMargin": canvas_w * 0.08,
        "placement": {"anchor": "bottom", "offset": [0.0, -(canvas_h * 0.07)]},
        "strokeWidth": font * 0.08,
        "strokeColorHex": "000000",
        "textColorHex": "FFFFFF",
    })
}

/// The carousel's flight time: short shows shorten it rather than spending
/// their whole length in motion.
fn carousel_ramp(duration: f64) -> f64 {
    (duration * 0.3).clamp(0.15, 0.9)
}

/// Authors the project and returns its metadata as canonical JSON — round
/// tripped through the model, so what leaves here is exactly what every
/// reader will see.
pub fn author(spec: &AuthorSpec) -> Result<String, String> {
    if spec.slides.is_empty() {
        return Err("a show needs at least one slide".into());
    }
    let mut ids = Ids::new(spec.created_at);
    let app_store = spec.kind == "appStore";
    let carousel = spec.kind == "carousel";
    let device = listing::device(&spec.device);

    // A store listing is sized by the store, so the canvas is the FIRST
    // thing the choice decides; any show may name its own.
    let (canvas_w, canvas_h) = match (spec.canvas_width, spec.canvas_height) {
        (Some(w), Some(h)) => (w, h),
        _ if app_store => device.canvas,
        _ => (1920.0, 1080.0),
    };
    let canvas = (canvas_w, canvas_h);

    // The listing: three layers, the shots swapping on one device. A
    // narrated listing keeps the per-slide shape — a draft narration hangs
    // inside its slide's layer, and swap times cannot follow the takes.
    if app_store && !spec.narration {
        if let Some((resources, layers, span)) = listing_show(spec, &mut ids, canvas, &device) {
            return document(spec, &mut ids, canvas, resources, layers, span, false);
        }
    }

    // Where every slide sits — the one statement of the show's shape,
    // ported from SlideshowDraft.timeline. Classic lays slides end to end
    // less each arrival's overlap; a carousel overlaps by one ramp because
    // the whole effect is the outgoing card leaving while the incoming one
    // arrives.
    let n = spec.slides.len();
    let mut starts = Vec::with_capacity(n);
    let mut spans = Vec::with_capacity(n);
    if carousel {
        let mut cursor = 0.0f64;
        for slide in &spec.slides {
            starts.push(cursor);
            spans.push(slide.duration);
            cursor += slide.duration - carousel_ramp(slide.duration);
        }
    } else {
        let mut cursor = 0.0f64;
        for (i, slide) in spec.slides.iter().enumerate() {
            let overlap = crossfade(spec, i);
            starts.push((cursor - overlap).max(0.0));
            spans.push(slide.duration + overlap.min(cursor));
            cursor += slide.duration;
        }
    }
    let total = if carousel {
        starts[n - 1] + spans[n - 1]
    } else {
        spec.slides.iter().map(|s| s.duration).sum::<f64>()
    }
    .max(0.1);

    let slab_json = device.slab(&spec.framing, spec.material.as_deref());
    let slab: Option<promo_model::ResourceFrame> = serde_json::from_value(slab_json.clone()).ok();
    let all: Vec<&AuthorSlide> = spec.slides.iter().collect();

    let mut resources: Vec<Value> = Vec::new();
    let mut layers: Vec<Value> = Vec::new();
    let mut sort = 0i64;

    layers.push(json!({
        "id": ids.take(),
        "name": "Background",
        "sortIndex": sort,
        "kind": "background",
        "isEnabled": true,
        "startTime": 0.0,
        "duration": total,
        "keyframes": [{"id": ids.take(), "time": 0.0, "transitionDuration": KEYFRAME_RAMP,
                       "colorHex": spec.background_color_hex}],
    }));
    sort += 1;

    // Which caption layer belongs to which shot, recorded rather than
    // inferred from where it sits — the narration rebuild reorders them.
    let mut caption_owner: Vec<(String, String)> = Vec::new();
    let mut slide_resources: Vec<String> = Vec::new();

    for (i, slide) in spec.slides.iter().enumerate() {
        let is_video = slide.kind == "video";
        let resource_id = slide.resource_id.clone().unwrap_or_else(|| ids.take());
        let mut resource = slide_resource(spec, slide, &resource_id);
        if app_store && !is_video && may_wear_a_device_frame(slide) {
            resource["frame"] = slab_json.clone();
        }
        resources.push(resource);
        slide_resources.push(resource_id.clone());

        // Entry/exit transitions (classic + appStore; carousel states its
        // motion in keyframes and takes none). A plain dissolve collapses
        // to the fadeIn shorthand it has always been.
        let mut fade_in: Option<f64> = None;
        let mut transition_in: Option<Value> = None;
        let mut transition_out: Option<Value> = None;
        if !carousel {
            if let Some(entry) = entry_transition(spec, &all, i) {
                if entry["kind"] == "fade" {
                    fade_in = entry["duration"].as_f64();
                } else {
                    transition_in = Some(entry);
                }
            }
            // Only a push moves what it replaces; it leaves by the far
            // side under its own steam, which is what reads as the shove.
            if spec.transition == "push" && i + 1 < n {
                let seconds = crossfade(spec, i + 1);
                if seconds > 0.0 {
                    let edge = spec
                        .transition_edge
                        .as_deref()
                        .unwrap_or_else(|| default_edge("push"));
                    transition_out = Some(json!({
                        "kind": "push",
                        "from": opposite(edge),
                        "duration": seconds,
                    }));
                }
            }
        }

        let keyframes = if carousel {
            carousel_keyframes(
                &mut ids,
                slide.duration,
                canvas_w,
                canvas_h,
                spec.direction == "rightToLeft",
                i == 0,
                i == n - 1,
            )
        } else if app_store {
            // Sized against the FRAMED picture, bezel included — the box
            // the slab makes, not the bare screenshot.
            let aspect = match (slide.pixel_width, slide.pixel_height) {
                (Some(w), Some(h)) if w > 0.0 && h > 0.0 => {
                    let shown = if is_video {
                        promo_model::Size::new(w, h)
                    } else {
                        promo_timeline::framed_pixel_size(
                            promo_model::Size::new(w, h),
                            slab.as_ref(),
                        )
                    };
                    if shown.height() > 0.0 {
                        shown.width() / shown.height()
                    } else {
                        1.0
                    }
                }
                _ => 1.0,
            };
            vec![json!({
                "id": ids.take(),
                "time": 0.0,
                "transitionDuration": KEYFRAME_RAMP,
                "placement": device.shot_placement(aspect, canvas),
            })]
        } else {
            vec![json!({
                "id": ids.take(),
                "time": 0.0,
                "transitionDuration": KEYFRAME_RAMP,
                "placement": {"mode": spec.sizing, "anchor": "center"},
            })]
        };

        let mut layer = json!({
            "id": ids.take(),
            "name": slide.display_name.clone().unwrap_or_else(|| slide.filename.clone()),
            "sortIndex": sort,
            "kind": if is_video { "video" } else { "image" },
            "isEnabled": true,
            "startTime": starts[i],
            "duration": spans[i],
            "resourceID": resource_id,
            "keyframes": keyframes,
        });
        if let Some(seconds) = fade_in {
            layer["fadeIn"] = json!(seconds);
        }
        if let Some(t) = &transition_in {
            layer["transitionIn"] = t.clone();
        }
        if let Some(t) = &transition_out {
            layer["transitionOut"] = t.clone();
        }
        if is_video && slide.looped {
            layer["beyondEnd"] = json!("loop");
        }
        if !is_video {
            layer["imageOrientation"] = json!(slide.image_orientation);
        }
        layers.push(layer);
        sort += 1;

        // A styled caption per slide: layout, typography and shadow are
        // what a wizard can decide; the words are what only the author
        // can. A store shot always carries its headline band, text left as
        // typed (empty is fine); a classic or carousel slide takes a lower
        // third only when words were given (issue #7). Colours stay
        // UNSTATED so a theme can move them later.
        if app_store || !slide.caption.is_empty() {
            let caption_id = ids.take();
            resources.push(json!({
                "id": caption_id,
                "kind": "caption",
                "filename": "",
                "displayName": if slide.caption.is_empty() {
                    format!("Caption {}", i + 1)
                } else {
                    slide.caption.chars().take(40).collect::<String>()
                },
                "addedAt": spec.created_at,
                "captionText": slide.caption,
                "captionStyle": caption_style(app_store, &device, canvas_w, canvas_h),
            }));
            let layer_id = ids.take();
            let mut caption_layer = json!({
                "id": layer_id,
                "name": format!("Caption {}", i + 1),
                "sortIndex": sort,
                "kind": "caption",
                "isEnabled": true,
                "startTime": starts[i],
                "duration": spans[i],
                "resourceID": caption_id,
                "keyframes": [],
            });
            // A slide is ONE thing: the words leave with the picture.
            if let Some(seconds) = fade_in {
                caption_layer["fadeIn"] = json!(seconds);
            }
            if let Some(t) = &transition_in {
                caption_layer["transitionIn"] = t.clone();
            }
            if let Some(t) = &transition_out {
                caption_layer["transitionOut"] = t.clone();
            }
            caption_owner.push((layer_id, resource_id.clone()));
            layers.push(caption_layer);
            sort += 1;
        }
    }

    // Narration builds the show ON THE FORMAT — anchors for the chain,
    // duration RULES for the waiting — with no chain arithmetic here:
    //   slide_i     durationRule fitDependents(tail 2.5)
    //   audio_i     start = previousStart + 0.4, durationRule fitContent
    //   slide_i+1   start = previousPeerEnd − fade
    // When a sound lands, its length reaches the audio resource and the
    // next resolution re-times the whole show.
    if spec.narration {
        let mut rebuilt = vec![layers[0].clone()];
        let mut sort = 1i64;
        for (index, resource_id) in slide_resources.iter().enumerate() {
            let fade = crossfade(spec, index);
            let Some(mut picture) = layers
                .iter()
                .find(|l| {
                    (l["kind"] == "image" || l["kind"] == "video")
                        && l["resourceID"].as_str() == Some(resource_id)
                })
                .cloned()
            else {
                continue;
            };
            picture["sortIndex"] = json!(sort);
            picture["durationRule"] = json!({"kind": "fitDependents", "tail": 2.5});
            if index > 0 {
                picture["timing"] = json!({"start": {"from": "previousPeerEnd", "offset": -fade}});
            }
            let start = picture["startTime"].as_f64().unwrap_or(0.0);
            rebuilt.push(picture);
            sort += 1;
            for (caption_layer, owner) in &caption_owner {
                if owner != resource_id {
                    continue;
                }
                if let Some(mut attached) = layers
                    .iter()
                    .find(|l| l["id"].as_str() == Some(caption_layer))
                    .cloned()
                {
                    attached["sortIndex"] = json!(sort);
                    rebuilt.push(attached);
                    sort += 1;
                }
            }
            let audio_id = ids.take();
            resources.push(json!({
                "id": audio_id,
                "kind": "audio",
                "filename": "",
                "displayName": format!("Narration {}", index + 1),
                "addedAt": spec.created_at,
                "speech": {
                    "text": "",
                    "provider": spec.narration_provider,
                    "voiceID": spec.narration_voice,
                    "sourceResourceID": resource_id,
                },
            }));
            // Duration 1.0 is a DRAFT placeholder: fitContent replaces it
            // the moment the sound exists.
            rebuilt.push(json!({
                "id": ids.take(),
                "name": format!("Narration {}", index + 1),
                "sortIndex": sort,
                "kind": "audio",
                "isEnabled": true,
                "startTime": start + 0.4,
                "duration": 1.0,
                "resourceID": audio_id,
                "keyframes": [],
                "durationRule": {"kind": "fitContent"},
                "timing": {"start": {"from": "previousStart", "offset": 0.4}},
            }));
            sort += 1;
        }
        layers = rebuilt;
    }

    document(
        spec,
        &mut ids,
        canvas,
        resources,
        layers,
        total,
        spec.narration,
    )
}

/// One slide's resource, as the host staged it.
fn slide_resource(spec: &AuthorSpec, slide: &AuthorSlide, id: &str) -> Value {
    let is_video = slide.kind == "video";
    let mut resource = json!({
        "id": id,
        "kind": if is_video { "video" } else { "image" },
        "filename": slide.filename,
        "displayName": slide.display_name.clone().unwrap_or_else(|| slide.filename.clone()),
        "addedAt": spec.created_at,
    });
    if is_video {
        resource["duration"] = json!(slide.duration);
    }
    if let (Some(w), Some(h)) = (slide.pixel_width, slide.pixel_height) {
        resource["pixelWidth"] = json!(w);
        resource["pixelHeight"] = json!(h);
    }
    resource
}

/// The store listing's three layers and their resources: the framable
/// shots on one device swapping, their headlines on one caption layer
/// swapping. None when no shot may wear the frame — the show is then
/// built slide by slide.
fn listing_show(
    spec: &AuthorSpec,
    ids: &mut Ids,
    canvas: (f64, f64),
    device: &listing::Device,
) -> Option<(Vec<Value>, Vec<Value>, f64)> {
    let slab_json = device.slab(&spec.framing, spec.material.as_deref());
    let slab: promo_model::ResourceFrame = serde_json::from_value(slab_json.clone()).ok()?;
    let mut resources = Vec::new();
    let mut shots: Vec<listing::Shot> = Vec::new();
    let mut shot_slides: Vec<&AuthorSlide> = Vec::new();
    // Ids in the order the per-slide path takes them, so a show that falls
    // back reads the same; the listing's own come after.
    let staged: Vec<String> = spec
        .slides
        .iter()
        .map(|slide| slide.resource_id.clone().unwrap_or_else(|| ids.take()))
        .collect();
    for (slide, id) in spec.slides.iter().zip(&staged) {
        let mut resource = slide_resource(spec, slide, id);
        // A PICTURE is required, not just permission to frame: the swap
        // layer is an image layer, and a swap to a resource of another
        // kind is ignored.
        let framable = slide.kind != "video" && may_wear_a_device_frame(slide);
        if framable {
            resource["frame"] = slab_json.clone();
            shots.push(listing::Shot {
                resource_id: id.clone(),
                content: slide.pixel_width.zip(slide.pixel_height),
                frame: Some(slab.clone()),
                headline: slide.caption.clone(),
                emphasis: listing::Emphasis::at(&spec.arrangement, shots.len()),
                // A slab peeks in from the edge, a third off-canvas; a BODY
                // cut by the edge reads as a cropped render, so the 3D form
                // keeps the whole device in frame, arranged by the box the
                // body makes at its camera.
                hiding: if spec.body == "model" { 0.0 } else { 1.0 / 3.0 },
                box_aspect: (spec.body == "model")
                    .then(|| listing::box_aspect(&spec.device, &spec.angle)),
            });
            shot_slides.push(slide);
        }
        resources.push(resource);
    }
    if shots.is_empty() {
        return None;
    }
    let words = device.caption_font_size(canvas.0);
    let look = json!({
        "isBold": true,
        "shadowOpacity": 0.35,
        "shadowRadius": words * 0.22,
        "shadowOffset": [0.0, words * 0.08],
    });
    let area = device.shot_area(canvas);
    let seconds = spec
        .slides
        .first()
        .map(|s| s.duration)
        .unwrap_or(3.0)
        .max(0.5);
    // The wizard's transition, as the classic show reads it: a listing is
    // a classic show whose slides happen to be framed. A one-shot listing
    // has no arrival to speak of.
    let arrival = if shot_slides.len() > 1 {
        entry_transition(spec, &shot_slides, 1)
    } else {
        None
    };
    let built = listing::build(
        &shots,
        canvas,
        area.1,
        area,
        (0.0, canvas.1 * device.band / 2.0),
        seconds,
        arrival,
        spec.turns.then_some(12.0),
        words,
        canvas.0 * 0.08,
        &look,
    )?;
    let caption_ids: Vec<String> = built.captions.iter().map(|_| ids.take()).collect();
    for (caption, id) in built.captions.iter().zip(&caption_ids) {
        resources.push(json!({
            "id": id,
            "kind": "caption",
            "filename": "",
            "displayName": caption.display_name,
            "addedAt": spec.created_at,
            "captionText": caption.text,
            "captionStyle": caption.style,
        }));
    }
    let background = json!({
        "id": ids.take(),
        "name": "Background",
        "sortIndex": 0,
        "kind": "background",
        "isEnabled": true,
        "startTime": 0.0,
        "duration": built.span,
        "keyframes": [{"id": ids.take(), "time": 0.0, "transitionDuration": KEYFRAME_RAMP,
                       "colorHex": spec.background_color_hex}],
    });
    let device_keys: Vec<Value> = built
        .device_keys
        .into_iter()
        .map(|mut key| {
            key["id"] = json!(ids.take());
            key
        })
        .collect();
    let device_layer = json!({
        "id": ids.take(),
        "name": "Device",
        "sortIndex": 1,
        "kind": "image",
        "isEnabled": true,
        "startTime": 0.0,
        "duration": built.span,
        "resourceID": shots[0].resource_id,
        "keyframes": device_keys,
    });
    let caption_keys: Vec<Value> = built
        .caption_keys
        .iter()
        .map(|(time, index)| {
            let mut key = json!({"id": ids.take(), "time": time, "transitionDuration": 0.0});
            if *index > 0 {
                key["resourceID"] = json!(caption_ids[*index]);
            }
            key
        })
        .collect();
    let headline = json!({
        "id": ids.take(),
        "name": "Headline",
        "sortIndex": 2,
        "kind": "caption",
        "isEnabled": true,
        "startTime": 0.0,
        "duration": built.span,
        "resourceID": caption_ids[0],
        "keyframes": caption_keys,
    });
    let layers = vec![background, device_layer, headline];
    if spec.body == "model" {
        let shot_ids: Vec<String> = shots.iter().map(|s| s.resource_id.clone()).collect();
        return Some(if spec.change == "device" {
            three_d_by_device(
                spec,
                ids,
                resources,
                layers,
                &shot_ids,
                &shot_slides,
                built.span,
            )
        } else {
            three_d_screen_reel(
                spec,
                ids,
                canvas,
                resources,
                layers,
                &shots,
                &shot_slides,
                built.span,
            )
        });
    }
    Some((resources, layers, built.span))
}

/// The device a 3D listing stands: the model the host linked from its
/// library, or — a headless host has none — the engine's own recipe.
fn linked_device(spec: &AuthorSpec, id: String) -> Value {
    let mut body = spec.device_model.clone().unwrap_or_else(|| {
        json!({
            "kind": "model",
            "filename": "",
            "displayName": listing::label(&spec.device),
            "recipe": {"device": {"kind": listing::recipe_kind(&spec.device)}},
        })
    });
    body["id"] = json!(id);
    body["addedAt"] = json!(spec.created_at);
    body
}

/// A body's slots: the screen is glass over what it shows, the metal is
/// what the finish IS, by word (rung 44).
fn device_materials(spec: &AuthorSpec, screen: &str) -> Value {
    let material = spec
        .material
        .clone()
        .unwrap_or_else(|| listing::device(&spec.device).material.to_string());
    let (hex, word) = listing::finish(&material);
    let mut slots = json!({"Screen": {"resourceID": screen, "finish": "glass"}});
    for slot in listing::FINISHED_SLOTS {
        slots[slot] = json!({"colorHex": hex, "finish": word});
    }
    slots
}

/// The shots lose the slab they were dressed with: a shot that is the
/// picture ON a body has no business carrying a drawn body of its own.
fn unframe(resources: &mut [Value], shots: &[String]) {
    for resource in resources.iter_mut() {
        if resource["id"]
            .as_str()
            .is_some_and(|id| shots.iter().any(|s| s == id))
        {
            if let Some(object) = resource.as_object_mut() {
                object.remove("frame");
            }
        }
    }
}

/// The 3D listing where the DEVICE changes: a body per shot, each its own
/// copy of the device with that shot on its Screen, placed where the swap
/// layer would have put the shot and timed by the classic show's rule —
/// so the wizard's transition moves the body. The words change where each
/// body has arrived.
fn three_d_by_device(
    spec: &AuthorSpec,
    ids: &mut Ids,
    mut resources: Vec<Value>,
    layers: Vec<Value>,
    shots: &[String],
    shot_slides: &[&AuthorSlide],
    span: f64,
) -> (Vec<Value>, Vec<Value>, f64) {
    unframe(&mut resources, shots);
    let placements = classic_timeline(spec, shot_slides);
    let swap = layers[1].clone();
    let schedule = swap["keyframes"].as_array().cloned().unwrap_or_default();
    let label = listing::label(&spec.device);
    let mut out: Vec<Value> = layers
        .iter()
        .filter(|l| l["kind"] != "image")
        .cloned()
        .collect();
    let mut sort = swap["sortIndex"].as_i64().unwrap_or(1);
    for (index, shot) in shots.iter().enumerate() {
        let body_id = ids.take();
        let mut body = linked_device(spec, body_id.clone());
        body["displayName"] = json!(format!("{label} {}", index + 1));
        body["materials"] = device_materials(spec, shot);
        resources.push(body);
        // The light is the PRESET's, ahead of its camera; the alternating
        // turn moves the camera alone.
        let preset = listing::angle_camera(&spec.angle);
        let light = listing::angle_light(&preset);
        let mut camera = preset;
        if spec.turns {
            let turn = if index % 2 == 0 { -12.0 } else { 12.0 };
            camera["yaw"] = json!(camera["yaw"].as_f64().unwrap_or(0.0) + turn);
        }
        let (start, span_of, entry, exit) = &placements[index];
        let mut layer = json!({
            "id": ids.take(),
            "name": format!("{label} {}", index + 1),
            "sortIndex": sort,
            "kind": "model",
            "isEnabled": true,
            "startTime": swap["startTime"].as_f64().unwrap_or(0.0) + start,
            "duration": span_of.max(0.1),
            "resourceID": body_id,
            "keyframes": [{
                "id": ids.take(), "time": 0.0, "transitionDuration": KEYFRAME_RAMP,
                "placement": schedule.get(index).map(|k| k["placement"].clone()).unwrap_or(Value::Null),
                "camera": camera, "light": light,
            }],
        });
        dress_transitions(&mut layer, entry, exit);
        out.push(layer);
        sort += 1;
    }
    if let Some(words) = out.iter_mut().find(|l| l["kind"] == "caption") {
        words["sortIndex"] = json!(sort);
        if let Some(keys) = words["keyframes"].as_array_mut() {
            for (index, key) in keys.iter_mut().enumerate() {
                if let Some((start, _, entry, _)) = placements.get(index) {
                    let arrival = entry
                        .as_ref()
                        .and_then(|e| e["duration"].as_f64())
                        .unwrap_or(0.0);
                    key["time"] = json!(start + arrival);
                }
            }
        }
    }
    out.sort_by_key(|l| l["sortIndex"].as_i64().unwrap_or(0));
    (resources, out, span)
}

/// The 3D listing where the SCREEN changes: one body for the whole
/// listing, moving as the slab would have, its Screen bound to a reel — a
/// composition of the shots laid out by the classic show's rule, fitted on
/// a canvas the screen's own shape — so the transition happens on the
/// screen while the body stays.
#[allow(clippy::too_many_arguments)]
fn three_d_screen_reel(
    spec: &AuthorSpec,
    ids: &mut Ids,
    canvas: (f64, f64),
    mut resources: Vec<Value>,
    layers: Vec<Value>,
    shots: &[listing::Shot],
    shot_slides: &[&AuthorSlide],
    span: f64,
) -> (Vec<Value>, Vec<Value>, f64) {
    let shot_ids: Vec<String> = shots.iter().map(|s| s.resource_id.clone()).collect();
    unframe(&mut resources, &shot_ids);
    let placements = classic_timeline(spec, shot_slides);
    let reel_canvas = listing::screen_canvas(&spec.device, canvas.1);
    let swap = layers[1].clone();
    let frames: Vec<Value> = shots
        .iter()
        .enumerate()
        .map(|(index, shot)| {
            let (start, span_of, entry, exit) = &placements[index];
            let name = if shot.headline.is_empty() {
                shot_slides[index]
                    .display_name
                    .clone()
                    .unwrap_or_else(|| shot_slides[index].filename.clone())
            } else {
                shot.headline.clone()
            };
            let mut frame = json!({
                "id": ids.take(),
                "name": name,
                "sortIndex": index,
                "kind": "image",
                "isEnabled": true,
                "startTime": start,
                "duration": span_of.max(0.1),
                "resourceID": shot.resource_id,
                "keyframes": [{"id": ids.take(), "time": 0.0, "transitionDuration": KEYFRAME_RAMP,
                               "placement": {"mode": "fit", "anchor": "center"}}],
            });
            dress_transitions(&mut frame, entry, exit);
            frame
        })
        .collect();
    let reel_span =
        swap["startTime"].as_f64().unwrap_or(0.0) + swap["duration"].as_f64().unwrap_or(0.0);
    let reel_id = ids.take();
    resources.push(json!({
        "id": reel_id,
        "kind": "composition",
        "filename": "",
        "displayName": "Screen reel",
        "addedAt": spec.created_at,
        "composition": {
            "canvasWidth": reel_canvas.0,
            "canvasHeight": reel_canvas.1,
            "backgroundColorHex": "000000",
            "layers": frames,
        },
        "pixelWidth": reel_canvas.0,
        "pixelHeight": reel_canvas.1,
        "duration": reel_span,
    }));
    let body_id = ids.take();
    let mut body = linked_device(spec, body_id.clone());
    body["displayName"] = json!(listing::label(&spec.device));
    body["materials"] = device_materials(spec, &reel_id);
    resources.push(body);
    let keys: Vec<Value> = swap["keyframes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(index, key)| {
            let preset = listing::angle_camera(&spec.angle);
            let light = listing::angle_light(&preset);
            let mut camera = preset;
            if spec.turns {
                let turn = if index % 2 == 0 { -12.0 } else { 12.0 };
                camera["yaw"] = json!(camera["yaw"].as_f64().unwrap_or(0.0) + turn);
            }
            json!({
                "id": ids.take(),
                "time": key["time"],
                "transitionDuration": key["transitionDuration"],
                "placement": key["placement"],
                "camera": camera,
                "light": light,
            })
        })
        .collect();
    let mut out: Vec<Value> = layers
        .iter()
        .filter(|l| l["kind"] != "image")
        .cloned()
        .collect();
    out.push(json!({
        "id": ids.take(),
        "name": listing::label(&spec.device),
        "sortIndex": swap["sortIndex"],
        "kind": "model",
        "isEnabled": true,
        "startTime": swap["startTime"],
        "duration": swap["duration"],
        "resourceID": body_id,
        "keyframes": keys,
    }));
    out.sort_by_key(|l| l["sortIndex"].as_i64().unwrap_or(0));
    (resources, out, span)
}

/// The project around the layers: canvas, length, the reader version its
/// features need — round-tripped through the model, so what leaves here
/// is canonical and an authoring bug fails HERE, loudly. With rules in it
/// (narration), resolved once, so the stored numbers already agree with
/// the resolver before anything reads them.
fn document(
    spec: &AuthorSpec,
    ids: &mut Ids,
    (canvas_w, canvas_h): (f64, f64),
    resources: Vec<Value>,
    layers: Vec<Value>,
    total: f64,
    resolve: bool,
) -> Result<String, String> {
    let document = json!({
        "id": ids.take(),
        "name": spec.name,
        "createdAt": spec.created_at,
        "state": "recorded",
        "trimStart": 0.0,
        "trimEnd": total,
        "videoDuration": total,
        "subtitles": [],
        "compositionSettings": {
            "canvasWidth": canvas_w,
            "canvasHeight": canvas_h,
            "backgroundColorHex": spec.background_color_hex,
            // Stated: absent, the reader migrates the colour into a legacy
            // settings keyframe the app's wizard never writes — the ground
            // is the background LAYER's keyframe.
            "backgroundKeyframes": [],
        },
        "resources": resources,
        "layers": layers,
    });
    let mut meta = promo_model::ProjectMetadata::from_json(&document.to_string())
        .map_err(|e| format!("authored an invalid project: {e}"))?;
    if resolve {
        promo_timeline::resolve_attachments(&mut meta);
    }
    // The features decide the gate, asked of the model rather than
    // restated here.
    meta.min_reader_version = Some(meta.minimum_reader_version());
    meta.to_json().map_err(|e| format!("re-encode: {e}"))
}

/// One card's whole life, ported from macapp's CarouselChoreography: the
/// first card opens settled and the last one stays, so the show neither
/// begins nor ends on an empty canvas; every handover between is the full
/// flight. Placement and rotation ride separate tracks at the same times.
fn carousel_keyframes(
    ids: &mut Ids,
    duration: f64,
    canvas_w: f64,
    canvas_h: f64,
    right_to_left: bool,
    is_first: bool,
    is_last: bool,
) -> Vec<Value> {
    const OFFSTAGE_HEIGHT: f64 = 430.0 / 1080.0;
    const SETTLED_HEIGHT: f64 = 680.0 / 1080.0;
    const DRIFTED_HEIGHT: f64 = 720.0 / 1080.0;
    const OFFSTAGE_OFFSET: f64 = 980.0 / 1920.0;
    const SETTLED_LIFT: f64 = -30.0 / 1080.0;
    const TILT_DEGREES: f64 = 6.0;

    let ramp = carousel_ramp(duration);
    let hold = (duration - ramp * 2.0).max(0.0);
    let sign = if right_to_left { 1.0 } else { -1.0 };
    let offset = OFFSTAGE_OFFSET * canvas_w * sign;
    let lift = SETTLED_LIFT * canvas_h;

    let placed = |height: f64, dx: f64, dy: f64| -> Value {
        json!({"height": height * canvas_h, "anchor": "center", "offset": [dx, dy]})
    };
    let mut key = |time: f64, transition: f64, easing: Option<&str>| -> Value {
        let mut k = json!({"id": ids.take(), "time": time,
                           "transitionDuration": transition});
        if let Some(easing) = easing {
            k["easing"] = json!(easing);
        }
        k
    };

    let mut frames: Vec<Value> = Vec::new();
    if is_first {
        let mut settled = key(0.0, 0.0, None);
        settled["placement"] = placed(SETTLED_HEIGHT, 0.0, lift);
        frames.push(settled);
        let mut level = key(0.0, 0.0, None);
        level["rotation"] = json!(0.0);
        frames.push(level);
    } else {
        let mut offstage = key(0.0, 0.0, None);
        offstage["placement"] = placed(OFFSTAGE_HEIGHT, offset, 0.0);
        frames.push(offstage);
        let mut arrive = key(ramp, ramp, Some("easeOut"));
        arrive["placement"] = placed(SETTLED_HEIGHT, 0.0, lift);
        frames.push(arrive);
        let mut tipped = key(0.0, 0.0, None);
        tipped["rotation"] = json!(TILT_DEGREES * sign);
        frames.push(tipped);
        let mut level = key(ramp, ramp, Some("easeOut"));
        level["rotation"] = json!(0.0);
        frames.push(level);
    }
    if is_last {
        // Nothing after it to hand over to: hold the drift to the end
        // rather than leaving the frame empty on the final beat.
        let mut drifted = key(duration, hold, Some("easeInOut"));
        drifted["placement"] = placed(DRIFTED_HEIGHT, 0.0, lift);
        frames.push(drifted);
        let mut level = key(duration, 0.0, None);
        level["rotation"] = json!(0.0);
        frames.push(level);
        return frames;
    }
    let mut drift = key(ramp + hold, hold, Some("easeInOut"));
    drift["placement"] = placed(DRIFTED_HEIGHT, 0.0, lift);
    frames.push(drift);
    let mut leave = key(duration, ramp, Some("easeIn"));
    leave["placement"] = placed(OFFSTAGE_HEIGHT, -offset, 0.0);
    frames.push(leave);
    let mut level = key(ramp + hold, 0.0, None);
    level["rotation"] = json!(0.0);
    frames.push(level);
    let mut tip_out = key(duration, ramp, Some("easeIn"));
    tip_out["rotation"] = json!(-TILT_DEGREES * sign);
    frames.push(tip_out);
    frames
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(kind: &str, transition: &str, slides: usize) -> AuthorSpec {
        serde_json::from_value(json!({
            "name": "Show",
            "kind": kind,
            "transition": transition,
            "createdAt": 1000.0,
            "slides": (0..slides).map(|i| json!({
                "filename": format!("s{i}.png"),
                "kind": "image",
                "pixelWidth": 1179.0,
                "pixelHeight": 2556.0,
                "caption": format!("Cap {i}"),
            })).collect::<Vec<_>>(),
        }))
        .unwrap()
    }

    fn parsed(spec: &AuthorSpec) -> Value {
        serde_json::from_str(&author(spec).expect("authors")).unwrap()
    }

    fn layers(doc: &Value) -> &Vec<Value> {
        doc["layers"].as_array().unwrap()
    }

    /// The authored document must not merely parse: the validator that
    /// names authoring mistakes has to find nothing to say.
    #[test]
    fn every_style_authors_a_clean_project() {
        for kind in ["classic", "carousel", "appStore"] {
            let doc = author(&spec(kind, "crossfade", 3)).expect(kind);
            let meta = promo_model::ProjectMetadata::from_json(&doc).expect(kind);
            let warnings = promo_timeline::validate::warnings(&meta);
            assert!(warnings.is_empty(), "{kind}: {warnings:?}");
        }
    }

    #[test]
    fn classic_lays_slides_end_to_end_and_dissolves_collapse_to_fade_in() {
        let doc = parsed(&spec("classic", "crossfade", 3));
        let layers = layers(&doc);
        assert_eq!(
            layers.len(),
            7,
            "background + a picture and its caption per slide"
        );
        // Slide 2 arrives over slide 1: starts half a second early, spans
        // the overlap extra, and carries the one-number shorthand rather
        // than a transitionIn object that says nothing more.
        let second = &layers[3];
        assert_eq!(second["startTime"], json!(2.5));
        assert_eq!(second["duration"], json!(3.5));
        assert_eq!(second["fadeIn"], json!(0.5));
        assert!(second.get("transitionIn").is_none());
        assert_eq!(doc["videoDuration"], json!(9.0));
    }

    #[test]
    fn a_push_shoves_the_slide_before_out_the_far_side() {
        let doc = parsed(&spec("classic", "push", 2));
        let layers = layers(&doc);
        let first = &layers[1];
        let second = &layers[3];
        assert_eq!(second["transitionIn"]["kind"], json!("push"));
        assert_eq!(second["transitionIn"]["from"], json!("right"));
        assert_eq!(first["transitionOut"]["kind"], json!("push"));
        assert_eq!(first["transitionOut"]["from"], json!("left"));
        assert!(
            first.get("transitionIn").is_none(),
            "nothing arrives before the first"
        );
    }

    #[test]
    fn the_carousel_opens_settled_and_flies_every_handover() {
        let doc = parsed(&spec("carousel", "crossfade", 3));
        let layers = layers(&doc);
        // Cards state their motion in keyframes and take no edge
        // transitions at all (their captions ride along, plain).
        let cards: Vec<&Value> = layers[1..]
            .iter()
            .filter(|l| l["kind"] == json!("image"))
            .collect();
        assert_eq!(cards.len(), 3);
        for layer in &cards {
            assert!(layer.get("transitionIn").is_none());
            assert!(layer.get("fadeIn").is_none());
        }
        // 3s slides: ramp = 0.9; the second card starts one ramp early.
        assert_eq!(cards[1]["startTime"], json!(3.0 - 0.9));
        // The first card is already settled at its first instant — a show
        // that flew it would open (and be postered) on an empty canvas.
        let first_keyframes = cards[0]["keyframes"].as_array().unwrap();
        let opening = &first_keyframes[0]["placement"];
        assert_eq!(opening["height"], json!(680.0 / 1080.0 * 1080.0));
        // A middle card flies in, drifts, and flies out: 8 keyframes
        // across the placement and rotation tracks.
        assert_eq!(cards[1]["keyframes"].as_array().unwrap().len(), 8);
    }

    #[test]
    fn a_store_listing_takes_the_devices_canvas_and_captions_every_shot() {
        let doc = parsed(&spec("appStore", "crossfade", 2));
        assert_eq!(doc["compositionSettings"]["canvasWidth"], json!(1290.0));
        assert_eq!(doc["compositionSettings"]["canvasHeight"], json!(2796.0));
        // THREE layers however many shots: the ground, one body whose
        // screen plays the shots, one headline whose words swap.
        let layers = layers(&doc);
        let kinds: Vec<_> = layers.iter().map(|l| l["kind"].clone()).collect();
        assert_eq!(
            kinds,
            vec![json!("background"), json!("model"), json!("caption")]
        );
        let resources = doc["resources"].as_array().unwrap();
        let captions: Vec<_> = resources
            .iter()
            .filter(|r| r["kind"] == json!("caption"))
            .collect();
        assert_eq!(captions.len(), 2);
        assert!(captions.iter().any(|c| c["captionText"] == json!("Cap 0")));
        // The shots wear no frame: they are pictures on the body's screen,
        // through a reel laid out by the show's own rule.
        let shots: Vec<_> = resources
            .iter()
            .filter(|r| r["kind"] == json!("image"))
            .collect();
        assert_eq!(shots.len(), 2);
        assert!(shots.iter().all(|s| s.get("frame").is_none()));
        let reel = resources
            .iter()
            .find(|r| r["kind"] == json!("composition"))
            .unwrap();
        assert_eq!(
            reel["composition"]["layers"].as_array().map(Vec::len),
            Some(2)
        );
        // A headless host has no library: the body is the engine's recipe.
        let body = resources
            .iter()
            .find(|r| r["kind"] == json!("model"))
            .unwrap();
        assert_eq!(body["recipe"]["device"]["kind"], json!("phone"));
        assert_eq!(body["materials"]["Screen"]["resourceID"], reel["id"]);
        assert_eq!(layers[1]["resourceID"], body["id"]);
        assert!(layers[1]["keyframes"][0].get("camera").is_some());
        // The headline swaps to the second shot's words.
        assert_eq!(layers[2]["keyframes"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn hard_cuts_overlap_nothing() {
        let doc = parsed(&spec("classic", "none", 2));
        let layers = layers(&doc);
        assert_eq!(layers[3]["startTime"], json!(3.0));
        assert!(layers[3].get("fadeIn").is_none());
    }

    /// Issue #7: `slides[].caption` was accepted for every kind and became
    /// a layer for one. A classic or carousel slide with words now carries
    /// a lower third that lives and arrives with its picture; a slide
    /// without words carries nothing, so a show of bare pictures is
    /// unchanged.
    #[test]
    fn every_kind_captions_the_slides_that_have_words() {
        for kind in ["classic", "carousel"] {
            let mut spec = spec(kind, "crossfade", 3);
            spec.slides[1].caption = String::new();
            let doc = parsed(&spec);
            let layers = layers(&doc);
            let captions: Vec<&Value> = layers
                .iter()
                .filter(|l| l["kind"] == json!("caption"))
                .collect();
            assert_eq!(captions.len(), 2, "{kind}: two slides had words");
            let picture = &layers[1];
            let caption = &layers[2];
            assert_eq!(caption["startTime"], picture["startTime"], "{kind}");
            assert_eq!(caption["duration"], picture["duration"], "{kind}");
            assert_eq!(
                caption.get("fadeIn"),
                picture.get("fadeIn"),
                "{kind}: arrives with it"
            );
            let resource = doc["resources"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"] == caption["resourceID"])
                .unwrap();
            assert_eq!(resource["captionText"], json!("Cap 0"));
            assert_eq!(
                resource["captionStyle"]["placement"]["anchor"],
                json!("bottom"),
                "{kind}: a lower third"
            );
            assert!(resource["captionStyle"].get("verticalMargin").is_none());
            let meta = promo_model::ProjectMetadata::from_json(&author(&spec).unwrap()).unwrap();
            assert!(
                promo_timeline::validate::warnings(&meta).is_empty(),
                "{kind}"
            );
        }
    }

    #[test]
    fn an_empty_show_is_refused() {
        assert!(author(&spec("classic", "crossfade", 0)).is_err());
    }
}

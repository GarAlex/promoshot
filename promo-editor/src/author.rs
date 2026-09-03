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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorSlide {
    /// Already staged into the project's Resources/ by the host.
    pub filename: String,
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

/// The App Store's accepted screenshot size for the device, as of 2026-08 —
/// one edit each when Apple revises them, and the canvas stays editable in
/// Settings afterwards. Mirrors macapp's `AppStoreDevice`.
fn device_geometry(device: &str) -> (f64, f64, f64, &'static str, f64) {
    // (canvas w, canvas h, caption band fraction, material, bezel fraction)
    match device {
        "iPad" => (2064.0, 2752.0, 0.30, "spaceBlack", 0.030),
        "mac" => (2880.0, 1800.0, 0.34, "silver", 0.018),
        _ => (1290.0, 2796.0, 0.26, "naturalTitanium", 0.035),
    }
}

fn transition_kind(transition: &str) -> Option<&'static str> {
    match transition {
        "crossfade" => Some("fade"),
        "wipe" => Some("wipe"),
        "slide" => Some("slide"),
        "push" => Some("push"),
        "scale" => Some("scale"),
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
    if spec.transition == "none" || index == 0 || index >= spec.slides.len() {
        return 0.0;
    }
    let outgoing = &spec.slides[index - 1];
    outgoing
        .transition_duration
        .min(outgoing.duration)
        .min(spec.slides[index].duration)
        .max(0.0)
}

/// The wizard's caption typography. A store shot's headline sits in the
/// band above the device (a top margin); every other show's caption is a
/// lower third — anchored to the bottom by rule, lifted clear of the edge,
/// stroked so it reads over any picture — and re-resolves if the canvas is
/// re-stamped for another size.
fn caption_style(app_store: bool, font: f64, canvas_w: f64, canvas_h: f64) -> Value {
    let mut style = json!({
        "fontSize": font,
        "isBold": true,
        "alignment": "center",
        "backgroundOpacity": 0.0,
        "shadowOpacity": 0.35,
        "shadowRadius": font * 0.22,
        "shadowOffset": [0.0, font * 0.08],
        "leftMargin": canvas_w * 0.08,
        "rightMargin": canvas_w * 0.08,
    });
    if app_store {
        style["verticalMargin"] = json!(canvas_h * 0.06);
    } else {
        style["placement"] = json!({"anchor": "bottom", "offset": [0.0, -(canvas_h * 0.07)]});
        style["strokeWidth"] = json!(font * 0.08);
        style["strokeColorHex"] = json!("000000");
        style["textColorHex"] = json!("FFFFFF");
    }
    style
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

    // A store listing is sized by the store, so the canvas is the FIRST
    // thing the choice decides.
    let (canvas_w, canvas_h) = if app_store {
        let (w, h, _, _, _) = device_geometry(&spec.device);
        (w, h)
    } else {
        (
            spec.canvas_width.unwrap_or(1920.0),
            spec.canvas_height.unwrap_or(1080.0),
        )
    };
    let (_, _, band, material, _) = device_geometry(&spec.device);
    // The body every framable shot stands in: a device recipe the engine
    // builds, so the authored document needs no file beside the shots.
    let (device_kind, device_label) = match spec.device.as_str() {
        "iPad" => ("tablet", "Tablet"),
        "mac" => ("laptop", "Laptop"),
        _ => ("phone", "Phone"),
    };
    let body_hex = match material {
        "spaceBlack" => "1C1C1E",
        "naturalTitanium" => "8E8B85",
        "silver" => "C8C9CC",
        _ => "@edge",
    };
    let (camera_yaw, camera_pitch) = if spec.framing == "angled" {
        (-12.0, 4.0)
    } else {
        (0.0, 0.0)
    };

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
        for (i, slide) in spec.slides.iter().enumerate() {
            starts.push(cursor);
            spans.push(slide.duration);
            let _ = i;
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
        "keyframes": [{"id": ids.take(), "time": 0.0, "transitionDuration": 0.0,
                       "colorHex": spec.background_color_hex}],
    }));
    sort += 1;

    let edge_kind = transition_kind(&spec.transition);
    let effective_edge = spec
        .transition_edge
        .as_deref()
        .unwrap_or_else(|| edge_kind.map(default_edge).unwrap_or("left"));

    for (i, slide) in spec.slides.iter().enumerate() {
        let is_video = slide.kind == "video";
        let resource_id = ids.take();
        let mut resource = json!({
            "id": resource_id,
            "kind": if is_video { "video" } else { "image" },
            "filename": slide.filename,
            "displayName": slide.display_name.clone()
                .unwrap_or_else(|| slide.filename.clone()),
            "addedAt": spec.created_at,
        });
        if is_video {
            resource["duration"] = json!(slide.duration);
        }
        if let (Some(w), Some(h)) = (slide.pixel_width, slide.pixel_height) {
            resource["pixelWidth"] = json!(w);
            resource["pixelHeight"] = json!(h);
        }
        resources.push(resource);
        // Every framable shot stands in a device BODY: a recipe resource of
        // its own with the shot bound to the Screen slot, decided once from
        // the device the wizard chose. V1 frames every image; the Mac's
        // photograph-evidence gate is logged backlog above.
        let device_id = if app_store && !is_video {
            let id = ids.take();
            resources.push(json!({
                "id": id,
                "kind": "model",
                "filename": "",
                "displayName": format!(
                    "{device_label} · {}",
                    slide.display_name.clone().unwrap_or_else(|| slide.filename.clone())
                ),
                "addedAt": spec.created_at,
                "imageCuts": [],
                "disabledAudioTrackIndices": [],
                "clips": [],
                "recipe": {"device": {"kind": device_kind}},
                "materials": {"Body": body_hex, "Screen": {"resourceID": resource_id}},
            }));
            Some(id)
        } else {
            None
        };

        // Entry/exit transitions (classic + appStore; carousel states its
        // motion in keyframes and takes none). A plain dissolve collapses
        // to the fadeIn shorthand it has always been.
        let mut fade_in: Option<f64> = None;
        let mut transition_in: Option<Value> = None;
        let mut transition_out: Option<Value> = None;
        if !carousel {
            if let Some(kind) = edge_kind {
                let seconds = crossfade(spec, i);
                if i > 0 && seconds > 0.0 {
                    if kind == "fade" {
                        fade_in = Some(seconds);
                    } else {
                        let mut t = json!({"kind": kind, "duration": seconds});
                        if uses_edge(kind) {
                            t["from"] = json!(effective_edge);
                        }
                        transition_in = Some(t);
                    }
                }
                // Only a push moves what it replaces; it leaves by the far
                // side under its own steam, which is what reads as the
                // shove.
                if kind == "push" && i + 1 < n {
                    let seconds = crossfade(spec, i + 1);
                    if seconds > 0.0 {
                        transition_out = Some(json!({
                            "kind": "push",
                            "from": opposite(effective_edge),
                            "duration": seconds,
                        }));
                    }
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
            let mut keyframe = app_store_keyframe(&mut ids, slide, band, canvas_w, canvas_h);
            if device_id.is_some() {
                keyframe["camera"] =
                    json!({"yaw": camera_yaw, "pitch": camera_pitch, "distance": 4.6});
            }
            vec![keyframe]
        } else {
            vec![json!({
                "id": ids.take(),
                "time": 0.0,
                "transitionDuration": 0.0,
                "placement": {"mode": spec.sizing, "anchor": "center"},
            })]
        };

        let mut layer = json!({
            "id": ids.take(),
            "name": slide.display_name.clone().unwrap_or_else(|| slide.filename.clone()),
            "sortIndex": sort,
            "kind": if is_video { "video" } else if device_id.is_some() { "model" } else { "image" },
            "isEnabled": true,
            "startTime": starts[i],
            "duration": spans[i],
            "resourceID": device_id.clone().unwrap_or_else(|| resource_id.clone()),
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
        layers.push(layer);
        sort += 1;

        // A styled caption per slide: layout, typography and shadow are
        // what a wizard can decide; the words are what only the author
        // can. A store shot always carries its headline band, text left as
        // typed (empty is fine); a classic or carousel slide takes a lower
        // third only when words were given — until this, `slides[].caption`
        // was accepted for every kind and honoured for one (issue #7).
        // Colours stay UNSTATED so a theme can move them later.
        if app_store || !slide.caption.is_empty() {
            let font = if app_store {
                canvas_w * 0.062
            } else {
                canvas_h * 0.055
            };
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
                "captionStyle": caption_style(app_store, font, canvas_w, canvas_h),
            }));
            let mut caption_layer = json!({
                "id": ids.take(),
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
            layers.push(caption_layer);
            sort += 1;
        }
    }

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
        },
        "resources": resources,
        "layers": layers,
    });

    // Round-tripped through the model so what leaves here is canonical —
    // and so an authoring bug fails HERE, loudly, not in whichever reader
    // opens the file next.
    let mut meta = promo_model::ProjectMetadata::from_json(&document.to_string())
        .map_err(|e| format!("authored an invalid project: {e}"))?;
    // The features decide the gate, asked of the model rather than
    // restated here: placement rules alone put this at rung 8, and a
    // document that fails to declare it invites an older reader to open
    // it and drop them on its next save.
    meta.min_reader_version = Some(meta.minimum_reader_version());
    meta.to_json().map_err(|e| format!("re-encode: {e}"))
}

/// One store shot's placed keyframe: fits BOTH dimensions into the room
/// under the caption band and centres in that room (half a band below the
/// canvas centre), so the binding constraint is chosen per picture.
fn app_store_keyframe(
    ids: &mut Ids,
    slide: &AuthorSlide,
    band: f64,
    canvas_w: f64,
    canvas_h: f64,
) -> Value {
    let area_w = canvas_w * 0.84;
    let area_h = (canvas_h - canvas_h * band) * 0.90;
    let drop = canvas_h * band / 2.0;
    let aspect = match (slide.pixel_width, slide.pixel_height) {
        (Some(w), Some(h)) if h > 0.0 => w / h,
        _ => 1.0,
    };
    let placement = if aspect > area_w / area_h.max(1.0) {
        json!({"width": area_w, "anchor": "center", "offset": [0.0, drop]})
    } else {
        json!({"height": area_h, "anchor": "center", "offset": [0.0, drop]})
    };
    json!({"id": ids.take(), "time": 0.0, "transitionDuration": 0.0,
           "placement": placement})
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
        let layers = layers(&doc);
        // Background, then per slide: shot + caption.
        assert_eq!(layers.len(), 5);
        let resources = doc["resources"].as_array().unwrap();
        let captions: Vec<_> = resources
            .iter()
            .filter(|r| r["kind"] == json!("caption"))
            .collect();
        assert_eq!(captions.len(), 2);
        assert_eq!(captions[0]["captionText"], json!("Cap 0"));
        // Shots stand in a device BODY — a recipe resource with the shot on
        // its Screen slot, no frame on the picture — and a portrait
        // screenshot is bound by the height it is allowed.
        let shot = resources
            .iter()
            .find(|r| r["kind"] == json!("image"))
            .unwrap();
        assert!(shot.get("frame").is_none());
        let body = resources
            .iter()
            .find(|r| r["kind"] == json!("model"))
            .unwrap();
        assert_eq!(body["recipe"]["device"]["kind"], json!("phone"));
        assert_eq!(body["materials"]["Screen"]["resourceID"], shot["id"]);
        assert_eq!(layers[1]["kind"], json!("model"));
        assert_eq!(layers[1]["resourceID"], body["id"]);
        assert!(layers[1]["keyframes"][0].get("camera").is_some());
        let placed = &layers[1]["keyframes"][0]["placement"];
        assert!(placed.get("height").is_some());
        assert!(placed.get("width").is_none());
        // The words leave with the picture: the caption layer carries the
        // same window and the same arrival.
        assert_eq!(layers[2]["startTime"], layers[1]["startTime"]);
        assert_eq!(layers[2]["fadeIn"], layers[1]["fadeIn"]);
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

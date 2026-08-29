//! Problems worth telling an author about, in the language they wrote.
//!
//! Not decode errors — those already come back from `ProjectMetadata::from_json`.
//! These are the silent corrections: a project that renders, but not the way
//! the file says. The renderer has always made them; nothing said so.

use crate::viewport;
use promo_model::{ProjectLayerKind, ProjectMetadata};

/// Every warning for `meta`, in the order an author would read the file.
///
/// Strings rather than a typed enum: the two callers (the CLI and the MCP
/// tool) both print prose, and a shape neither needs is a shape that goes
/// stale. The prefix convention — `layer "NAME" at Ts: …` — matches the
/// warnings the app already emits, so the two lists read as one.
pub fn warnings(meta: &ProjectMetadata) -> Vec<String> {
    let mut out = Vec::new();
    duration_rule_warnings(meta, &mut out);
    palette_warnings(meta, &mut out);
    for layer in meta.layers.as_deref().unwrap_or(&[]) {
        let honours_viewport = matches!(
            layer.kind,
            ProjectLayerKind::Image | ProjectLayerKind::Video
        );
        for keyframe in &layer.keyframes {
            let Some(window) = keyframe.viewport else {
                continue;
            };
            let at = format!("layer \"{}\" at {}s", layer.name, keyframe.time);
            if !honours_viewport {
                out.push(format!(
                    "{at}: viewport is ignored on a {:?} layer — only image and \
                     video layers show a window of their source",
                    layer.kind
                ));
                continue;
            }
            if let Some(slid) = viewport::out_of_bounds(window) {
                out.push(format!(
                    "{at}: viewport {window:?} hangs outside the source — the \
                     renderer slides it back to {slid:?}, size first"
                ));
            }
        }
    }

    for layer in meta.layers.as_deref().unwrap_or(&[]) {
        // Two ways to say one thing, saying different things. The renderer
        // picks the richer one; nothing said so until now.
        for (side, rich, fade) in [
            ("transitionIn", &layer.transition_in, layer.fade_in),
            ("transitionOut", &layer.transition_out, layer.fade_out),
        ] {
            let shorthand = if side == "transitionIn" {
                "fadeIn"
            } else {
                "fadeOut"
            };
            if let (Some(rich), Some(seconds)) = (rich.as_ref(), fade) {
                out.push(format!(
                    "layer \"{}\": {shorthand} {seconds}s and {side} \"{}\" both set — \
                     {side} wins and the {shorthand} is ignored",
                    layer.name,
                    rich.kind.as_str()
                ));
            }
        }
        if let Some(span) = layer.duration {
            for (side, transition) in [
                ("transitionIn", crate::transition::incoming(layer)),
                ("transitionOut", crate::transition::outgoing(layer)),
            ] {
                if let Some(t) = transition {
                    if t.duration > span {
                        out.push(format!(
                            "layer \"{}\": {side} lasts {}s but the layer is only {span}s — \
                             it never finishes arriving",
                            layer.name, t.duration
                        ));
                    }
                }
            }
        }
    }

    // A grade whose fields cannot act is a broken-looking feature: a tint
    // amount with no colour (or a colour with no amount) does nothing, and
    // a constant the keyframes override is dead weight.
    for layer in meta.layers.as_deref().unwrap_or(&[]) {
        if let Some(adjust) = &layer.adjustments {
            let has_amount = adjust.tint_amount.is_some_and(|a| a > 0.0)
                || layer.keyframes.iter().any(|k| k.tint_amount.is_some());
            if adjust.tint_hex.is_some() && !has_amount {
                out.push(format!(
                    "layer \"{}\": adjustments name a tintHex but no tintAmount — \
                     the gel is never applied",
                    layer.name
                ));
            }
            if has_amount && adjust.tint_hex.is_none() {
                out.push(format!(
                    "layer \"{}\": a tintAmount with no tintHex does nothing — \
                     name the gel's colour",
                    layer.name
                ));
            }
            let overridden = [
                (
                    adjust.saturation.is_some()
                        && layer.keyframes.iter().any(|k| k.saturation.is_some()),
                    "saturation",
                ),
                (
                    adjust.contrast.is_some()
                        && layer.keyframes.iter().any(|k| k.contrast.is_some()),
                    "contrast",
                ),
                (
                    adjust.brightness.is_some()
                        && layer.keyframes.iter().any(|k| k.brightness.is_some()),
                    "brightness",
                ),
                (
                    adjust.tint_amount.is_some()
                        && layer.keyframes.iter().any(|k| k.tint_amount.is_some()),
                    "tintAmount",
                ),
            ];
            for (both, name) in overridden {
                if both {
                    out.push(format!(
                        "layer \"{}\": has BOTH a constant {name} and keyframed \
                         {name} — the keyframes win and the constant is ignored",
                        layer.name
                    ));
                }
            }
        }
    }

    // A blend mode on a layer that draws no pixels does nothing.
    for layer in meta.layers.as_deref().unwrap_or(&[]) {
        if layer.blend_mode.is_some()
            && matches!(
                layer.kind,
                promo_model::ProjectLayerKind::Background | promo_model::ProjectLayerKind::Audio
            )
        {
            out.push(format!(
                "layer \"{}\": blendMode on a {:?} layer does nothing — only \
                 layers that draw pixels combine with anything",
                layer.name, layer.kind
            ));
        }
    }

    // A mask must point at a drawing with ink, on a layer that draws media.
    for layer in meta.layers.as_deref().unwrap_or(&[]) {
        let media = matches!(
            layer.kind,
            promo_model::ProjectLayerKind::Video | promo_model::ProjectLayerKind::Image
        );
        if let Some(rid) = layer.mask_resource_id.as_deref() {
            if !media {
                out.push(format!(
                    "layer \"{}\": maskResourceID on a {:?} layer does nothing — \
                     masks window video and image layers",
                    layer.name, layer.kind
                ));
            }
            match meta
                .resources
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .find(|r| r.id == rid)
            {
                None => out.push(format!(
                    "layer \"{}\": maskResourceID \"{rid}\" names no resource — \
                     the layer renders unmasked",
                    layer.name
                )),
                Some(resource) => match resource.drawing.as_ref() {
                    None => out.push(format!(
                        "layer \"{}\": mask resource \"{}\" is {:?}, not a drawing — \
                         a mask is a drawing's ink, and the layer renders unmasked",
                        layer.name, resource.display_name, resource.kind
                    )),
                    Some(doc) if doc.shapes.is_empty() => out.push(format!(
                        "layer \"{}\": mask drawing \"{}\" has no shapes — no ink, \
                         no window, the layer renders unmasked",
                        layer.name, resource.display_name
                    )),
                    Some(_) => {}
                },
            }
        } else if layer.mask_inverted.is_some() {
            out.push(format!(
                "layer \"{}\": maskInverted without a maskResourceID does nothing",
                layer.name
            ));
        }
        // Placement keyframes fly the window; with no window they fly nothing.
        let flies = layer.keyframes.iter().any(|k| {
            k.mask_offset_x.is_some()
                || k.mask_offset_y.is_some()
                || k.mask_zoom.is_some()
                || k.mask_rotation.is_some()
        });
        if flies && layer.mask_resource_id.is_none() {
            out.push(format!(
                "layer \"{}\": mask placement keyframes without a maskResourceID \
                 do nothing — there is no window to fly",
                layer.name
            ));
        }
        for keyframe in &layer.keyframes {
            if keyframe.mask_zoom.is_some_and(|z| z <= 0.0) {
                out.push(format!(
                    "layer \"{}\": maskZoom {} at {}s — zero or negative collapses \
                     the window; the renderer clamps it to nearly nothing",
                    layer.name,
                    keyframe.mask_zoom.unwrap_or_default(),
                    keyframe.time
                ));
            }
        }
    }

    // Ids are UUIDs — all of them. The Rust side reads them as strings and
    // never minds, which is exactly the trap: `validate` said "ok" to a
    // project whose layers were named "CAP" and "B", and the app then
    // refused to LIST it — no error, no row, a project that exists on disk
    // and nowhere else. The one place that can say why is here.
    let uuid_ok = |id: &str| {
        let bytes = id.as_bytes();
        bytes.len() == 36
            && bytes.iter().enumerate().all(|(i, b)| match i {
                8 | 13 | 18 | 23 => *b == b'-',
                _ => b.is_ascii_hexdigit(),
            })
    };
    for layer in meta.layers.as_deref().unwrap_or(&[]) {
        if !uuid_ok(&layer.id) {
            out.push(format!(
                "layer \"{}\": id \"{}\" is not a UUID — the app reads ids as \
                 UUIDs and will refuse to open the project (silently: it \
                 just never appears in the list)",
                layer.name, layer.id
            ));
        }
        for keyframe in &layer.keyframes {
            if !uuid_ok(&keyframe.id) {
                out.push(format!(
                    "layer \"{}\": keyframe id \"{}\" is not a UUID — the app \
                     will refuse to open the project",
                    layer.name, keyframe.id
                ));
            }
        }
    }
    for resource in meta.resources.as_deref().unwrap_or(&[]) {
        if !uuid_ok(&resource.id) {
            out.push(format!(
                "resource \"{}\": id \"{}\" is not a UUID — the app will \
                 refuse to open the project",
                resource.display_name, resource.id
            ));
        }
    }

    // A shutter is a fraction of one frame interval, open (0, 1]. Zero or
    // negative does nothing, and more than 1 is a shutter open longer than
    // the frame it exposes — the engine clamps it, so say so here rather
    // than let two projects with different numbers render the same.
    for layer in meta.layers.as_deref().unwrap_or(&[]) {
        for keyframe in &layer.keyframes {
            if let Some(shutter) = keyframe.shutter {
                if !(0.0..=1.0).contains(&shutter) {
                    out.push(format!(
                        "layer \"{}\": keyframe shutter {} is outside 0..1 — the \
                         renderer clamps it (0 = sharp, 1 = 360 degrees)",
                        layer.name, shutter
                    ));
                }
            }
        }
        if layer.motion_blur.is_some() && layer.keyframes.iter().any(|k| k.shutter.is_some()) {
            out.push(format!(
                "layer \"{}\": has BOTH motionBlur and keyframed shutters — the \
                 keyframes win and the constant is ignored",
                layer.name
            ));
        }
        if let Some(blur) = &layer.motion_blur {
            if blur.shutter <= 0.0 {
                out.push(format!(
                    "layer \"{}\": motionBlur shutter {} does nothing — use 0.5 \
                     for the classic 180 degrees, or drop the field",
                    layer.name, blur.shutter
                ));
            } else if blur.shutter > 1.0 {
                out.push(format!(
                    "layer \"{}\": motionBlur shutter {} is longer than the frame \
                     — the renderer clamps it to 1 (360 degrees)",
                    layer.name, blur.shutter
                ));
            }
        }
    }

    // A reveal states its pace one way or the other. Both is not an error —
    // the total wins — but it is certainly not what the author meant.
    let mut reveal_conflict = |where_: String, reveal: &promo_model::TextReveal| {
        if let (Some(per), Some(total)) = (reveal.seconds_per, reveal.seconds) {
            out.push(format!(
                "{where_}: reveal states secondsPer {per} AND seconds {total} — \
                 the total wins and the rate is ignored"
            ));
        }
        // An arrival time on a mode that has no arrival is a setting that
        // does nothing, which reads as a broken feature rather than a
        // mode that was never changed.
        if reveal.unit_seconds.is_some() && !reveal.animates() {
            out.push(format!(
                "{where_}: reveal sets unitSeconds but mode {} has no arrival — \
                 use fade, rise or scale, or drop it",
                reveal.mode.as_str()
            ));
        }
        if reveal.rise.is_some() && reveal.mode != promo_model::RevealMode::Rise {
            out.push(format!(
                "{where_}: reveal sets rise but mode is {} — it only travels in \
                 rise mode",
                reveal.mode.as_str()
            ));
        }
    };
    if let Some(reveal) = meta.composition_settings.subtitle_reveal.as_ref() {
        reveal_conflict("compositionSettings.subtitleReveal".into(), reveal);
    }
    for layer in meta.layers.as_deref().unwrap_or(&[]) {
        if let Some(reveal) = layer
            .caption_style
            .as_ref()
            .and_then(|style| style.reveal.as_ref())
        {
            reveal_conflict(format!("layer \"{}\"", layer.name), reveal);
        }
    }
    for resource in meta.resources.as_deref().unwrap_or(&[]) {
        if let Some(reveal) = resource
            .caption_style
            .as_ref()
            .and_then(|style| style.reveal.as_ref())
        {
            reveal_conflict(format!("caption \"{}\"", resource.display_name), reveal);
        }
    }

    let required = meta.minimum_reader_version();
    match meta.min_reader_version {
        Some(declared) if declared >= required => {}
        Some(declared) => out.push(format!(
            "this project declares \"minReaderVersion\": {declared} but uses \
             features that need {required} — an older reader would open it and \
             drop them on its next save"
        )),
        None if required > 1 => out.push(format!(
            "this project uses features that need \"minReaderVersion\": \
             {required}, which it does not declare — an older reader would open \
             it and drop them on its next save"
        )),
        None => {}
    }
    out
}

/// A duration rule wired to nothing does nothing, and says so here rather
/// than letting the author wonder why the slide never waits.
/// Colour references the palette does not define, and a palette resource
/// the settings copy has drifted from.
///
/// Walked over the SERIALIZED document rather than field by field, so it
/// cannot fall behind the model: every `*Hex` string the file writes is
/// checked, fields added since this was written included. The palette's own
/// entries are skipped — they are the definitions references point at, not
/// uses of a colour.
fn palette_warnings(meta: &ProjectMetadata, out: &mut Vec<String>) {
    let settings = &meta.composition_settings;
    let defined: std::collections::BTreeSet<String> = settings
        .palette
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|entry| entry.name.to_lowercase())
        .collect();

    let Ok(document) = serde_json::to_value(meta) else {
        return;
    };
    let mut used = std::collections::BTreeSet::new();
    collect_colour_references(&document, &mut used);
    for name in used.difference(&defined) {
        out.push(format!(
            "colour \"@{name}\" is used but no palette entry defines it — an \
             unresolved name is NOT a fallback to the field's own default: it \
             is handed on unchanged, fails to parse as hex, and renders black"
        ));
    }

    // The app materializes the selected resource into `settings.palette` on
    // open and save, so the two agreeing is the normal state. When they do
    // not, a render from the file AS IT STANDS — the CLI, or anything that
    // never opened it in the app — uses the settings copy.
    if let Some(id) = settings.palette_resource_id.as_deref() {
        match meta
            .resources
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|r| r.id == id)
        {
            None => out.push(format!(
                "compositionSettings.paletteResourceID names \"{id}\", which is \
                 not a resource in this project — nothing materializes, and \
                 settings.palette stands as written"
            )),
            Some(resource) => {
                let entries = resource.palette.as_deref().unwrap_or_default();
                let same = entries.len() == settings.palette.as_deref().unwrap_or_default().len()
                    && entries
                        .iter()
                        .zip(settings.palette.as_deref().unwrap_or_default().iter())
                        .all(|(a, b)| {
                            a.name.eq_ignore_ascii_case(&b.name)
                                && a.color_hex.eq_ignore_ascii_case(&b.color_hex)
                        });
                if !same {
                    out.push(format!(
                        "compositionSettings.palette differs from the palette \
                         resource \"{}\" it follows — the app rewrites it from \
                         the resource on open, so a render from this file as it \
                         stands uses the copy in settings",
                        resource.display_name
                    ));
                }
            }
        }
    }
}

/// Every `@name` a `*Hex` field in the document holds, lowercased.
fn collect_colour_references(
    node: &serde_json::Value,
    out: &mut std::collections::BTreeSet<String>,
) {
    match node {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                // Definitions, not uses — on the settings and on a palette
                // resource alike.
                if key == "palette" {
                    continue;
                }
                if key.ends_with("Hex") {
                    if let Some(name) = value.as_str().and_then(|s| s.strip_prefix('@')) {
                        out.insert(name.to_lowercase());
                    }
                } else {
                    collect_colour_references(value, out);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_colour_references(item, out);
            }
        }
        _ => {}
    }
}

fn duration_rule_warnings(meta: &ProjectMetadata, out: &mut Vec<String>) {
    use promo_model::{DurationRuleKind, TimingReference};
    let layers = meta.layers.as_deref().unwrap_or(&[]);
    let resources = meta.resources.as_deref().unwrap_or(&[]);

    let mut order: Vec<usize> = (0..layers.len()).collect();
    order.sort_by(|&a, &b| {
        layers[a]
            .sort_index
            .cmp(&layers[b].sort_index)
            .then_with(|| a.cmp(&b))
    });

    for (position, &index) in order.iter().enumerate() {
        let layer = &layers[index];
        let Some(rule) = layer.duration_rule else {
            continue;
        };
        match rule.kind {
            DurationRuleKind::FitContent => {
                let has_content = layer
                    .resource_id
                    .as_ref()
                    .and_then(|id| resources.iter().find(|r| &r.id == id))
                    .and_then(|r| r.duration)
                    .is_some_and(|d| d > 0.0);
                if layer.resource_id.is_none() {
                    out.push(format!(
                        "layer \"{}\": durationRule fitContent has no resource to fit — \
                         the stored duration stands",
                        layer.name
                    ));
                } else if !has_content {
                    out.push(format!(
                        "layer \"{}\": durationRule fitContent — the resource's length is \
                         not known yet (no file, or no measured duration); the stored \
                         duration stands until it is",
                        layer.name
                    ));
                }
                if layer.timing.as_ref().is_some_and(|t| t.end.is_some()) {
                    out.push(format!(
                        "layer \"{}\": durationRule and an END anchor are two producers \
                         for one number — the anchor wins and the rule does nothing",
                        layer.name
                    ));
                }
            }
            DurationRuleKind::FitDependents => {
                // A dependent is a layer whose START is anchored to this
                // layer's start — containment, mirroring the resolver.
                let has_dependent = order.iter().enumerate().any(|(other, &oi)| {
                    layers[oi]
                        .timing
                        .as_ref()
                        .and_then(|t| t.start.as_ref())
                        .is_some_and(|a| match a.from {
                            TimingReference::PreviousStart => other == position + 1,
                            TimingReference::NextStart => other + 1 == position,
                            _ => false,
                        })
                });
                if !has_dependent {
                    out.push(format!(
                        "layer \"{}\": durationRule fitDependents, but no layer's start \
                         is anchored to it — nothing to fit, the stored duration stands",
                        layer.name
                    ));
                }
                if layer.timing.as_ref().is_some_and(|t| t.end.is_some()) {
                    out.push(format!(
                        "layer \"{}\": durationRule and an END anchor are two producers \
                         for one number — the anchor wins and the rule does nothing",
                        layer.name
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(layers: &str, extra: &str) -> ProjectMetadata {
        ProjectMetadata::from_json(&format!(
            r#"{{"id":"AAAAAAAA-0000-0000-0000-00000000AAAA","name":"v","createdAt":0,
                 "state":"recorded","trimStart":0,"trimEnd":0,"videoDuration":0,
                 "subtitles":[],
                 "compositionSettings":{{"canvasWidth":1920,"canvasHeight":1080}}
                 {extra},"layers":[{layers}]}}"#
        ))
        .expect("fixture")
    }
    fn layer(kind: &str, keyframe: &str) -> String {
        format!(
            r#"{{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0A03","name":"Clip","sortIndex":0,"kind":"{kind}","isEnabled":true,
                 "startTime":0,"duration":4,
                 "keyframes":[{{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0A04","time":2,"transitionDuration":0{keyframe}}}]}}"#
        )
    }

    #[test]
    fn a_window_hanging_past_the_edge_is_named_with_its_keyframe() {
        let meta = project(
            &layer("video", r#","viewport":[0.55,0.1,0.6,0.4]"#),
            r#","minReaderVersion":6"#,
        );
        let warnings = warnings(&meta);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("layer \"Clip\" at 2s"),
            "{}",
            warnings[0]
        );
        assert!(warnings[0].contains("slides it back"), "{}", warnings[0]);
    }

    #[test]
    fn a_window_inside_the_frame_says_nothing() {
        let meta = project(
            &layer("video", r#","viewport":[0.2,0.2,0.5,0.5]"#),
            r#","minReaderVersion":6"#,
        );
        assert!(warnings(&meta).is_empty(), "{:?}", warnings(&meta));
    }

    /// A viewport on a caption is not clamped, it is dropped — a different
    /// silence, and worth a different sentence.
    #[test]
    fn a_window_on_a_layer_that_cannot_use_one_is_named_too() {
        let meta = project(
            &layer("caption", r#","viewport":[0,0,0.5,0.5]"#),
            r#","minReaderVersion":6"#,
        );
        let warnings = warnings(&meta);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("ignored"), "{}", warnings[0]);
    }

    #[test]
    fn a_project_that_understates_its_reader_version_is_told() {
        let meta = project(&layer("video", r#","viewport":[0,0,0.5,0.5]"#), "");
        let warnings = warnings(&meta);
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("minReaderVersion") && w.contains('6')),
            "{warnings:?}"
        );
    }

    /// A knob that does nothing is worse than a missing one: it reads as a
    /// broken feature. Both new reveal fields belong to particular modes.
    #[test]
    fn a_reveal_setting_its_mode_does_not_have_is_named() {
        let caption = |reveal: &str| {
            format!(
                r#"{{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0A01","name":"Words","sortIndex":0,"kind":"caption",
                     "isEnabled":true,"startTime":0,"duration":4,
                     "captionText":"one two","captionStyle":{{"reveal":{reveal}}},
                     "keyframes":[]}}"#
            )
        };
        // Declared, because a reveal now claims rung 9 and an undeclared
        // one would warn about that instead of about the field under test.
        let warned =
            |reveal: &str| warnings(&project(&caption(reveal), r#","minReaderVersion":9"#));

        // That claim itself: a reveal an older reader would silently drop.
        assert!(
            warnings(&project(&caption(r#"{"by":"word","mode":"wipe"}"#), ""))
                .iter()
                .any(|w| w.contains("minReaderVersion") && w.contains("9")),
            "an undeclared reveal is named, because a save would destroy it",
        );

        assert!(
            warned(r#"{"by":"word","mode":"wipe","unitSeconds":0.3}"#)
                .iter()
                .any(|w| w.contains("unitSeconds") && w.contains("no arrival")),
            "a type-on has no arrival to time",
        );
        assert!(
            warned(r#"{"by":"word","mode":"fade","rise":1.5}"#)
                .iter()
                .any(|w| w.contains("rise") && w.contains("only travels")),
            "and only a rise travels",
        );
        assert!(
            warned(r#"{"by":"word","mode":"rise","unitSeconds":0.3,"rise":1.5}"#).is_empty(),
            "a rise that states both is exactly what those fields are for",
        );
    }

    /// A shutter outside (0, 1] is either a no-op or quietly clamped —
    /// both worth a sentence, neither worth guessing at.
    #[test]
    fn a_useless_or_clamped_shutter_is_named() {
        let clip = |blur: &str| {
            format!(
                r#"{{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0A02","name":"Clip","sortIndex":0,"kind":"video",
                     "isEnabled":true,"startTime":0,"duration":4,
                     "motionBlur":{blur},"keyframes":[]}}"#
            )
        };
        let warned = |blur: &str| warnings(&project(&clip(blur), r#","minReaderVersion":10"#));
        assert!(warned(r#"{"shutter":0}"#)
            .iter()
            .any(|w| w.contains("does nothing")));
        assert!(warned(r#"{"shutter":1.5}"#)
            .iter()
            .any(|w| w.contains("clamps")));
        assert!(
            warned(r#"{"shutter":0.5}"#).is_empty(),
            "the 180-degree default is exactly right"
        );
        assert!(
            warnings(&project(&clip(r#"{"shutter":0.5}"#), ""))
                .iter()
                .any(|w| w.contains("minReaderVersion")),
            "and an undeclared one is named, because a save would destroy it",
        );
    }

    /// The CLI reads ids as strings; the app reads them as UUIDs and
    /// silently refuses a project whose ids are not. Validate is the only
    /// place that can say so before someone loses an afternoon to a project
    /// that "just never shows up".
    #[test]
    fn a_non_uuid_id_is_named_before_the_app_silently_refuses_it() {
        let bad = r#"{"id":"CAP","name":"Words","sortIndex":0,"kind":"caption",
                      "isEnabled":true,"startTime":0,"duration":4,
                      "captionText":"hi","keyframes":[]}"#;
        assert!(
            warnings(&project(bad, ""))
                .iter()
                .any(|w| w.contains("not a UUID")),
            "a layer id of \"CAP\" must be named",
        );
        let good = r#"{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0A11","name":"Words",
                       "sortIndex":0,"kind":"caption","isEnabled":true,
                       "startTime":0,"duration":4,"captionText":"hi","keyframes":[]}"#;
        assert!(
            !warnings(&project(good, ""))
                .iter()
                .any(|w| w.contains("not a UUID")),
            "a real UUID passes",
        );
    }

    /// Keyframed shutters: out-of-range values are named, and so is a
    /// constant the keyframes silently override.
    #[test]
    fn keyframed_shutter_conflicts_and_ranges_are_named() {
        let clip = |extra_layer: &str, extra_kf: &str| {
            format!(
                r#"{{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0A05","name":"Clip",
                     "sortIndex":0,"kind":"video","isEnabled":true,
                     "startTime":0,"duration":4{extra_layer},
                     "keyframes":[{{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0A06",
                       "time":2,"transitionDuration":1{extra_kf}}}]}}"#
            )
        };
        let warned = |layer: &str, kf: &str| {
            warnings(&project(&clip(layer, kf), r#","minReaderVersion":10"#))
        };
        assert!(warned("", r#","shutter":2.0"#)
            .iter()
            .any(|w| w.contains("outside 0..1")));
        assert!(
            warned(r#","motionBlur":{"shutter":0.5}"#, r#","shutter":0.7"#)
                .iter()
                .any(|w| w.contains("keyframes win"))
        );
        assert!(
            warned("", r#","shutter":0.7"#).is_empty(),
            "a ramp on its own is exactly right"
        );
    }

    /// A tint needs both halves, and a constant the keyframes shadow gets
    /// named — the same honesty the shutter's warnings give.
    #[test]
    fn a_half_stated_tint_and_a_shadowed_constant_are_named() {
        let clip = |adjust: &str, kf: &str| {
            format!(
                r#"{{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0A07","name":"Clip",
                     "sortIndex":0,"kind":"video","isEnabled":true,
                     "startTime":0,"duration":4,"adjustments":{adjust},
                     "keyframes":[{{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0A08",
                       "time":2,"transitionDuration":1{kf}}}]}}"#
            )
        };
        let warned = |adjust: &str, kf: &str| {
            warnings(&project(&clip(adjust, kf), r#","minReaderVersion":11"#))
        };
        assert!(warned(r#"{"tintHex":"FF8000"}"#, "")
            .iter()
            .any(|w| w.contains("never applied")));
        assert!(warned(r#"{"tintAmount":0.5}"#, "")
            .iter()
            .any(|w| w.contains("no tintHex")));
        assert!(warned(r#"{"saturation":0.5}"#, r#","saturation":0"#)
            .iter()
            .any(|w| w.contains("keyframes win")));
        assert!(
            warned(r#"{"saturation":0.5}"#, "").is_empty(),
            "a plain constant grade is exactly right"
        );
    }

    /// Blend on a background or audio layer is a knob wired to nothing.
    #[test]
    fn a_blend_mode_that_cannot_act_is_named() {
        let bg = r#"{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0A09","name":"BG",
                     "sortIndex":0,"kind":"background","isEnabled":true,
                     "startTime":0,"duration":4,"blendMode":"screen","keyframes":[]}"#;
        assert!(warnings(&project(bg, r#","minReaderVersion":12"#))
            .iter()
            .any(|w| w.contains("does nothing")));
        let clip = r#"{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0A10","name":"Glow",
                       "sortIndex":0,"kind":"image","isEnabled":true,
                       "startTime":0,"duration":4,"blendMode":"screen","keyframes":[]}"#;
        assert!(
            warnings(&project(clip, r#","minReaderVersion":12"#)).is_empty(),
            "screen on an image is exactly what the mode is for"
        );
    }

    /// Every way a mask can silently not act gets a voice: a missing
    /// resource, a resource of the wrong kind, an inkless drawing, a layer
    /// kind masks skip, and an invert with nothing to flip.
    #[test]
    fn a_mask_that_cannot_act_is_named() {
        let masked = |kind: &str, mask: &str| {
            format!(
                r#"{{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0A20","name":"Shot",
                     "sortIndex":0,"kind":"{kind}","isEnabled":true,
                     "startTime":0,"duration":4{mask},"keyframes":[]}}"#
            )
        };
        let resources = r#","minReaderVersion":13,"resources":[
            {"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0A21","kind":"drawing",
             "filename":"m.json","displayName":"Oval","addedAt":0,
             "drawing":{"shapes":[{"id":"S","kind":"oval",
                 "points":[[0.0,0.0],[10.0,10.0]],"strokeColorHex":"FFFFFF",
                 "strokeWidth":1.0,"fillColorHex":"FFFFFF",
                 "arrowStart":false,"arrowEnd":false}]}},
            {"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0A22","kind":"drawing",
             "filename":"e.json","displayName":"Empty","addedAt":0,
             "drawing":{"shapes":[]}},
            {"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0A23","kind":"image",
             "filename":"a.png","displayName":"Pic","addedAt":0}]"#;
        let mask =
            |rid: &str| format!(r#","maskResourceID":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0A{rid}""#);

        // The happy path stays silent.
        assert!(
            warnings(&project(&masked("image", &mask("21")), resources)).is_empty(),
            "a drawing mask on an image layer is the feature"
        );
        assert!(warnings(&project(&masked("image", &mask("99")), resources))
            .iter()
            .any(|w| w.contains("names no resource")));
        assert!(warnings(&project(&masked("image", &mask("23")), resources))
            .iter()
            .any(|w| w.contains("not a drawing")));
        assert!(warnings(&project(&masked("image", &mask("22")), resources))
            .iter()
            .any(|w| w.contains("no shapes")));
        assert!(
            warnings(&project(&masked("caption", &mask("21")), resources))
                .iter()
                .any(|w| w.contains("masks window video and image layers"))
        );
        assert!(warnings(&project(
            &masked("image", r#","maskInverted":true"#),
            resources
        ))
        .iter()
        .any(|w| w.contains("without a maskResourceID")));
    }

    /// Flying a window that does not exist, and collapsing one that does.
    #[test]
    fn a_flightless_or_collapsed_mask_is_named() {
        let flier = |mask: &str, keyframe: &str| {
            format!(
                r#"{{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0A30","name":"Shot",
                     "sortIndex":0,"kind":"image","isEnabled":true,
                     "startTime":0,"duration":4{mask},
                     "keyframes":[{{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0A31",
                        "time":1,"transitionDuration":0{keyframe}}}]}}"#
            )
        };
        let resources = r#","minReaderVersion":14,"resources":[
            {"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0A21","kind":"drawing",
             "filename":"m.json","displayName":"Oval","addedAt":0,
             "drawing":{"shapes":[{"id":"S","kind":"oval",
                 "points":[[0.0,0.0],[10.0,10.0]],"strokeColorHex":"FFFFFF",
                 "strokeWidth":1.0,"fillColorHex":"FFFFFF",
                 "arrowStart":false,"arrowEnd":false}]}}]"#;
        let mask = r#","maskResourceID":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0A21""#;

        assert!(
            warnings(&project(&flier(mask, r#","maskRotation":45"#), resources)).is_empty(),
            "a flying window over a real mask is the feature"
        );
        assert!(
            warnings(&project(&flier("", r#","maskOffsetX":40"#), resources))
                .iter()
                .any(|w| w.contains("no window to fly"))
        );
        assert!(
            warnings(&project(&flier(mask, r#","maskZoom":0"#), resources))
                .iter()
                .any(|w| w.contains("collapses"))
        );
    }
}

#[cfg(test)]
mod palette_tests {
    use super::*;

    /// `settings` and `resources`/`layers` fragments, spliced into a file.
    fn project(settings: &str, resources: &str, layers: &str) -> ProjectMetadata {
        ProjectMetadata::from_json(&format!(
            // Declares 17 throughout: a palette RESOURCE needs that rung,
            // and an under-declared file warns about it, which would drown
            // out what these tests are actually looking at.
            r#"{{"id":"AAAAAAAA-0000-0000-0000-00000000AAAA","name":"v","createdAt":0,
                 "state":"recorded","trimStart":0,"trimEnd":0,"videoDuration":0,
                 "subtitles":[],"minReaderVersion":17,
                 "compositionSettings":{{"canvasWidth":1920,"canvasHeight":1080{settings}}},
                 "resources":[{resources}],"layers":[{layers}]}}"#
        ))
        .expect("fixture")
    }

    const PALETTE: &str = r#","palette":[{"name":"text","colorHex":"FFFFFF"}]"#;

    fn caption(colour: &str) -> String {
        format!(
            r#"{{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0A03","name":"C","sortIndex":0,
                 "kind":"caption","isEnabled":true,"startTime":0,"keyframes":[],
                 "captionStyle":{{"textColorHex":"{colour}"}}}}"#
        )
    }

    #[test]
    fn an_undefined_name_is_named() {
        let warnings = warnings(&project(PALETTE, "", &caption("@brand")));
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("\"@brand\"") && w.contains("renders black")),
            "{warnings:?}"
        );
    }

    #[test]
    fn a_defined_name_says_nothing() {
        assert!(warnings(&project(PALETTE, "", &caption("@text"))).is_empty());
    }

    /// A palette entry is a DEFINITION. If its own name counted as a use,
    /// every palette would report itself.
    #[test]
    fn the_palettes_own_entries_are_not_uses() {
        let defs = r#","palette":[{"name":"text","colorHex":"@text"}]"#;
        assert!(warnings(&project(defs, "", "")).is_empty());
    }

    /// The reason the walk runs over the SERIALIZED document: these two
    /// fields are resolved by the engine but were invisible to the app's
    /// hand-written colour walk for as long as it existed. A field-by-field
    /// validator would have the same blind spot.
    #[test]
    fn deep_fields_are_walked_too() {
        let tinted = r#"{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0A04","name":"L","sortIndex":0,
             "kind":"image","isEnabled":true,"startTime":0,"keyframes":[],
             "adjustments":{"tintHex":"@warm","tintAmount":0.5}}"#;
        let reveal = r#"{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0A05","name":"K","sortIndex":1,
             "kind":"caption","isEnabled":true,"startTime":0,"keyframes":[],
             "captionStyle":{"reveal":{"mode":"highlight","highlightColorHex":"@pop"}}}"#;
        let found = warnings(&project(PALETTE, "", &format!("{tinted},{reveal}")));
        assert!(found.iter().any(|w| w.contains("@warm")), "{found:?}");
        assert!(found.iter().any(|w| w.contains("@pop")), "{found:?}");
    }

    /// The CLI renders the file as it stands. When the settings copy has
    /// drifted from the resource it follows, that is what it draws.
    #[test]
    fn a_stale_settings_copy_is_named() {
        let settings = concat!(
            r#","palette":[{"name":"text","colorHex":"000000"}]"#,
            r#","paletteResourceID":"CCCCCCCC-0000-4000-8000-000000000041""#
        );
        let resource = r#"{"id":"CCCCCCCC-0000-4000-8000-000000000041","kind":"palette",
             "filename":"","displayName":"Studio Dark","addedAt":0,
             "palette":[{"name":"text","colorHex":"FFFFFF"}]}"#;
        let found = warnings(&project(settings, resource, ""));
        assert!(
            found.iter().any(|w| w.contains("differs from the palette")),
            "{found:?}"
        );

        // Agreeing says nothing.
        let agreeing = concat!(
            r#","palette":[{"name":"text","colorHex":"FFFFFF"}]"#,
            r#","paletteResourceID":"CCCCCCCC-0000-4000-8000-000000000041""#
        );
        let quiet = warnings(&project(agreeing, resource, ""));
        assert!(quiet.is_empty(), "{quiet:?}");
    }

    #[test]
    fn a_selection_naming_nothing_is_named() {
        let settings = r#","paletteResourceID":"CCCCCCCC-0000-4000-8000-0000000000FF""#;
        let found = warnings(&project(settings, "", ""));
        assert!(
            found.iter().any(|w| w.contains("not a resource")),
            "{found:?}"
        );
    }
}

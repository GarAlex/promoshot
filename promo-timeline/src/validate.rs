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
            let shorthand = if side == "transitionIn" { "fadeIn" } else { "fadeOut" };
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

    // A reveal states its pace one way or the other. Both is not an error —
    // the total wins — but it is certainly not what the author meant.
    let mut reveal_conflict = |where_: String, reveal: &promo_model::TextReveal| {
        if let (Some(per), Some(total)) = (reveal.seconds_per, reveal.seconds) {
            out.push(format!(
                "{where_}: reveal states secondsPer {per} AND seconds {total} — \
                 the total wins and the rate is ignored"
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
            r#"{{"id":"L","name":"Clip","sortIndex":0,"kind":"{kind}","isEnabled":true,
                 "startTime":0,"duration":4,
                 "keyframes":[{{"id":"K","time":2,"transitionDuration":0{keyframe}}}]}}"#
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
        assert!(warnings[0].contains("layer \"Clip\" at 2s"), "{}", warnings[0]);
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
            warnings.iter().any(|w| w.contains("minReaderVersion") && w.contains('6')),
            "{warnings:?}"
        );
    }
}

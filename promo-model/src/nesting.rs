//! Nested compositions: the rules that keep recursion finite, and the walk
//! every reader of nested layers shares.
//!
//! A composition resource carries layers that reference the parent
//! project's resources by id, and one of those layers may show another
//! composition. Two rules make that safe to render by recursion: a
//! composition may not contain itself, directly or through others, and
//! the nesting is no deeper than [`MAX_DEPTH`].

use crate::{
    ProjectLayer, ProjectLayerKind, ProjectMetadata, ProjectResource, ProjectResourceKind,
};

/// The deepest a composition may sit inside compositions. A title card in
/// a section in a reel is three; eight is more than any cut needs and
/// small enough that a runaway file fails fast.
pub const MAX_DEPTH: usize = 8;

/// The composition a layer shows at rest, if it shows one.
pub fn composition_of<'a>(
    layer: &ProjectLayer,
    resources: &'a [ProjectResource],
) -> Option<&'a ProjectResource> {
    let id = layer.resource_id.as_deref()?;
    resources
        .iter()
        .find(|r| r.id == id && r.kind == ProjectResourceKind::Composition)
}

/// Every layer a renderer will be asked about: the project's, then each
/// composition's, nested ones included — the walk a host makes when it
/// prepares sources per layer, so a request for a nested layer's frame
/// finds what it needs.
pub fn all_layers(meta: &ProjectMetadata) -> Vec<&ProjectLayer> {
    // A stage layer's members (rung 33) are layers too — they play media
    // the renderers must be given — so they follow their stage here.
    fn with_members<'a>(layers: &'a [ProjectLayer], out: &mut Vec<&'a ProjectLayer>) {
        for layer in layers {
            out.push(layer);
            if let Some(members) = layer.members.as_deref() {
                out.extend(members.iter());
            }
        }
    }
    let mut out: Vec<&ProjectLayer> = Vec::new();
    with_members(meta.layers.as_deref().unwrap_or(&[]), &mut out);
    for resource in meta.resources.as_deref().unwrap_or(&[]) {
        if let Some(composition) = resource.composition.as_ref() {
            with_members(&composition.layers, &mut out);
        }
    }
    out
}

/// Every problem that makes the nesting unrenderable or meaningless, in
/// words. Empty means the recursion is finite and every nested layer names
/// a resource the project has.
pub fn problems(meta: &ProjectMetadata) -> Vec<String> {
    let resources = meta.resources.as_deref().unwrap_or(&[]);
    let mut out = Vec::new();
    for resource in resources
        .iter()
        .filter(|r| r.kind == ProjectResourceKind::Composition)
    {
        let Some(composition) = resource.composition.as_ref() else {
            out.push(format!(
                "composition resource {} ({}) carries no `composition` document",
                resource.display_name, resource.id
            ));
            continue;
        };
        if composition.canvas_width <= 0.0 || composition.canvas_height <= 0.0 {
            out.push(format!(
                "composition {} has no canvas ({}x{})",
                resource.display_name, composition.canvas_width, composition.canvas_height
            ));
        }
        let mut trail = vec![resource.id.as_str()];
        walk(resource, resources, &mut trail, &mut out);
    }
    // A composition is shown by a video-kind layer: the clock, trims and
    // cuts are a clip's. Any other kind would draw nothing and say nothing.
    let mut check_placements = |layers: &[ProjectLayer], whose: &str| {
        for layer in layers {
            if let Some(shown) = composition_of(layer, resources) {
                if layer.kind != ProjectLayerKind::Video {
                    out.push(format!(
                        "{whose}layer {} ({}) shows composition {} but is a {:?} layer; a composition is shown by a video layer",
                        layer.name, layer.id, shown.display_name, layer.kind
                    ));
                }
            }
        }
    };
    check_placements(meta.layers.as_deref().unwrap_or(&[]), "");
    for resource in resources
        .iter()
        .filter(|r| r.kind == ProjectResourceKind::Composition)
    {
        if let Some(composition) = resource.composition.as_ref() {
            check_placements(
                &composition.layers,
                &format!("in composition {}: ", resource.display_name),
            );
        }
    }
    out
}

fn walk<'a>(
    resource: &'a ProjectResource,
    resources: &'a [ProjectResource],
    trail: &mut Vec<&'a str>,
    out: &mut Vec<String>,
) {
    let Some(composition) = resource.composition.as_ref() else {
        return;
    };
    if trail.len() > MAX_DEPTH {
        out.push(format!(
            "composition {} nests deeper than {} ({})",
            resource.display_name,
            MAX_DEPTH,
            trail.join(" > ")
        ));
        return;
    }
    for layer in &composition.layers {
        let Some(id) = layer.resource_id.as_deref() else {
            continue;
        };
        let Some(shown) = resources.iter().find(|r| r.id == id) else {
            out.push(format!(
                "in composition {}: layer {} ({}) references unknown resource {id}",
                resource.display_name, layer.name, layer.id
            ));
            continue;
        };
        if shown.kind != ProjectResourceKind::Composition {
            continue;
        }
        if trail.contains(&shown.id.as_str()) {
            out.push(format!(
                "composition {} contains itself ({} > {})",
                shown.display_name,
                trail.join(" > "),
                shown.id
            ));
            continue;
        }
        trail.push(shown.id.as_str());
        walk(shown, resources, trail, out);
        trail.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(resources: serde_json::Value, layers: serde_json::Value) -> ProjectMetadata {
        let json = serde_json::json!({
            "id": "P", "name": "p", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
            "compositionSettings": {"canvasWidth": 1920, "canvasHeight": 1080},
            "resources": resources, "layers": layers
        });
        ProjectMetadata::from_json(&json.to_string()).unwrap()
    }
    fn comp(id: &str, layers: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"id": id, "kind": "composition", "filename": "", "displayName": id,
            "addedAt": 0, "duration": 4, "pixelWidth": 1920, "pixelHeight": 1080, "imageCuts": [],
            "composition": {"canvasWidth": 1920, "canvasHeight": 1080, "layers": layers}})
    }
    fn video_layer(id: &str, resource: &str) -> serde_json::Value {
        serde_json::json!({"id": id, "name": id, "sortIndex": 0, "kind": "video", "isEnabled": true,
            "startTime": 0, "duration": 4, "resourceID": resource, "keyframes": []})
    }

    #[test]
    fn a_composition_resource_gates_at_nineteen_and_round_trips() {
        let meta = doc(
            serde_json::json!([comp("A", serde_json::json!([video_layer("L", "clip")])),
                {"id": "clip", "kind": "video", "filename": "c.mp4", "displayName": "c", "addedAt": 0, "duration": 4, "imageCuts": []}]),
            serde_json::json!([video_layer("P", "A")]),
        );
        assert_eq!(meta.minimum_reader_version(), 19);
        let again = ProjectMetadata::from_json(&meta.to_json().unwrap()).unwrap();
        assert_eq!(again, meta);
        assert!(problems(&meta).is_empty(), "{:?}", problems(&meta));
        let shown = composition_of(
            &meta.layers.as_deref().unwrap()[0],
            meta.resources.as_deref().unwrap(),
        )
        .unwrap();
        assert_eq!(shown.composition.as_ref().unwrap().layers.len(), 1);
    }

    #[test]
    fn a_composition_may_not_contain_itself() {
        let meta = doc(
            serde_json::json!([
                comp("A", serde_json::json!([video_layer("L", "B")])),
                comp("B", serde_json::json!([video_layer("M", "A")]))
            ]),
            serde_json::json!([video_layer("P", "A")]),
        );
        let found = problems(&meta);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].contains("contains itself"), "{found:?}");
    }

    #[test]
    fn nesting_is_capped_and_unknown_references_are_named() {
        let mut resources = Vec::new();
        for depth in 0..(MAX_DEPTH + 2) {
            let inner = format!("C{}", depth + 1);
            resources.push(comp(
                &format!("C{depth}"),
                serde_json::json!([video_layer(&format!("L{depth}"), &inner)]),
            ));
        }
        resources.push(comp(&format!("C{}", MAX_DEPTH + 2), serde_json::json!([])));
        let meta = doc(
            serde_json::Value::Array(resources),
            serde_json::json!([video_layer("P", "C0")]),
        );
        let found = problems(&meta);
        assert!(
            found.iter().any(|p| p.contains("nests deeper than")),
            "{found:?}"
        );

        let meta = doc(
            serde_json::json!([comp("A", serde_json::json!([video_layer("L", "ghost")]))]),
            serde_json::json!([video_layer("P", "A")]),
        );
        let found = problems(&meta);
        assert!(
            found.iter().any(|p| p.contains("unknown resource ghost")),
            "{found:?}"
        );
    }

    #[test]
    fn a_composition_is_shown_by_a_video_layer() {
        let meta = doc(
            serde_json::json!([comp("A", serde_json::json!([]))]),
            serde_json::json!([{"id": "P", "name": "p", "sortIndex": 0, "kind": "image", "isEnabled": true,
                "startTime": 0, "duration": 4, "resourceID": "A", "keyframes": []}]),
        );
        let found = problems(&meta);
        assert!(
            found
                .iter()
                .any(|p| p.contains("is shown by a video layer")),
            "{found:?}"
        );
    }
    /// Markers round-trip, an unknown kind reads as a marker, and a project
    /// with any marker gates at 20.
    #[test]
    fn markers_round_trip_and_gate_at_twenty() {
        let json = serde_json::json!({
            "id": "P", "name": "p", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
            "compositionSettings": {"canvasWidth": 1920, "canvasHeight": 1080},
            "markers": [
                {"id": "M1", "time": 2.5, "name": "Pricing", "kind": "chapter", "colorHex": "@accent"},
                {"id": "M2", "time": 4.0, "name": "note"},
                {"id": "M3", "time": 6.0, "name": "future", "kind": "bookmark"}
            ]
        });
        let meta = ProjectMetadata::from_json(&json.to_string()).unwrap();
        let markers = meta.markers.as_deref().unwrap();
        assert_eq!(markers.len(), 3);
        assert_eq!(markers[0].kind, crate::MarkerKind::Chapter);
        assert_eq!(markers[1].kind, crate::MarkerKind::Marker);
        assert_eq!(
            markers[2].kind,
            crate::MarkerKind::Marker,
            "unknown kinds read as markers"
        );
        assert_eq!(meta.minimum_reader_version(), 20);
        let again = ProjectMetadata::from_json(&meta.to_json().unwrap()).unwrap();
        assert_eq!(
            again.markers.as_deref().unwrap()[0].color_hex.as_deref(),
            Some("@accent")
        );
        let plain: ProjectMetadata = ProjectMetadata::from_json(
            &serde_json::json!({
                "id": "P", "name": "p", "createdAt": 0, "state": "recorded",
                "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
                "compositionSettings": {"canvasWidth": 1920, "canvasHeight": 1080}
            })
            .to_string(),
        )
        .unwrap();
        assert!(plain.minimum_reader_version() < 20);
    }
    /// Audio effects round-trip, an unknown kind reads as none, and a
    /// resource with any effect gates the project at 21.
    #[test]
    fn audio_effects_round_trip_and_gate_at_twenty_one() {
        let meta = doc(
            serde_json::json!([{"id": "V", "kind": "video", "filename": "v.mp4", "displayName": "v", "addedAt": 0,
                "duration": 4, "imageCuts": [],
                "audioEffects": [{"kind": "normalize", "targetLufs": -14}, {"kind": "reverb"}, {"kind": "eq", "frequencyHz": 200, "gainDb": -3}]}]),
            serde_json::json!([]),
        );
        let effects = meta.resources.as_deref().unwrap()[0]
            .audio_effects
            .as_deref()
            .unwrap();
        assert_eq!(effects.len(), 3);
        assert_eq!(effects[0].kind, crate::AudioEffectKind::Normalize);
        assert_eq!(effects[0].target_lufs, Some(-14.0));
        assert_eq!(
            effects[1].kind,
            crate::AudioEffectKind::None,
            "unknown effects are skipped"
        );
        assert_eq!(meta.minimum_reader_version(), 21);
        let again = ProjectMetadata::from_json(&meta.to_json().unwrap()).unwrap();
        assert_eq!(again.resources, meta.resources);
    }
}

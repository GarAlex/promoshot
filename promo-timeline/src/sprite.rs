//! Which frame of a sprite sheet is showing, and how big one frame is.
//!
//! The rule lives here rather than in a renderer because both the preview and
//! the export build their own scenes: two implementations of "which cell" is
//! exactly how a sprite would come to animate at one speed on screen and
//! another in the file. Callers ask, then draw.
//!
//! A sprite's material is a CYCLE, unlike a recording, so it repeats by
//! default — the opposite of a video layer, whose default is to freeze on its
//! last frame. A layer can still say otherwise: `hold` stops on the final
//! frame once the cycle is spent, and `hide` stops drawing entirely.

use promo_model::{BeyondEnd, ProjectLayer, ProjectResource, ResourceSampling, Size, SpriteSheet};

/// One frame of a sprite sheet: where it sits in the texture and how big it
/// is in pixels. The SIZE is what a layer lays out against — a walk cycle in
/// a 4×2 sheet places as one 64×64 frame, never as the 256×128 sheet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpriteFrame {
    /// Which frame it is, counting from zero. Carried rather than left to the
    /// caller to recompute: `hold` shows the last frame long after the
    /// cycling arithmetic would have moved on, so a second derivation is a
    /// second answer.
    pub index: u32,
    /// `[u, v, width, height]` in 0…1, for the compositor's `uv_rect`.
    pub uv_rect: [f64; 4],
    /// One cell's size in source pixels.
    pub cell: Size,
}

/// The sheet a resource carries, but only where it means something: a sprite
/// is a way of reading an IMAGE, and a video or an audio file with a stray
/// `sprite` key is a mistake rather than an instruction.
pub fn sheet_for(resource: &ProjectResource) -> Option<&SpriteSheet> {
    match resource.kind {
        promo_model::ProjectResourceKind::Image => resource.sprite.as_ref(),
        _ => None,
    }
}

/// Whether this resource is drawn without smoothing.
pub fn is_nearest(resource: Option<&ProjectResource>) -> bool {
    matches!(
        resource.and_then(|r| r.sampling),
        Some(ResourceSampling::Nearest)
    )
}

/// The frame showing `local_time` seconds into a layer, or `None` when the
/// layer has run out of animation and asked to disappear.
///
/// `sheet_size` is the whole image in pixels — the cell size is derived, so a
/// caller cannot disagree with the grid about how big a frame is.
pub fn frame_at(
    sheet: &SpriteSheet,
    layer: &ProjectLayer,
    local_time: f64,
    sheet_size: Size,
) -> Option<SpriteFrame> {
    let cycle = sheet.cycle_duration();
    let spent = local_time >= cycle;
    let index = match layer.beyond_end {
        // Only once the cycle is actually spent; before that every mode
        // plays the animation normally.
        Some(BeyondEnd::Hide) if spent => return None,
        Some(BeyondEnd::Hold) if spent => sheet.frames().saturating_sub(1),
        _ => sheet.frame_at(local_time),
    };
    Some(SpriteFrame {
        index,
        uv_rect: sheet.uv_rect(index),
        cell: Size::new(
            sheet_size.width() / sheet.columns.max(1) as f64,
            sheet_size.height() / sheet.rows.max(1) as f64,
        ),
    })
}

/// What a layer SHOWS at a given time.
///
/// A keyframe may carry a `resourceID`, swapping the layer's source without
/// touching anything else: position, zoom, rotation and opacity go on ramping
/// straight through it, which is the whole point — the alternative is a second
/// layer with every keyframe duplicated.
///
/// Three rules, all of them refusals:
///
/// - The swap is a STEP. It lands at the keyframe's own time, not at the
///   start of a ramp, and no transition applies to it.
/// - Only image and caption layers honour swaps. On video or audio the
///   layer's local time maps to source time through trims, speed and cuts, so
///   a mid-layer swap would have to answer "where does the second clip
///   start" — a sequence model, not a keyframe field.
/// - The new resource must be of the layer's OWN kind. Anything else is
///   ignored rather than drawn, because an image layer handed a video has no
///   sensible thing to do with it.
/// The resource `id` names, if this layer could actually swap to it —
/// it exists and is the kind this layer draws.
///
/// The one rule, so `layer_resource_id` and `transition::active_swap` cannot
/// answer differently about the same keyframe.
pub fn swappable<'a>(
    layer: &ProjectLayer,
    id: &str,
    resources: &'a [ProjectResource],
) -> Option<&'a ProjectResource> {
    let wanted = swappable_kind(layer)?;
    resources.iter().find(|r| r.id == id && r.kind == wanted)
}

/// The resource kind this layer's swaps must name, or `None` for a kind that
/// does not swap at all.
pub fn swappable_kind(layer: &ProjectLayer) -> Option<promo_model::ProjectResourceKind> {
    match layer.kind {
        promo_model::ProjectLayerKind::Image => Some(promo_model::ProjectResourceKind::Image),
        promo_model::ProjectLayerKind::Caption => Some(promo_model::ProjectResourceKind::Caption),
        promo_model::ProjectLayerKind::Drawing => Some(promo_model::ProjectResourceKind::Drawing),
        _ => None,
    }
}

pub fn layer_resource_id<'a>(
    layer: &'a ProjectLayer,
    time: f64,
    resources: &[ProjectResource],
) -> Option<&'a str> {
    let base = layer.resource_id.as_deref();
    // A drawing swaps like an image: the vector document is just as
    // replaceable as a bitmap. Video is deliberately absent — a mid-layer
    // swap there is a playlist, and it would have to answer where the second
    // clip starts and what happens to its audio.
    let Some(wanted) = swappable_kind(layer) else {
        return base;
    };
    let local = crate::layer_local_time(layer, time);
    let mut swaps: Vec<&promo_model::ProjectLayerKeyframe> = layer
        .keyframes
        .iter()
        .filter(|k| k.resource_id.is_some())
        .collect();
    swaps.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap_or(std::cmp::Ordering::Equal));
    swaps
        .iter()
        .rev()
        .find(|k| {
            k.time <= local
                && k.resource_id.as_deref().is_some_and(|id| {
                    resources.iter().any(|r| r.id == id && r.kind == wanted)
                })
        })
        .and_then(|k| k.resource_id.as_deref())
        .or(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(beyond_end: &str) -> ProjectLayer {
        let end = if beyond_end.is_empty() {
            String::new()
        } else {
            format!(r#""beyondEnd": "{beyond_end}","#)
        };
        serde_json::from_str(&format!(
            r#"{{"id": "L", "name": "L", "sortIndex": 0, "kind": "image",
                 "isEnabled": true, "startTime": 0, {end} "keyframes": []}}"#
        ))
        .expect("layer")
    }

    fn resource(json: &str) -> ProjectResource {
        serde_json::from_str(json).expect("resource")
    }

    fn sheet() -> SpriteSheet {
        SpriteSheet {
            columns: 2,
            rows: 2,
            frame_count: Some(4),
            fps: Some(10.0),
            frame_durations: None,
        }
    }

    #[test]
    fn a_frame_is_one_cell_of_the_sheet_not_the_whole_image() {
        let frame = frame_at(&sheet(), &layer(""), 0.0, Size::new(256.0, 128.0)).unwrap();
        assert_eq!(frame.cell, Size::new(128.0, 64.0));
        assert_eq!(frame.uv_rect, [0.0, 0.0, 0.5, 0.5]);
    }

    /// The asymmetry with video, stated as a test: a recording freezes on its
    /// last frame by default, a sprite sheet is a loop and keeps going.
    #[test]
    fn a_sprite_repeats_by_default_and_obeys_the_layer_when_told_otherwise() {
        let (s, size) = (sheet(), Size::new(256.0, 128.0));
        // One cycle is 0.4s at 10fps, so 0.45s is 0.05s into the NEXT one:
        // back to frame 0, and 0.55s is frame 1.
        assert_eq!(frame_at(&s, &layer(""), 0.45, size).unwrap().uv_rect, s.uv_rect(0));
        assert_eq!(frame_at(&s, &layer(""), 0.55, size).unwrap().uv_rect, s.uv_rect(1));
        assert_eq!(frame_at(&s, &layer("loop"), 0.55, size).unwrap().uv_rect, s.uv_rect(1));
        // Hold stops on the last frame rather than starting over.
        assert_eq!(frame_at(&s, &layer("hold"), 0.45, size).unwrap().uv_rect, s.uv_rect(3));
        assert_eq!(frame_at(&s, &layer("hold"), 90.0, size).unwrap().uv_rect, s.uv_rect(3));
        // Hide draws nothing at all — but only after the animation has run.
        assert!(frame_at(&s, &layer("hide"), 0.45, size).is_none());
        assert!(frame_at(&s, &layer("hide"), 0.2, size).is_some(), "mid-cycle");
    }

    #[test]
    fn only_an_image_is_read_as_a_sheet() {
        let sprite = r#""sprite":{"columns":2,"rows":2}"#;
        let image = resource(&format!(
            r#"{{"id":"R","kind":"image","filename":"s.png","displayName":"S",
                 "addedAt":0,"imageCuts":[],"disabledAudioTrackIndices":[],{sprite}}}"#
        ));
        assert!(sheet_for(&image).is_some());
        // A stray key on a video is a mistake, not an instruction to slice it.
        let video = resource(&format!(
            r#"{{"id":"R","kind":"video","filename":"v.mp4","displayName":"V",
                 "addedAt":0,"imageCuts":[],"disabledAudioTrackIndices":[],{sprite}}}"#
        ));
        assert!(sheet_for(&video).is_none());
    }

    /// The swap, and the three things it refuses to do.
    #[test]
    fn a_keyframe_can_change_what_a_layer_shows() {
        let resources: Vec<ProjectResource> = serde_json::from_str(
            r#"[{"id":"A","kind":"image","filename":"a.png","displayName":"A",
                 "addedAt":0,"imageCuts":[],"disabledAudioTrackIndices":[]},
                {"id":"B","kind":"image","filename":"b.png","displayName":"B",
                 "addedAt":0,"imageCuts":[],"disabledAudioTrackIndices":[]},
                {"id":"V","kind":"video","filename":"v.mp4","displayName":"V",
                 "addedAt":0,"imageCuts":[],"disabledAudioTrackIndices":[]}]"#,
        )
        .unwrap();
        let swapping: ProjectLayer = serde_json::from_str(
            r#"{"id":"L","name":"L","sortIndex":0,"kind":"image","isEnabled":true,
                "startTime":0,"resourceID":"A","keyframes":[
                  {"id":"K0","time":0,"zoom":1,"horizontalShift":0,
                   "verticalShift":0,"transitionDuration":0},
                  {"id":"K1","time":2,"resourceID":"B","transitionDuration":0},
                  {"id":"K2","time":4,"zoom":2,"horizontalShift":400,
                   "verticalShift":0,"transitionDuration":2}]}"#,
        )
        .unwrap();

        // The layer's own resource holds until the swap, which lands at the
        // keyframe's OWN time rather than at the start of any ramp.
        assert_eq!(layer_resource_id(&swapping, 0.0, &resources), Some("A"));
        assert_eq!(layer_resource_id(&swapping, 1.999, &resources), Some("A"));
        assert_eq!(layer_resource_id(&swapping, 2.0, &resources), Some("B"));
        assert_eq!(layer_resource_id(&swapping, 99.0, &resources), Some("B"));

        // A swap to the wrong KIND is ignored, not drawn: an image layer
        // handed a video has nothing sensible to do with it.
        let mismatched: ProjectLayer = serde_json::from_str(
            r#"{"id":"L","name":"L","sortIndex":0,"kind":"image","isEnabled":true,
                "startTime":0,"resourceID":"A","keyframes":[
                  {"id":"K","time":1,"resourceID":"V","transitionDuration":0}]}"#,
        )
        .unwrap();
        assert_eq!(layer_resource_id(&mismatched, 5.0, &resources), Some("A"));

        // Video and audio layers do not honour swaps at all — that would make
        // the layer a playlist and leave "where does the second clip start"
        // unanswered.
        let video: ProjectLayer = serde_json::from_str(
            r#"{"id":"L","name":"L","sortIndex":0,"kind":"video","isEnabled":true,
                "startTime":0,"resourceID":"V","keyframes":[
                  {"id":"K","time":1,"resourceID":"A","transitionDuration":0}]}"#,
        )
        .unwrap();
        assert_eq!(layer_resource_id(&video, 5.0, &resources), Some("V"));

        // A swap naming something the project does not have falls back rather
        // than drawing nothing.
        let gone: ProjectLayer = serde_json::from_str(
            r#"{"id":"L","name":"L","sortIndex":0,"kind":"image","isEnabled":true,
                "startTime":0,"resourceID":"A","keyframes":[
                  {"id":"K","time":1,"resourceID":"GONE","transitionDuration":0}]}"#,
        )
        .unwrap();
        assert_eq!(layer_resource_id(&gone, 5.0, &resources), Some("A"));
    }

    /// The layer's start time shifts the swap with it: keyframe times are
    /// LOCAL, and a layer dragged along the timeline must not change what it
    /// shows when.
    #[test]
    fn swap_times_are_local_to_the_layer() {
        let resources: Vec<ProjectResource> = serde_json::from_str(
            r#"[{"id":"A","kind":"image","filename":"a.png","displayName":"A",
                 "addedAt":0,"imageCuts":[],"disabledAudioTrackIndices":[]},
                {"id":"B","kind":"image","filename":"b.png","displayName":"B",
                 "addedAt":0,"imageCuts":[],"disabledAudioTrackIndices":[]}]"#,
        )
        .unwrap();
        let layer: ProjectLayer = serde_json::from_str(
            r#"{"id":"L","name":"L","sortIndex":0,"kind":"image","isEnabled":true,
                "startTime":10,"resourceID":"A","keyframes":[
                  {"id":"K","time":2,"resourceID":"B","transitionDuration":0}]}"#,
        )
        .unwrap();
        assert_eq!(layer_resource_id(&layer, 11.0, &resources), Some("A"));
        assert_eq!(layer_resource_id(&layer, 12.0, &resources), Some("B"));
    }

    #[test]
    fn sampling_defaults_to_smooth() {
        let plain = resource(
            r#"{"id":"R","kind":"image","filename":"a.png","displayName":"A",
                "addedAt":0,"imageCuts":[],"disabledAudioTrackIndices":[]}"#,
        );
        assert!(!is_nearest(Some(&plain)));
        assert!(!is_nearest(None), "a layer with no resource is not pixel art");
        let crisp = resource(
            r#"{"id":"R","kind":"image","filename":"a.png","displayName":"A",
                "addedAt":0,"imageCuts":[],"disabledAudioTrackIndices":[],
                "sampling":"nearest"}"#,
        );
        assert!(is_nearest(Some(&crisp)));
    }
}

//! Speech timing on the output timeline (docs/SPEECH-SYNC.md §6.3): a
//! take's source seconds placed through the layer that plays it. Slice 5
//! carries `promo words --timeline` (both hosts); the speech clock and
//! speech values build on it in slice 6.

/// A take's word times on the output timeline, through the first enabled
/// audio or video layer that plays it (by start time): its start plus the
/// resource's source-to-timeline mapping through the layer's media cut —
/// trims, speed, cuts. A word
/// the layer never plays is None.
/// The layer a take is mapped through, and each word's output start and
/// end (None where the layer never plays it).
pub type TimelineWords = (String, Vec<Option<(f64, f64)>>);

pub fn timeline_words(
    meta: &promo_model::ProjectMetadata,
    id: &str,
    timing: &promo_model::speech::TakeTiming,
) -> Result<TimelineWords, String> {
    let resource = meta
        .resources
        .iter()
        .flatten()
        .find(|r| r.id == id)
        .ok_or_else(|| format!("no resource `{id}`"))?;
    // The mapping needs the media's length; a resource that states none
    // has the take's measured one.
    let mut resource = resource.clone();
    if resource.duration.is_none_or(|d| d <= 0.0) {
        resource.duration = Some(timing.duration);
    }
    let resource = &resource;
    let mut layers: Vec<&promo_model::ProjectLayer> = meta
        .layers
        .iter()
        .flatten()
        .filter(|l| {
            l.is_enabled
                && l.resource_id.as_deref() == Some(id)
                && matches!(
                    l.kind,
                    promo_model::ProjectLayerKind::Audio | promo_model::ProjectLayerKind::Video
                )
        })
        .collect();
    layers.sort_by(|a, b| a.start_time.total_cmp(&b.start_time));
    let layer = layers.first().ok_or_else(|| {
        format!("no enabled layer plays `{id}` — --timeline has nothing to map through")
    })?;
    let end = layer.duration.map(|d| layer.start_time + d);
    // Through the layer's media cut, as the speech clock and validate map
    // it: the resource's own trims alone listed the words the cut plays as
    // trimmed out and the ones it removed on the timeline.
    let material = crate::mapping::resource_for_cut(resource, layer.media_cut_id.as_deref());
    let place = |source: f64| {
        crate::mapping::timeline_output_time_for_source(&material, source)
            .map(|t| layer.start_time + t)
            .filter(|t| end.is_none_or(|e| *t <= e + 1e-9))
            .map(promo_model::speech::round_us)
    };
    Ok((
        layer.id.clone(),
        timing
            .words
            .iter()
            .map(|w| Some((place(w.start)?, place(w.end)?)))
            .collect(),
    ))
}

/// The `timeline` key of `promo_words`' answer: the layer, and each word's
/// output start and end (null where the layer never plays it).
pub fn timeline_json(layer: &str, times: &[Option<(f64, f64)>]) -> serde_json::Value {
    serde_json::json!({
        "layer": layer,
        "words": times
            .iter()
            .map(|t| t.map(|(s, e)| serde_json::json!({"start": s, "end": e})))
            .collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    /// Issue #27: words map through the layer's media cut, as the speech
    /// clock does — a cut from 4 s on a layer at 1 s puts file second 4 at
    /// timeline 1, and what the cut removes is None.
    #[test]
    fn timeline_words_follow_the_layers_media_cut() {
        let meta = promo_model::ProjectMetadata::from_json(
            r#"{"id":"P","name":"p","createdAt":0,"state":"recorded","trimStart":0,"trimEnd":0,
              "videoDuration":0,"subtitles":[],
              "compositionSettings":{"canvasWidth":320,"canvasHeight":180},
              "resources":[{"id":"song","kind":"audio","filename":"s.wav","displayName":"s",
                "addedAt":0,"duration":8,
                "mediaCuts":[{"id":"late","trimStart":4,"trimEnd":8}]}],
              "layers":[{"id":"songL","name":"songL","sortIndex":0,"kind":"audio","isEnabled":true,
                "startTime":1,"duration":4,"resourceID":"song","mediaCutID":"late","keyframes":[]}]}"#,
        )
        .unwrap();
        let mut timing = promo_model::estimate::estimate_words(
            "One two three four five six seven eight.",
            8.0,
            None,
            1.0,
        );
        for (i, w) in timing.words.iter_mut().enumerate() {
            (w.start, w.end) = (i as f64, i as f64 + 0.8);
        }
        let (layer, times) = super::timeline_words(&meta, "song", &timing).unwrap();
        assert_eq!(layer, "songL");
        assert!(times[..4].iter().all(Option::is_none), "{times:?}");
        assert_eq!(times[4], Some((1.0, 1.8)), "{times:?}");
        assert_eq!(times[7], Some((4.0, 4.8)), "{times:?}");
    }
}

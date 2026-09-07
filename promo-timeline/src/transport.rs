//! The consumer's transport (rung 47).
//!
//! A clocked resource — a video, a composition, an audio, a sprite sheet —
//! runs from its beginning on the layer's clock by default. The LAYER that
//! plays it is the consumer, and the consumer decides what happens to that
//! clock, by keyframes of its own: `sourceTime` is a seek (the material is
//! at that time of its own from this keyframe on), `playback` is `play` or
//! `pause`, a state held until the next keyframe that says. Both are
//! steps. A swap keyframe's `sourceTime` is where the arriving material
//! starts — absent, it is wherever the clock already is, which is what
//! makes a film that has played on a screen since frame one continue
//! seamlessly when it becomes the main.
//!
//! Two readings of one walk: [`material_time`] for a picture (where is the
//! material at this instant) and [`playing_spans`] for a soundtrack (when
//! does the material advance, and from where).
use promo_model::{Playback, ProjectLayer, ProjectLayerKeyframe};

/// A span the layer plays through: from `local_start` until `local_end`
/// (`None`: the layer's end) the material advances at its rate, and it
/// stands at `material_start` — its own seconds — when the span opens.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayingSpan {
    pub local_start: f64,
    pub local_end: Option<f64>,
    pub material_start: f64,
}

/// Whether any keyframe touches the clock.
pub fn has_transport(layer: &ProjectLayer) -> bool {
    layer
        .keyframes
        .iter()
        .any(|k| k.source_time.is_some() || k.playback.is_some())
}

/// The keyframes that touch the clock, in time order — array order on a
/// tie, the rule every track here plays.
fn events(layer: &ProjectLayer) -> Vec<&ProjectLayerKeyframe> {
    let mut events: Vec<&ProjectLayerKeyframe> = layer
        .keyframes
        .iter()
        .filter(|k| k.source_time.is_some() || k.playback.is_some())
        .collect();
    events.sort_by(|a, b| {
        a.time
            .partial_cmp(&b.time)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    events
}

/// Where the material is, in its own seconds, at layer-local `local`.
pub fn material_time(layer: &ProjectLayer, local: f64) -> f64 {
    let mut material = 0.0f64;
    let mut playing = true;
    let mut anchor = 0.0f64;
    for k in events(layer) {
        if k.time > local {
            break;
        }
        if playing {
            material += k.time - anchor;
        }
        anchor = k.time;
        if let Some(seek) = k.source_time {
            material = seek.max(0.0);
        }
        if let Some(p) = k.playback {
            playing = p == Playback::Play;
        }
    }
    if playing {
        material += local - anchor;
    }
    material.max(0.0)
}

/// The spans the layer plays through: a pause is the gap between two
/// spans, a seek closes one span and opens the next at its target. A layer
/// with no transport keyframes is one span from its start, at material 0.
pub fn playing_spans(layer: &ProjectLayer) -> Vec<PlayingSpan> {
    let mut spans = Vec::new();
    let mut material = 0.0f64;
    let mut playing = true;
    let mut anchor = 0.0f64;
    // The span in progress: where it opened, and the material then.
    let mut open: Option<(f64, f64)> = Some((0.0, 0.0));
    for k in events(layer) {
        if playing {
            material += k.time - anchor;
        }
        anchor = k.time;
        let seek = k.source_time.map(|s| s.max(0.0));
        let next_playing = k.playback.map(|p| p == Playback::Play).unwrap_or(playing);
        if let Some((local_start, material_start)) = open {
            if seek.is_some() || !next_playing {
                if k.time > local_start {
                    spans.push(PlayingSpan {
                        local_start,
                        local_end: Some(k.time),
                        material_start,
                    });
                }
                open = None;
            }
        }
        if let Some(seek) = seek {
            material = seek;
        }
        playing = next_playing;
        if playing && open.is_none() {
            open = Some((k.time, material));
        }
    }
    if let Some((local_start, material_start)) = open {
        spans.push(PlayingSpan {
            local_start,
            local_end: None,
            material_start,
        });
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(keyframes: &str) -> ProjectLayer {
        serde_json::from_str(&format!(
            r#"{{"id": "L", "name": "deck", "sortIndex": 0, "kind": "video",
                "isEnabled": true, "startTime": 2.0, "duration": 20,
                "resourceID": "V", "keyframes": [{keyframes}]}}"#
        ))
        .expect("layer")
    }

    fn span(a: f64, b: Option<f64>, m: f64) -> PlayingSpan {
        PlayingSpan {
            local_start: a,
            local_end: b,
            material_start: m,
        }
    }

    /// No transport: the material is the layer's clock, one span from 0.
    #[test]
    fn without_transport_the_material_is_the_layers_clock() {
        let l = layer(r#"{"id": "K", "time": 0, "transitionDuration": 0, "opacity": 1}"#);
        assert!(!has_transport(&l));
        assert_eq!(material_time(&l, 3.5), 3.5);
        assert_eq!(playing_spans(&l), vec![span(0.0, None, 0.0)]);
    }

    /// Paused at 2, resumed at 5: the clock stands still between, and
    /// the soundtrack has a gap there.
    #[test]
    fn a_pause_holds_the_clock_until_play() {
        let l = layer(
            r#"{"id": "A", "time": 2, "transitionDuration": 0, "playback": "pause"},
               {"id": "B", "time": 5, "transitionDuration": 0, "playback": "play"}"#,
        );
        assert!(has_transport(&l));
        assert_eq!(material_time(&l, 1.0), 1.0);
        assert_eq!(material_time(&l, 3.0), 2.0);
        assert_eq!(material_time(&l, 5.0), 2.0);
        assert_eq!(material_time(&l, 6.0), 3.0);
        assert_eq!(
            playing_spans(&l),
            vec![span(0.0, Some(2.0), 0.0), span(5.0, None, 2.0)]
        );
    }

    /// A seek is a step to the material's own time; a takeover with
    /// `sourceTime: 0` starts the arriving material fresh.
    #[test]
    fn a_seek_steps_the_material_to_its_own_time() {
        let l = layer(
            r#"{"id": "A", "time": 4, "transitionDuration": 0, "sourceTime": 10},
               {"id": "B", "time": 8, "transitionDuration": 0, "resourceID": "C", "sourceTime": 0}"#,
        );
        assert_eq!(material_time(&l, 3.0), 3.0);
        assert_eq!(material_time(&l, 4.5), 10.5);
        assert_eq!(material_time(&l, 8.25), 0.25);
        assert_eq!(
            playing_spans(&l),
            vec![
                span(0.0, Some(4.0), 0.0),
                span(4.0, Some(8.0), 10.0),
                span(8.0, None, 0.0)
            ]
        );
    }

    /// "Static until it takes control": paused from the first frame,
    /// played from the takeover — the material starts at 0 then.
    #[test]
    fn static_until_it_takes_control() {
        let l = layer(
            r#"{"id": "A", "time": 0, "transitionDuration": 0, "playback": "pause"},
               {"id": "B", "time": 3, "transitionDuration": 0, "playback": "play"}"#,
        );
        assert_eq!(material_time(&l, 2.0), 0.0);
        assert_eq!(material_time(&l, 4.0), 1.0);
        assert_eq!(playing_spans(&l), vec![span(3.0, None, 0.0)]);
    }

    /// A second `play` while playing, or `pause` while paused, changes
    /// nothing; keyframes on one instant resolve in array order.
    #[test]
    fn repeated_states_are_no_ops() {
        let l = layer(
            r#"{"id": "A", "time": 1, "transitionDuration": 0, "playback": "play"},
               {"id": "B", "time": 2, "transitionDuration": 0, "playback": "pause"},
               {"id": "C", "time": 3, "transitionDuration": 0, "playback": "pause"},
               {"id": "D", "time": 4, "transitionDuration": 0, "playback": "play"}"#,
        );
        assert_eq!(material_time(&l, 5.0), 3.0);
        assert_eq!(
            playing_spans(&l),
            vec![span(0.0, Some(2.0), 0.0), span(4.0, None, 2.0)]
        );
    }
}

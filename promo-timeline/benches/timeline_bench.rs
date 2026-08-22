//! P1 perf baselines: keyframe interpolation (10/100/1k keys), trim/pause/
//! loop mapping across a synthetic 3-hour timeline, and layout math.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use promo_model::{
    CompositionSettings, ProjectLayer, ProjectLayerKind, ProjectResource, ProjectResourceKind,
    Size, VideoTrimKeyframe,
};

// Built from JSON like the tests do, so the bench stops rotting every time
// the model grows an optional field.
fn layer_with_keyframes(count: usize, span: f64) -> ProjectLayer {
    let keyframes: Vec<serde_json::Value> = (0..count)
        .map(|i| {
            serde_json::json!({
                "id": format!("kf-{i}"),
                "time": span * i as f64 / count.max(1) as f64,
                "zoom": 1.0 + (i % 7) as f64 * 0.1,
                "verticalShift": (i % 11) as f64 * 10.0,
                "horizontalShift": (i % 5) as f64 * -8.0,
                "gain": 0.2 + (i % 4) as f64 * 0.2,
                "rotation": (i % 13) as f64 * 3.0,
                "transitionDuration": 0.5,
            })
        })
        .collect();
    let raw = serde_json::json!({
        "id": "bench-layer",
        "name": "Bench",
        "sortIndex": 0,
        "kind": "video",
        "isEnabled": true,
        "startTime": 0.0,
        "duration": span,
        "keyframes": keyframes,
    });
    let _ = ProjectLayerKind::Video;
    serde_json::from_value(raw).expect("bench layer")
}

/// A looped 3-hour video resource with 200 alternating include/exclude trim
/// cuts and a held-frame pause every 4th keyframe — the worst realistic
/// mapping workload.
fn three_hour_resource() -> ProjectResource {
    let duration = 3.0 * 3600.0;
    let cuts = 200usize;
    let trim_keyframes: Vec<VideoTrimKeyframe> = (0..cuts)
        .map(|i| VideoTrimKeyframe {
            id: format!("trim-{i}"),
            time: duration * i as f64 / cuts as f64,
            is_included: i % 2 == 0,
            extended_pause_duration: (i % 8 == 0).then_some(1.5),
        })
        .collect();
    let raw = format!(
        r#"{{"id":"bench","kind":"video","filename":"bench.mp4","displayName":"Bench",
            "addedAt":0,"duration":{duration},"imageCuts":[],
            "disabledAudioTrackIndices":[],"looped":true,
            "trimKeyframes":{}}}"#,
        serde_json::to_string(&trim_keyframes).unwrap()
    );
    let _ = ProjectResourceKind::Video;
    serde_json::from_str(&raw).expect("bench resource")
}

fn interpolation(c: &mut Criterion) {
    let settings = CompositionSettings::default();
    let mut group = c.benchmark_group("keyframe_interpolation");
    for count in [10usize, 100, 1000] {
        let layer = layer_with_keyframes(count, 600.0);
        group.bench_function(format!("transform_{count}_keys"), |b| {
            let mut t = 0.0;
            b.iter(|| {
                t = (t + 0.37) % 600.0;
                black_box(promo_timeline::layer_transform(&layer, t, &settings))
            });
        });
        group.bench_function(format!("gain_{count}_keys"), |b| {
            let mut t = 0.0;
            b.iter(|| {
                t = (t + 0.37) % 600.0;
                black_box(promo_timeline::layer_gain(&layer, t, 0.8))
            });
        });
    }
    group.finish();
}

fn mapping(c: &mut Criterion) {
    let res = three_hour_resource();
    let period = promo_timeline::loop_period(&res);
    let mut group = c.benchmark_group("mapping_3h_200cuts");
    group.bench_function("source_time", |b| {
        let mut t = 0.0;
        b.iter(|| {
            t = (t + 13.7) % (period * 2.5);
            black_box(promo_timeline::source_time_for_local(&res, t))
        });
    });
    group.bench_function("video_segment", |b| {
        let mut t = 0.0;
        b.iter(|| {
            t = (t + 13.7) % (period * 2.5);
            black_box(promo_timeline::video_segment(&res, t))
        });
    });
    group.bench_function("trim_ranges_rebuild", |b| {
        b.iter(|| black_box(promo_timeline::video_trim_ranges(&res)));
    });
    group.finish();
}

fn layout(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout");
    group.bench_function("media_rect", |b| {
        let mut zoom = 0.3;
        b.iter(|| {
            zoom = if zoom > 3.0 { 0.3 } else { zoom + 0.013 };
            black_box(promo_timeline::media_rect(
                Size::new(3840.0, 2160.0),
                Size::new(1920.0, 1080.0),
                zoom,
                42.0,
                -17.0,
            ))
        });
    });
    group.bench_function("letterbox_transform", |b| {
        b.iter(|| {
            black_box(promo_timeline::letterbox_transform(
                Size::new(1080.0, 1920.0),
                Size::new(3840.0, 2160.0),
            ))
        });
    });
    group.finish();
}

criterion_group!(benches, interpolation, mapping, layout);
criterion_main!(benches);

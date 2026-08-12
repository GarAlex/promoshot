//! P3 perf baselines: preview render-at-time — the warm path (all frames
//! cached: pure GPU composition, the scrub-latency proxy) and the cold path
//! (cache miss: provider + texture adoption included).

use criterion::{criterion_group, criterion_main, Criterion};

fn preview(c: &mut Criterion) {
    #[cfg(target_os = "macos")]
    {
        use promo_engine::PreviewEngine;
        use promo_gpu::iosurface::{IOSurfaceRef, OwnedIoSurface};
        use promo_model::ProjectMetadata;
        use std::ffi::{c_char, c_void};
        use std::sync::Mutex;

        struct State {
            keep_alive: Vec<OwnedIoSurface>,
        }

        extern "C" fn provider(
            user: *mut c_void,
            _layer_id: *const c_char,
            _source_time: f64,
            _tier: i32,
            out_surface: *mut IOSurfaceRef,
            out_flags: *mut i32,
        ) -> i32 {
            let state = unsafe { &*(user as *const Mutex<State>) };
            let mut state = state.lock().unwrap();
            // 1080p frame, like a real decoded proxy/full frame.
            let s = OwnedIoSurface::new_bgra(1920, 1080).expect("surface");
            unsafe {
                *out_surface = s.raw();
                *out_flags = 0;
            }
            state.keep_alive.push(s);
            0
        }

        let json = r#"{
            "id": "AAAAAAAA-0000-0000-0000-000000000001",
            "name": "bench", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
            "compositionSettings": {"canvasWidth": 1920, "canvasHeight": 1080},
            "layers": [
                {"id": "BG", "name": "b", "sortIndex": 0, "kind": "background",
                 "isEnabled": true, "startTime": 0, "keyframes": []},
                {"id": "V1", "name": "v", "sortIndex": 1, "kind": "video",
                 "isEnabled": true, "startTime": 0,
                 "resourceID": "AAAAAAAA-0000-0000-0000-00000000BB01",
                 "keyframes": [{"id": "K", "time": 0, "zoom": 1,
                   "verticalShift": 0, "horizontalShift": 0,
                   "transitionDuration": 0}]},
                {"id": "V2", "name": "v2", "sortIndex": 2, "kind": "video",
                 "isEnabled": true, "startTime": 0,
                 "resourceID": "AAAAAAAA-0000-0000-0000-00000000BB01",
                 "keyframes": [{"id": "K2", "time": 0, "zoom": 0.4,
                   "verticalShift": 500, "horizontalShift": 1100,
                   "transitionDuration": 0}]}
            ],
            "resources": [
                {"id": "AAAAAAAA-0000-0000-0000-00000000BB01", "kind": "video",
                 "filename": "c.mp4", "displayName": "c", "addedAt": 0,
                 "duration": 3600, "imageCuts": [],
                 "disabledAudioTrackIndices": []}
            ]}"#;
        let meta = ProjectMetadata::from_json(json).expect("meta");

        let state = Box::new(Mutex::new(State {
            keep_alive: Vec::new(),
        }));
        let user = &*state as *const Mutex<State> as *mut c_void;
        let mut engine = PreviewEngine::new(meta, provider, user, 512 << 20).expect("engine");
        let out = OwnedIoSurface::new_bgra(1920, 1080).expect("out");

        let mut group = c.benchmark_group("preview");
        group.sample_size(50);
        // Warm: fixed time — both layer frames cached after the first call.
        engine.render(10.0, out.raw(), 1920, 1080).expect("prime");
        group.bench_function("render_warm_1080p_2layers", |b| {
            b.iter(|| engine.render(10.0, out.raw(), 1920, 1080).expect("render"));
        });
        // Cold: new time every iteration — miss + provider + adoption.
        let mut t = 100.0;
        group.bench_function("render_cold_1080p_2layers", |b| {
            b.iter(|| {
                t += 1.0;
                engine.render(t, out.raw(), 1920, 1080).expect("render")
            });
        });
        group.finish();
    }
}

criterion_group!(benches, preview);
criterion_main!(benches);

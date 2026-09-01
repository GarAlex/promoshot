//! What every decoder backend must agree on.
//!
//! Backends differ in everything except behaviour: ffmpeg here, VideoToolbox
//! on Apple, Media Foundation on Windows. This is the shared exam, so a new
//! backend is judged against the same invariants rather than against whatever
//! its author happened to test.
//!
//! Run it over any [`DecoderBackend`]:
//!
//! ```no_run
//! # use promo_media::{conformance, ffmpeg::FfmpegDecoderBackend};
//! # let clip = std::path::Path::new("clip.mp4");
//! conformance::run(&FfmpegDecoderBackend, clip, &conformance::Expected {
//!     display_width: 1920, display_height: 1080, rotation_degrees: 0,
//!     duration_s: 2.0,
//! }).expect("conformant");
//! ```

use crate::{DecoderBackend, GpuSurface};
use std::path::Path;

/// What the caller knows about the asset, independent of any backend.
#[derive(Debug, Clone)]
pub struct Expected {
    /// Dimensions **as displayed** — already rotated, if the file is.
    pub display_width: u32,
    pub display_height: u32,
    pub rotation_degrees: i32,
    pub duration_s: f64,
}

#[derive(Debug)]
pub struct Failure(pub String);

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

fn fail(what: impl Into<String>) -> Failure {
    Failure(what.into())
}

fn dimensions(surface: &GpuSurface) -> Option<(u32, u32, usize)> {
    match surface {
        GpuSurface::CpuPixels {
            data,
            width,
            height,
            ..
        } => Some((*width, *height, data.len())),
        _ => None,
    }
}

/// Runs every invariant. Returns the first failure, phrased as something a
/// backend author can act on.
pub fn run(backend: &dyn DecoderBackend, path: &Path, expected: &Expected) -> Result<(), Failure> {
    let mut decoder = backend
        .open(path)
        .map_err(|e| fail(format!("open failed: {e}")))?;

    // 1. Reported size is DISPLAY size. A portrait capture is stored
    //    landscape with a rotation tag; a backend that reports the stored
    //    size hands every caller a frame of the wrong shape — and because the
    //    pixel COUNT is unchanged, nothing errors, the picture is just
    //    scrambled.
    let info = decoder.info().clone();
    if (info.width, info.height) != (expected.display_width, expected.display_height) {
        return Err(fail(format!(
            "info reports {}x{}, display size is {}x{} (rotation {}° must be applied)",
            info.width,
            info.height,
            expected.display_width,
            expected.display_height,
            expected.rotation_degrees
        )));
    }
    if info.rotation_degrees != expected.rotation_degrees {
        return Err(fail(format!(
            "rotation reported as {}°, expected {}°",
            info.rotation_degrees, expected.rotation_degrees
        )));
    }
    if expected.duration_s > 0.0 && (info.duration_s - expected.duration_s).abs() > 0.5 {
        return Err(fail(format!(
            "duration {}, expected about {}",
            info.duration_s, expected.duration_s
        )));
    }

    // 2. A decoded frame matches the reported size, exactly.
    let first = decoder
        .frame_at(0.0)
        .map_err(|e| fail(format!("frame_at(0) failed: {e}")))?
        .ok_or_else(|| fail("frame_at(0) returned nothing"))?;
    let (w, h, bytes) = dimensions(&first).ok_or_else(|| fail("frame is not CPU pixels"))?;
    if (w, h) != (info.width, info.height) {
        return Err(fail(format!(
            "frame is {w}x{h} but info says {}x{}",
            info.width, info.height
        )));
    }
    if bytes != (w * h * 4) as usize {
        return Err(fail(format!(
            "frame carries {bytes} bytes, expected {} for {w}x{h} BGRA",
            w * h * 4
        )));
    }

    // 3. Walking forward works, and the picture actually changes.
    let step = 1.0 / 10.0;
    let mut previous = dimensions(&first).map(|_| first);
    let mut differing = 0;
    let mut samples = 0;
    let mut t = step;
    while t < expected.duration_s.max(step) {
        let frame = decoder
            .frame_at(t)
            .map_err(|e| fail(format!("frame_at({t}) failed: {e}")))?;
        let Some(frame) = frame else { break };
        samples += 1;
        if let (Some(a), Some(b)) = (previous.as_ref(), Some(&frame)) {
            if let (
                GpuSurface::CpuPixels { data: da, .. },
                GpuSurface::CpuPixels { data: db, .. },
            ) = (a, b)
            {
                if da != db {
                    differing += 1;
                }
            }
        }
        previous = Some(frame);
        t += step;
    }
    if samples > 2 && differing == 0 {
        return Err(fail(
            "every sampled frame was identical — the decoder is not advancing",
        ));
    }

    // 4. The same timestamp decodes to the same frame, even after a rewind.
    //    Whatever a backend does internally, a seek must be repeatable.
    let again = decoder
        .frame_at(0.0)
        .map_err(|e| fail(format!("rewind failed: {e}")))?
        .ok_or_else(|| fail("rewind returned nothing"))?;
    let (a, b) = (decoder.frame_at(0.0).ok().flatten(), Some(again));
    if let (
        Some(GpuSurface::CpuPixels { data: da, .. }),
        Some(GpuSurface::CpuPixels { data: db, .. }),
    ) = (&a, &b)
    {
        if da != db {
            return Err(fail("the same timestamp decoded differently twice"));
        }
    }

    // 5. Past the end is None, not an error and not a frame.
    if expected.duration_s > 0.0 {
        match decoder.frame_at(expected.duration_s + 30.0) {
            Ok(None) => {}
            Ok(Some(_)) => return Err(fail("returned a frame past the end of the asset")),
            Err(e) => return Err(fail(format!("past the end should be None, got: {e}"))),
        }
    }

    Ok(())
}

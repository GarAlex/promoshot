//! The ffmpeg backend, driven as a **separate process**.
//!
//! Frames are read from `ffmpeg` as raw BGRA on a pipe rather than by linking
//! libav. That trade is deliberate for the first backend:
//!
//! - no build dependency on ffmpeg development libraries, which is the part
//!   that makes cross-platform Rust media builds miserable;
//! - invoking a program is not linking it, so nothing here inherits ffmpeg's
//!   licence;
//! - it is small enough to get the trait shape and the conformance fixtures
//!   right, which is what a linked backend will need anyway.
//!
//! The cost is a process and a pipe per open asset, and no hardware decode
//! path. When that starts to matter, a linked backend implements the same
//! traits and this one stays as the portable fallback.

use crate::{
    BackendCapabilities, DecoderBackend, EncodeSpec, EncoderBackend, MediaError, VideoDecoder,
    VideoEncoder, VideoInfo,
};
use promo_gpu::GpuSurface;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// A backwards jump, or a forward jump larger than this, restarts ffmpeg with
/// a seek. Smaller forward jumps just read and discard, which is cheaper than
/// paying process startup plus a keyframe search.
const SEEK_AHEAD_LIMIT_S: f64 = 1.0;

pub struct FfmpegDecoderBackend;

impl DecoderBackend for FfmpegDecoderBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            name: "ffmpeg",
            hardware_accelerated: false,
            max_width: 8192,
            max_height: 8192,
        }
    }

    fn probe(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn open(&self, path: &Path) -> Result<Box<dyn VideoDecoder>, MediaError> {
        Ok(Box::new(FfmpegDecoder::open(path)?))
    }
}

pub struct FfmpegDecoder {
    path: PathBuf,
    info: VideoInfo,
    /// Frame bytes, once, so a 4K decode does not allocate per frame.
    buffer: Vec<u8>,
    reader: Option<FrameReader>,
    /// Presentation time of the frame currently in `buffer`, if any.
    current_pts: Option<f64>,
}

struct FrameReader {
    child: Child,
    /// Where this reader started, so pts can be derived from frame count.
    start_s: f64,
    frames_read: u64,
}

impl FfmpegDecoder {
    pub fn open(path: &Path) -> Result<Self, MediaError> {
        let info = probe(path)?;
        if info.width == 0 || info.height == 0 {
            return Err(MediaError::Unsupported(format!(
                "{}: no video stream",
                path.display()
            )));
        }
        Ok(Self {
            path: path.to_path_buf(),
            buffer: vec![0u8; (info.width * info.height * 4) as usize],
            info,
            reader: None,
            current_pts: None,
        })
    }

    fn frame_bytes(&self) -> usize {
        (self.info.width * self.info.height * 4) as usize
    }

    /// Starts ffmpeg decoding from `start_s`.
    ///
    /// `-ss` before `-i` seeks by keyframe and is fast; the frames that come
    /// out start at or just before the request, which is what `frame_at`
    /// then walks forward from.
    fn start_reader(&mut self, start_s: f64) -> Result<(), MediaError> {
        self.stop_reader();
        let mut command = Command::new("ffmpeg");
        command.args(["-v", "error", "-nostdin"]);
        if start_s > 0.0 {
            command.args(["-ss", &format!("{start_s}")]);
        }
        command
            .arg("-i")
            .arg(&self.path)
            .args([
                "-f",
                "rawvideo",
                "-pix_fmt",
                "bgra",
                // Honour the display rotation rather than emitting the stored
                // orientation — the conformance rule every backend must meet.
                "-vf",
                "scale=iw:ih",
                "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null());

        let child = command.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => MediaError::ToolMissing(
                "ffmpeg",
                "not found on PATH — install it, or render stills instead".into(),
            ),
            _ => MediaError::Backend(format!("ffmpeg: {e}")),
        })?;

        self.reader = Some(FrameReader {
            child,
            start_s,
            frames_read: 0,
        });
        self.current_pts = None;
        Ok(())
    }

    fn stop_reader(&mut self) {
        if let Some(mut reader) = self.reader.take() {
            let _ = reader.child.kill();
            let _ = reader.child.wait();
        }
    }

    /// Reads one frame into `buffer`. `false` at end of stream.
    fn read_frame(&mut self) -> Result<bool, MediaError> {
        let need = self.frame_bytes();
        let Some(reader) = self.reader.as_mut() else {
            return Ok(false);
        };
        let Some(stdout) = reader.child.stdout.as_mut() else {
            return Ok(false);
        };
        // A short read at the end of the stream is normal; a short read in
        // the middle is a truncated frame and must not be shown.
        let mut filled = 0usize;
        while filled < need {
            match stdout.read(&mut self.buffer[filled..need]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) => return Err(MediaError::Backend(format!("ffmpeg read: {e}"))),
            }
        }
        if filled == 0 {
            return Ok(false);
        }
        if filled < need {
            return Err(MediaError::Backend(
                "ffmpeg produced a truncated frame".into(),
            ));
        }
        let fps = if self.info.nominal_fps > 0.0 {
            self.info.nominal_fps
        } else {
            30.0
        };
        let reader = self.reader.as_mut().expect("reader");
        self.current_pts = Some(reader.start_s + reader.frames_read as f64 / fps);
        reader.frames_read += 1;
        Ok(true)
    }

    fn current_surface(&self) -> GpuSurface {
        GpuSurface::CpuPixels {
            data: self.buffer.clone(),
            width: self.info.width,
            height: self.info.height,
            bytes_per_row: self.info.width * 4,
        }
    }
}

impl Drop for FfmpegDecoder {
    fn drop(&mut self) {
        self.stop_reader();
    }
}

impl VideoDecoder for FfmpegDecoder {
    fn info(&self) -> &VideoInfo {
        &self.info
    }

    fn frame_at(&mut self, pts_s: f64) -> Result<Option<GpuSurface>, MediaError> {
        let want = pts_s.max(0.0);
        if self.info.duration_s > 0.0 && want > self.info.duration_s {
            return Ok(None);
        }

        // Decide whether the open reader can serve this, or we start over.
        let restart = match (self.reader.is_some(), self.current_pts) {
            (false, _) => true,
            // Backwards: the pipe only goes one way.
            (true, Some(current)) if want + 1e-6 < current => true,
            // Far ahead: cheaper to seek than to decode everything between.
            (true, Some(current)) if want - current > SEEK_AHEAD_LIMIT_S => true,
            _ => false,
        };
        if restart {
            self.start_reader(want)?;
            if !self.read_frame()? {
                return Ok(None);
            }
        }

        // Walk forward to the frame that covers `want`.
        let fps = if self.info.nominal_fps > 0.0 {
            self.info.nominal_fps
        } else {
            30.0
        };
        let step = 1.0 / fps;
        loop {
            match self.current_pts {
                None => {
                    if !self.read_frame()? {
                        return Ok(None);
                    }
                }
                Some(current) if current + step <= want + 1e-9 => {
                    if !self.read_frame()? {
                        // Past the last frame: hold the final one rather than
                        // blinking to nothing at the tail of a clip.
                        break;
                    }
                }
                _ => break,
            }
        }
        Ok(Some(self.current_surface()))
    }
}

/// Reads stream properties with ffprobe.
fn probe(path: &Path) -> Result<VideoInfo, MediaError> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,r_frame_rate,duration:format=duration",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(path)
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => MediaError::ToolMissing(
                "ffprobe",
                "not found on PATH (it ships with ffmpeg)".into(),
            ),
            _ => MediaError::Backend(format!("ffprobe: {e}")),
        })?;
    if !output.status.success() {
        return Err(MediaError::Unsupported(format!(
            "{}: ffprobe could not read it",
            path.display()
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut info = VideoInfo::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "width" => info.width = value.trim().parse().unwrap_or(0),
            "height" => info.height = value.trim().parse().unwrap_or(0),
            "r_frame_rate" => info.nominal_fps = parse_rational(value.trim()),
            // The stream's duration is missing in some containers; ffprobe
            // then prints the format's, and the first one we see wins.
            "duration" if info.duration_s <= 0.0 => {
                info.duration_s = value.trim().parse().unwrap_or(0.0);
            }
            _ => {}
        }
    }
    info.rotation_degrees = probe_rotation(path);
    Ok(info)
}

/// Display rotation, if the container carries one.
fn probe_rotation(path: &Path) -> i32 {
    let Ok(output) = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream_side_data=rotation",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
    else {
        return 0;
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|l| l.trim().parse::<f64>().ok())
        .map(|r| r.round() as i32)
        .unwrap_or(0)
}

/// `"30000/1001"` → 29.97.
fn parse_rational(raw: &str) -> f64 {
    match raw.split_once('/') {
        Some((n, d)) => {
            let (n, d) = (
                n.parse::<f64>().unwrap_or(0.0),
                d.parse::<f64>().unwrap_or(1.0),
            );
            if d == 0.0 {
                0.0
            } else {
                n / d
            }
        }
        None => raw.parse().unwrap_or(0.0),
    }
}

pub struct FfmpegEncoderBackend;

impl EncoderBackend for FfmpegEncoderBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            name: "ffmpeg",
            hardware_accelerated: false,
            max_width: 8192,
            max_height: 8192,
        }
    }

    fn open(&self, path: &Path, spec: &EncodeSpec) -> Result<Box<dyn VideoEncoder>, MediaError> {
        Ok(Box::new(FfmpegEncoder::open(path, spec)?))
    }
}

pub struct FfmpegEncoder {
    child: Child,
}

impl FfmpegEncoder {
    pub fn open(path: &Path, spec: &EncodeSpec) -> Result<Self, MediaError> {
        let child = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "bgra",
                "-s",
                &format!("{}x{}", spec.width, spec.height),
                "-r",
                &format!("{}", spec.fps),
                "-i",
                "-",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-crf",
                &format!("{}", spec.quality),
            ])
            .arg(path)
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => MediaError::ToolMissing(
                    "ffmpeg",
                    "not found on PATH — install it, or render a PNG sequence instead".into(),
                ),
                _ => MediaError::Backend(format!("ffmpeg: {e}")),
            })?;
        Ok(Self { child })
    }
}

impl VideoEncoder for FfmpegEncoder {
    fn write_frame(&mut self, bgra: &[u8]) -> Result<(), MediaError> {
        let stdin = self
            .child
            .stdin
            .as_mut()
            .ok_or_else(|| MediaError::Backend("ffmpeg: stdin closed".into()))?;
        stdin
            .write_all(bgra)
            .map_err(|e| MediaError::Backend(format!("ffmpeg stdin: {e}")))
    }

    fn finish(mut self: Box<Self>) -> Result<(), MediaError> {
        // Closing stdin is what tells ffmpeg to flush and write the trailer.
        drop(self.child.stdin.take());
        let status = self
            .child
            .wait()
            .map_err(|e| MediaError::Backend(format!("ffmpeg: {e}")))?;
        if !status.success() {
            return Err(MediaError::Backend(format!("ffmpeg exited with {status}")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a clip whose colour changes over time, so a decoded frame can
    /// be checked against the moment it claims to be.
    fn fixture(seconds: u32) -> Option<PathBuf> {
        let dir = std::env::temp_dir().join("promo-media-tests");
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join(format!("clip-{seconds}s.mp4"));
        if path.is_file() {
            return Some(path);
        }
        let status = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                &format!("testsrc=size=320x240:rate=30:duration={seconds}"),
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&path)
            .status()
            .ok()?;
        status.success().then_some(path)
    }

    fn pixels(surface: &GpuSurface) -> (Vec<u8>, u32, u32) {
        match surface {
            GpuSurface::CpuPixels {
                data,
                width,
                height,
                ..
            } => (data.clone(), *width, *height),
            other => panic!("expected CPU pixels, got {other:?}"),
        }
    }

    #[test]
    fn reads_a_clips_dimensions_and_rate() {
        let Some(path) = fixture(2) else {
            eprintln!("ffmpeg unavailable; skipping");
            return;
        };
        let decoder = FfmpegDecoder::open(&path).expect("open");
        let info = decoder.info();
        assert_eq!((info.width, info.height), (320, 240));
        assert!(
            (info.nominal_fps - 30.0).abs() < 0.01,
            "{}",
            info.nominal_fps
        );
        assert!(info.duration_s > 1.5, "{}", info.duration_s);
        assert_eq!(info.rotation_degrees, 0);
    }

    #[test]
    fn decodes_frames_at_requested_times() {
        let Some(path) = fixture(2) else {
            eprintln!("ffmpeg unavailable; skipping");
            return;
        };
        let mut decoder = FfmpegDecoder::open(&path).expect("open");
        let frame = decoder.frame_at(0.0).expect("decode").expect("a frame");
        let (data, w, h) = pixels(&frame);
        assert_eq!((w, h), (320, 240));
        assert_eq!(data.len(), (w * h * 4) as usize);
        assert!(data.iter().any(|&b| b != 0), "the frame is not blank");
    }

    /// The rendering case: many forward requests down one pipe.
    #[test]
    fn walking_forward_stays_on_one_reader() {
        let Some(path) = fixture(2) else {
            eprintln!("ffmpeg unavailable; skipping");
            return;
        };
        let mut decoder = FfmpegDecoder::open(&path).expect("open");
        let mut previous: Option<Vec<u8>> = None;
        let mut changed = 0;
        for i in 0..30 {
            let t = i as f64 / 30.0;
            let frame = decoder.frame_at(t).expect("decode").expect("a frame");
            let (data, _, _) = pixels(&frame);
            if let Some(prev) = &previous {
                if *prev != data {
                    changed += 1;
                }
            }
            previous = Some(data);
        }
        assert!(
            changed > 20,
            "a moving test pattern must produce different frames ({changed}/29 differed)"
        );
    }

    /// Seeking backwards is the case a pipe cannot serve, so it must restart
    /// and still be correct.
    #[test]
    fn seeking_backwards_restarts_and_matches() {
        let Some(path) = fixture(2) else {
            eprintln!("ffmpeg unavailable; skipping");
            return;
        };
        let mut decoder = FfmpegDecoder::open(&path).expect("open");
        let first = pixels(&decoder.frame_at(0.0).unwrap().unwrap()).0;
        let _ = decoder.frame_at(1.5).unwrap().unwrap();
        let again = pixels(&decoder.frame_at(0.0).unwrap().unwrap()).0;
        assert_eq!(
            first, again,
            "the same timestamp must decode to the same frame after a rewind"
        );
    }

    #[test]
    fn past_the_end_reports_nothing() {
        let Some(path) = fixture(2) else {
            eprintln!("ffmpeg unavailable; skipping");
            return;
        };
        let mut decoder = FfmpegDecoder::open(&path).expect("open");
        assert!(decoder.frame_at(99.0).expect("decode").is_none());
    }

    #[test]
    fn encodes_what_it_is_given() {
        let Some(_) = fixture(1) else {
            eprintln!("ffmpeg unavailable; skipping");
            return;
        };
        let out = std::env::temp_dir().join("promo-media-tests/encoded.mp4");
        let _ = std::fs::remove_file(&out);
        let spec = EncodeSpec {
            width: 64,
            height: 48,
            fps: 30.0,
            quality: 23,
        };
        let mut encoder: Box<dyn VideoEncoder> =
            Box::new(FfmpegEncoder::open(&out, &spec).expect("open encoder"));
        let frame = vec![200u8; (64 * 48 * 4) as usize];
        for _ in 0..30 {
            encoder.write_frame(&frame).expect("write");
        }
        encoder.finish().expect("finish");

        let decoder = FfmpegDecoder::open(&out).expect("read back");
        assert_eq!((decoder.info().width, decoder.info().height), (64, 48));
    }
}

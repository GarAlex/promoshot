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
    AudioBuffer, AudioReader, BackendCapabilities, DecoderBackend, EncodeSpec, EncoderBackend,
    MediaError, VideoDecoder, VideoEncoder, VideoInfo,
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
    // ffprobe reports the STORED size; ffmpeg's decoder auto-applies the
    // display matrix, so a quarter-turn comes out transposed. Report what
    // will actually arrive — the pixel count is identical either way, so a
    // mismatch here scrambles the picture instead of raising an error.
    if info.rotation_degrees.rem_euclid(180) == 90 {
        std::mem::swap(&mut info.width, &mut info.height);
    }
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

/// Reads audio by asking ffmpeg for raw f32 samples at the rate and channel
/// count the mixer works in, so resampling is ffmpeg's problem rather than
/// ours.
pub struct FfmpegAudioReader;

impl AudioReader for FfmpegAudioReader {
    fn read_at_speed(
        &self,
        path: &Path,
        sample_rate: u32,
        channels: u16,
        speed: f64,
    ) -> Result<Option<AudioBuffer>, MediaError> {
        if !has_audio_stream(path) {
            return Ok(None);
        }
        let mut command = Command::new("ffmpeg");
        command.args(["-v", "error", "-nostdin", "-i"]).arg(path);
        if let Some(filter) = crate::atempo_chain(speed) {
            command.args(["-filter:a", &filter]);
        }
        let output = command
            .args([
                "-vn",
                "-f",
                "f32le",
                "-acodec",
                "pcm_f32le",
                "-ac",
                &channels.to_string(),
                "-ar",
                &sample_rate.to_string(),
                "-",
            ])
            .output()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => {
                    MediaError::ToolMissing("ffmpeg", "not found on PATH".into())
                }
                _ => MediaError::Backend(format!("ffmpeg: {e}")),
            })?;
        if !output.status.success() {
            return Err(MediaError::Backend(format!(
                "{}: ffmpeg could not read its audio",
                path.display()
            )));
        }
        let samples: Vec<f32> = output
            .stdout
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        if samples.is_empty() {
            return Ok(None);
        }
        Ok(Some(AudioBuffer {
            samples,
            channels,
            sample_rate,
        }))
    }

    fn read_tracks(
        &self,
        path: &Path,
        sample_rate: u32,
        channels: u16,
        speed: f64,
        tracks: &crate::TrackSelection,
    ) -> Result<Option<AudioBuffer>, MediaError> {
        let count = audio_stream_count(path);
        if count == 0 {
            return Ok(None);
        }
        let kept = tracks.kept(count);
        if kept.is_empty() {
            return Ok(None);
        }
        // Only a single-track asset can take the plain path: with no -map,
        // ffmpeg picks ONE "best" audio stream, so a multi-track recording
        // read that way is its first track, not the sum the apps play.
        if count == 1 {
            return self.read_at_speed(path, sample_rate, channels, speed);
        }
        let mut command = Command::new("ffmpeg");
        command.args(["-v", "error", "-nostdin", "-i"]).arg(path);
        let tempo = crate::atempo_chain(speed);
        if kept.len() == 1 {
            command.args(["-map", &format!("0:a:{}", kept[0])]);
            if let Some(filter) = tempo {
                command.args(["-filter:a", &filter]);
            }
        } else {
            // Sum the kept tracks at unity (normalize=0), as the apps'
            // composition mixer does when each is its own track.
            let inputs: String = kept.iter().map(|i| format!("[0:a:{i}]")).collect();
            let mut graph = format!("{inputs}amix=inputs={}:normalize=0", kept.len());
            if let Some(filter) = tempo {
                graph.push(',');
                graph.push_str(&filter);
            }
            graph.push_str("[out]");
            command.args(["-filter_complex", &graph, "-map", "[out]"]);
        }
        let output = command
            .args([
                "-vn",
                "-f",
                "f32le",
                "-acodec",
                "pcm_f32le",
                "-ac",
                &channels.to_string(),
                "-ar",
                &sample_rate.to_string(),
                "-",
            ])
            .output()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => {
                    MediaError::ToolMissing("ffmpeg", "not found on PATH".into())
                }
                _ => MediaError::Backend(format!("ffmpeg: {e}")),
            })?;
        if !output.status.success() {
            return Err(MediaError::Backend(format!(
                "{}: ffmpeg could not read its audio tracks",
                path.display()
            )));
        }
        let samples: Vec<f32> = output
            .stdout
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        if samples.is_empty() {
            return Ok(None);
        }
        Ok(Some(AudioBuffer {
            samples,
            channels,
            sample_rate,
        }))
    }
}

/// How many audio streams the asset carries (0 when ffprobe is missing or
/// the file has none).
fn audio_stream_count(path: &Path) -> usize {
    let Ok(output) = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a",
            "-show_entries",
            "stream=index",
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
        .filter(|l| !l.trim().is_empty())
        .count()
}

/// Does this asset have an audio track at all? Most screen recordings do not,
/// and asking ffmpeg to decode a stream that is not there is an error rather
/// than an empty answer.
fn has_audio_stream(path: &Path) -> bool {
    let Ok(output) = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=index",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
    else {
        return false;
    };
    !String::from_utf8_lossy(&output.stdout).trim().is_empty()
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
    /// The mixed soundtrack, written to a temp WAV that ffmpeg reads as a
    /// second input. Two pipes into one process would have to be fed in
    /// lockstep or ffmpeg blocks; a file sidesteps that for what is at most a
    /// few minutes of audio.
    audio_temp: Option<PathBuf>,
}

impl FfmpegEncoder {
    pub fn open(path: &Path, spec: &EncodeSpec) -> Result<Self, MediaError> {
        let audio_temp = match &spec.audio {
            Some(audio) if !audio.samples.is_empty() => Some(write_wav(audio)?),
            _ => None,
        };

        // Chapters ride an ffmetadata file as one more input, mapped in as
        // the container's metadata; the end of each is the next's start.
        let chapters_temp = if spec.chapters.is_empty() {
            None
        } else {
            Some(write_chapters(&spec.chapters)?)
        };
        let mut command = Command::new("ffmpeg");
        command.args([
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
        ]);
        if let Some(wav) = &audio_temp {
            command.arg("-i").arg(wav);
        }
        if let Some(chapters) = &chapters_temp {
            let index = if audio_temp.is_some() { "2" } else { "1" };
            command.arg("-i").arg(chapters);
            command.args(["-map_metadata", index, "-map", "0:v"]);
            if audio_temp.is_some() {
                command.args(["-map", "1:a"]);
            }
        }
        command.args([
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-crf",
            &format!("{}", spec.quality),
        ]);
        if audio_temp.is_some() {
            // AAC for compatibility; -shortest so a soundtrack longer than
            // the render cannot extend the file past its last frame.
            command.args(["-c:a", "aac", "-b:a", "192k", "-shortest"]);
        }
        let child = command
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
        Ok(Self { child, audio_temp })
    }
}

/// Writes interleaved f32 PCM as a WAV (format 3, IEEE float).
/// An ffmetadata file with one [CHAPTER] per entry, in milliseconds; each
/// chapter ends where the next begins, the last a day later (the container
/// clamps it to the stream's end).
fn write_chapters(chapters: &[(f64, String)]) -> Result<PathBuf, MediaError> {
    let dir = std::env::temp_dir().join("promo-media");
    std::fs::create_dir_all(&dir).map_err(|e| MediaError::Backend(format!("temp dir: {e}")))?;
    let path = dir.join(format!(
        "chapters-{}-{}.txt",
        std::process::id(),
        chapters.len()
    ));
    let mut sorted: Vec<&(f64, String)> = chapters.iter().collect();
    sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut text = String::from(";FFMETADATA1\n");
    for (i, (start, title)) in sorted.iter().enumerate() {
        let end = sorted
            .get(i + 1)
            .map(|next| next.0)
            .unwrap_or(start + 86_400.0);
        let title = title
            .replace('\\', "\\\\")
            .replace('=', "\\=")
            .replace(';', "\\;")
            .replace('#', "\\#")
            .replace('\n', " ");
        text.push_str(&format!(
            "[CHAPTER]\nTIMEBASE=1/1000\nSTART={}\nEND={}\ntitle={}\n",
            (start.max(0.0) * 1000.0).round() as i64,
            (end.max(*start) * 1000.0).round() as i64,
            title
        ));
    }
    std::fs::write(&path, text).map_err(|e| MediaError::Backend(format!("chapters: {e}")))?;
    Ok(path)
}

fn write_wav(audio: &AudioBuffer) -> Result<PathBuf, MediaError> {
    let dir = std::env::temp_dir().join("promo-media");
    std::fs::create_dir_all(&dir).map_err(|e| MediaError::Backend(format!("temp dir: {e}")))?;
    let path = dir.join(format!(
        "mix-{}-{}.wav",
        std::process::id(),
        audio.samples.len()
    ));

    let channels = audio.channels.max(1);
    let bytes_per_frame = channels as u32 * 4;
    let data_len = (audio.samples.len() * 4) as u32;
    let mut out = Vec::with_capacity(data_len as usize + 44);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&audio.sample_rate.to_le_bytes());
    out.extend_from_slice(&(audio.sample_rate * bytes_per_frame).to_le_bytes());
    out.extend_from_slice(&(bytes_per_frame as u16).to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in &audio.samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    std::fs::write(&path, out).map_err(|e| MediaError::Backend(format!("temp wav: {e}")))?;
    Ok(path)
}

impl Drop for FfmpegEncoder {
    fn drop(&mut self) {
        // An encoder dropped without finish() is an ABANDONED export, and
        // ffmpeg must die with it: left alone it reads EOF off the closed
        // pipe, flushes the trailer, and completes a half-export that then
        // looks exported — while holding the output file open, so a
        // Windows caller cannot even delete it. finish() has already
        // waited by the time this runs; kill/wait on a reaped child are
        // no-ops worth ignoring, a live one dies and is reaped.
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(wav) = &self.audio_temp {
            let _ = std::fs::remove_file(wav);
        }
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
        if let Some(wav) = &self.audio_temp {
            let _ = std::fs::remove_file(wav);
        }
        if !status.success() {
            return Err(MediaError::Backend(format!("ffmpeg exited with {status}")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dropping an encoder without finish() abandons the export: ffmpeg
    /// must be dead and the file deletable IMMEDIATELY. A leaked child
    /// reads EOF, completes a half-export behind the caller's back, and on
    /// Windows holds the file against the deletion a cancel needs.
    #[test]
    fn a_dropped_encoder_releases_its_output_at_once() {
        let dir = std::env::temp_dir().join("promo-media-tests");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let out = dir.join(format!("abandoned-{}.mp4", std::process::id()));
        let spec = crate::EncodeSpec {
            chapters: Vec::new(),
            width: 64,
            height: 64,
            fps: 30.0,
            quality: 18,
            audio: None,
        };
        let Ok(mut encoder) = crate::Registry::with_defaults().open_encoder(&out, &spec) else {
            eprintln!("ffmpeg unavailable; skipping");
            return;
        };
        // Feed frames until ffmpeg has demonstrably created the output:
        // dropping before that proves nothing — an absent file cannot say
        // whether the child is dead or merely slow.
        let mut frames = 0;
        while !out.exists() && frames < 300 {
            encoder.write_frame(&[0u8; 64 * 64 * 4]).expect("frame in");
            frames += 1;
        }
        assert!(out.exists(), "ffmpeg never created the output");
        drop(encoder);
        std::fs::remove_file(&out).expect("the abandoned output is closed and deletable");
    }

    /// Builds a clip whose colour changes over time, so a decoded frame can
    /// be checked against the moment it claims to be.
    fn fixture(seconds: u32) -> Option<PathBuf> {
        let dir = std::env::temp_dir().join("promo-media-tests");
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join(format!("clip-{seconds}s.mp4"));
        if path.is_file() {
            return Some(path);
        }
        // Written under a private name and renamed into place. Parallel test
        // threads all fill this cache on a machine's first run, and ffmpeg
        // writing the shared name directly means a second caller sees the
        // file mid-write, takes it as cached, and reads an mp4 whose moov
        // atom does not exist yet. The tmp keeps the .mp4 suffix because
        // ffmpeg infers the container from it.
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let tmp = dir.join(format!(
            "clip-{seconds}s-{}-{}.tmp.mp4",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
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
            .arg(&tmp)
            .status()
            .ok()?;
        if !status.success() {
            let _ = std::fs::remove_file(&tmp);
            return None;
        }
        if std::fs::rename(&tmp, &path).is_err() {
            // Windows refuses to rename over an existing file, so losing the
            // publish race lands here — and the winner's clip is complete,
            // because only complete clips are ever renamed in.
            let _ = std::fs::remove_file(&tmp);
            if !path.is_file() {
                return None;
            }
        }
        Some(path)
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

    /// fixture() callers run on parallel test threads, and a machine that
    /// has never run the suite fills the cache for all of them at once. A
    /// caller that sees the file mid-write takes it as cached and reads a
    /// clip whose moov atom does not exist yet — how the suite's first-ever
    /// Windows run failed. So: the moment the cache shows the file, it must
    /// be readable. 1s is this test's own cache key; clearing it cannot
    /// disturb the shared 2s clip.
    #[test]
    fn a_clip_visible_in_the_cache_is_always_complete() {
        let path = std::env::temp_dir().join("promo-media-tests/clip-1s.mp4");
        let _ = std::fs::remove_file(&path);
        let writer = std::thread::spawn(|| fixture(1));
        while !path.is_file() && !writer.is_finished() {
            std::thread::yield_now();
        }
        // Size at first sight. A cache that publishes in place shows a file
        // that is still growing — asserting on the size catches that even
        // when the write finishes faster than a decoder could be pointed at
        // it, which on this hardware it does.
        let seen = path.metadata().map(|m| m.len()).unwrap_or(0);
        let Some(clip) = writer.join().expect("fixture thread panicked") else {
            eprintln!("ffmpeg unavailable; skipping");
            return;
        };
        let full = clip.metadata().expect("clip metadata").len();
        assert_eq!(seen, full, "the clip was visible before it was complete");
        FfmpegDecoder::open(&clip).expect("open");
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

    /// A clip carrying a 90° display matrix — a portrait phone capture, in
    /// miniature. Stored 320x240, displayed 240x320.
    fn rotated_fixture() -> Option<PathBuf> {
        let plain = fixture(2)?;
        let path = plain.with_file_name("clip-rot90.mp4");
        if path.is_file() {
            return Some(path);
        }
        let status = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-display_rotation", "90", "-i"])
            .arg(&plain)
            .args(["-c", "copy"])
            .arg(&path)
            .status()
            .ok()?;
        status.success().then_some(path)
    }

    #[test]
    fn meets_the_conformance_suite() {
        let Some(path) = fixture(2) else {
            eprintln!("ffmpeg unavailable; skipping");
            return;
        };
        crate::conformance::run(
            &FfmpegDecoderBackend,
            &path,
            &crate::conformance::Expected {
                display_width: 320,
                display_height: 240,
                rotation_degrees: 0,
                duration_s: 2.0,
            },
        )
        .expect("conformant");
    }

    /// The invariant most likely to be got wrong, and the most expensive to
    /// discover late: a rotated asset must report and decode at its DISPLAY
    /// size. The pixel count is unchanged by a 90° swap, so getting this
    /// wrong produces a scrambled picture, not an error.
    #[test]
    fn meets_the_conformance_suite_on_a_rotated_clip() {
        let Some(path) = rotated_fixture() else {
            eprintln!("ffmpeg unavailable; skipping");
            return;
        };
        crate::conformance::run(
            &FfmpegDecoderBackend,
            &path,
            &crate::conformance::Expected {
                display_width: 240,
                display_height: 320,
                rotation_degrees: 90,
                duration_s: 2.0,
            },
        )
        .expect("conformant on a rotated asset");
    }

    /// A two-track recording — silence on track 0, a tone on track 1, the
    /// shape of a screen capture with the mic muted. Track selection must
    /// keep exactly the tracks asked for and sum the rest at unity.
    #[test]
    fn track_selection_keeps_what_it_is_asked_to() {
        use crate::TrackSelection;
        let dir = std::env::temp_dir().join("promo-media-tests");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let two = dir.join("two-tracks.mp4");
        let ok = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=64x48:rate=30:duration=1",
                "-f",
                "lavfi",
                "-i",
                "anullsrc=r=48000:cl=stereo:d=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1",
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-map",
                "2:a",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ])
            .arg(&two)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("ffmpeg unavailable; skipping");
            return;
        }
        let rms = |audio: Option<AudioBuffer>| -> f32 {
            let Some(audio) = audio else { return 0.0 };
            let n = audio.samples.len().max(1) as f32;
            (audio.samples.iter().map(|s| s * s).sum::<f32>() / n).sqrt()
        };
        let reader = FfmpegAudioReader;
        let read = |sel: TrackSelection| {
            reader
                .read_tracks(&two, 48_000, 2, 1.0, &sel)
                .expect("read")
        };
        let all = rms(read(TrackSelection::All));
        let first = rms(read(TrackSelection::First));
        let without_tone = rms(read(TrackSelection::Except(vec![1])));
        let without_silence = rms(read(TrackSelection::Except(vec![0])));
        assert!(all > 0.01, "the summed tracks carry the tone: {all}");
        assert!(first < 0.001, "track 0 alone is silence: {first}");
        assert!(
            without_tone < 0.001,
            "dropping the tone leaves silence: {without_tone}"
        );
        assert!(
            (without_silence - all).abs() < all * 0.05,
            "dropping silence changes nothing audible: {without_silence} vs {all}"
        );
        assert!(
            read(TrackSelection::Except(vec![0, 1])).is_none(),
            "every track switched off is None, not an error"
        );
    }

    /// A clip with a tone: audio must come back as PCM at the rate asked
    /// for, and a clip without one must answer None rather than erroring —
    /// most screen recordings have no audio track at all.
    #[test]
    fn reads_audio_when_there_is_some_and_none_when_there_is_not() {
        let Some(silent) = fixture(2) else {
            eprintln!("ffmpeg unavailable; skipping");
            return;
        };
        let reader = FfmpegAudioReader;
        assert!(
            reader.read(&silent, 48_000, 2).expect("read").is_none(),
            "a video with no audio track is None, not an error"
        );

        let dir = std::env::temp_dir().join("promo-media-tests");
        let toned = dir.join("tone.mp4");
        if !toned.is_file() {
            let ok = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc=size=64x48:rate=30:duration=2",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:duration=2",
                    "-pix_fmt",
                    "yuv420p",
                    "-c:a",
                    "aac",
                    "-shortest",
                ])
                .arg(&toned)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                eprintln!("could not build the tone fixture; skipping");
                return;
            }
        }
        let audio = reader
            .read(&toned, 48_000, 2)
            .expect("read")
            .expect("audio");
        assert_eq!(audio.sample_rate, 48_000);
        assert_eq!(audio.channels, 2);
        assert!(
            (audio.duration_s() - 2.0).abs() < 0.1,
            "about two seconds, got {}",
            audio.duration_s()
        );
        // ffmpeg's `sine` source is not full scale — this fixture peaks
        // around 0.09 (≈ -20 dBFS), verified by decoding it directly. The
        // point is that samples arrive and are not silence, not their level.
        let peak = audio.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            peak > 0.01,
            "a 440 Hz tone should not be silence (peak {peak})"
        );
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
            chapters: Vec::new(),
            width: 64,
            height: 48,
            fps: 30.0,
            quality: 23,
            audio: None,
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
    /// Chapters given to the encoder come back out of the container.
    #[test]
    fn chapters_land_in_the_container() {
        use crate::VideoEncoder;
        if Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| !s.success())
            .unwrap_or(true)
        {
            eprintln!("ffmpeg unavailable; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-chapters-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("chapters.mp4");
        let spec = EncodeSpec {
            chapters: vec![(0.0, "Open".into()), (1.0, "Pricing = plans".into())],
            width: 64,
            height: 48,
            fps: 30.0,
            quality: 30,
            audio: None,
        };
        let mut encoder: Box<dyn VideoEncoder> =
            Box::new(FfmpegEncoder::open(&out, &spec).expect("encoder"));
        let frame = vec![0u8; 64 * 48 * 4];
        for _ in 0..60 {
            encoder.write_frame(&frame).expect("frame");
        }
        encoder.finish().expect("finish");
        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_chapters",
                "-of",
                "default=noprint_wrappers=0",
            ])
            .arg(&out)
            .output()
            .expect("ffprobe");
        let text = String::from_utf8_lossy(&probe.stdout).to_string();
        let titles: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("TAG:title="))
            .collect();
        assert_eq!(titles, vec!["Open", "Pricing = plans"], "{text}");
        let starts: Vec<f64> = text
            .lines()
            .filter_map(|l| l.strip_prefix("start_time="))
            .map(|v| v.parse::<f64>().unwrap())
            .collect();
        assert_eq!(starts.len(), 2, "{text}");
        assert!((starts[1] - 1.0).abs() < 0.01, "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! A small in-process stand-in for ACE-Step's REST API (`acestep-api`),
//! enough for the generate client: `POST /release_task` (multipart with
//! `src_audio`), `POST /query_result`, `GET /v1/audio`, `GET /health` and
//! `GET /v1/models`, in the documented `{data, code, error}` wrapper. No
//! model: the "layer" it answers is a click on every beat of the `bpm`
//! it was sent, `offset_ms` late — or, with `tones`, sustained sines a
//! bar at a time — as long as the uploaded context cut to a multiple of
//! 1920 samples (lego's latent), so a test controls exactly how far off
//! the grid, or off the chords, a layer is.
//!
//! Shared by promo-midi's tests and promo-ffi's (`#[path]`).

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct State {
    /// `query_result` answers "running" this many times before "done".
    pub polls_before_done: usize,
    /// How late the clicks are, ms.
    pub offset_ms: f64,
    /// Fail the task with this reason.
    pub fail: Option<String>,
    /// Sustained tones instead of clicks: (seconds per bar, the
    /// frequencies of each bar, cycling).
    pub tones: Option<(f64, Vec<Vec<f64>>)>,
    /// Bars (indices into the `tones` cycle) left silent.
    pub silent_bars: Vec<usize>,
    /// Answer at this rate (the context's when None), to exercise resampling.
    pub answer_rate: Option<u32>,
    /// Answer 401 unless `Authorization: Bearer <key>`.
    pub require_key: Option<String>,
    /// Every release's text fields, and the uploaded context's length.
    pub releases: Vec<HashMap<String, String>>,
    pub context_frames: Vec<usize>,
    pub polls: usize,
    tasks: HashMap<String, (usize, Vec<u8>)>,
}

pub struct MockAce {
    pub url: String,
    pub state: Arc<Mutex<State>>,
}

impl MockAce {
    pub fn start() -> MockAce {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State::default()));
        let shared = Arc::clone(&state);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let state = Arc::clone(&shared);
                std::thread::spawn(move || {
                    let _ = serve(stream, &state);
                });
            }
        });
        MockAce { url, state }
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        f(&mut self.state.lock().unwrap())
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn serve(mut stream: TcpStream, state: &Mutex<State>) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 65536];
    let head_end = loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let first = lines.next().unwrap_or_default().to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let length: usize = headers
        .get("content-length")
        .and_then(|l| l.parse().ok())
        .unwrap_or(0);
    while buf.len() < head_end + length {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = &buf[head_end..(head_end + length).min(buf.len())];
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let (status, content_type, payload) = answer(method, path, query, &headers, body, state);
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Error",
    };
    stream.write_all(
        format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n",
            payload.len()
        )
        .as_bytes(),
    )?;
    stream.write_all(&payload)?;
    stream.flush()
}

fn wrap(data: serde_json::Value) -> Vec<u8> {
    serde_json::json!({ "data": data, "code": 200, "error": null,
        "timestamp": 1_700_000_000_000u64, "extra": null })
    .to_string()
    .into_bytes()
}

fn answer(
    method: &str,
    path: &str,
    query: &str,
    headers: &HashMap<String, String>,
    body: &[u8],
    state: &Mutex<State>,
) -> (u16, &'static str, Vec<u8>) {
    let json = "application/json";
    let mut st = state.lock().unwrap();
    if let Some(key) = &st.require_key {
        if headers.get("authorization") != Some(&format!("Bearer {key}")) {
            return (401, json, br#"{"detail":"Unauthorized"}"#.to_vec());
        }
    }
    match (method, path) {
        ("GET", "/health") => (
            200,
            json,
            wrap(
                serde_json::json!({ "status": "ok", "service": "ACE-Step API", "version": "1.0" }),
            ),
        ),
        ("GET", "/v1/models") => (
            200,
            json,
            wrap(
                serde_json::json!({ "models": [{ "name": "acestep-v15-base", "is_default": true }],
                "default_model": "acestep-v15-base" }),
            ),
        ),
        ("POST", "/release_task") => {
            let boundary = headers
                .get("content-type")
                .and_then(|c| c.split("boundary=").nth(1))
                .unwrap_or_default()
                .to_string();
            let (fields, audio) = multipart(body, &boundary);
            let (rate, channels, frames) = wav_shape(&audio.unwrap_or_default());
            let bpm: f64 = fields
                .get("bpm")
                .and_then(|b| b.parse().ok())
                .unwrap_or(120.0);
            let id = format!("task-{}", st.releases.len() + 1);
            let out_rate = st.answer_rate.unwrap_or(rate.max(1));
            let latent = frames / 1920 * 1920;
            let out_frames =
                (latent as f64 * f64::from(out_rate) / f64::from(rate.max(1))) as usize;
            let layer = match &st.tones {
                Some((bar, cycle)) => tones(
                    out_frames,
                    out_rate,
                    channels.max(1),
                    *bar,
                    cycle,
                    &st.silent_bars,
                ),
                None => clicks(out_frames, out_rate, channels.max(1), bpm, st.offset_ms),
            };
            st.releases.push(fields);
            st.context_frames.push(frames);
            st.tasks.insert(id.clone(), (0, layer));
            (
                200,
                json,
                wrap(serde_json::json!({ "task_id": id, "status": "queued", "queue_position": 1 })),
            )
        }
        ("POST", "/query_result") => {
            st.polls += 1;
            let asked: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
            let id = asked["task_id_list"][0]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let wait = st.polls_before_done;
            let fail = st.fail.clone();
            let Some(task) = st.tasks.get_mut(&id) else {
                return (200, json, wrap(serde_json::json!([])));
            };
            task.0 += 1;
            let entry = if task.0 <= wait {
                serde_json::json!({ "task_id": id, "status": 0, "result": "[]" })
            } else if let Some(why) = fail {
                serde_json::json!({ "task_id": id, "status": 2,
                    "result": serde_json::json!([{ "status": 2, "error": why }]).to_string() })
            } else {
                let result = serde_json::json!([{
                    "file": format!("/v1/audio?path=%2Ftmp%2Fapi_audio%2F{id}.wav"),
                    "wave": "", "status": 1, "dit_model": "acestep-v15-base",
                    "seed_value": "1234", "metas": { "bpm": 120 } }]);
                serde_json::json!({ "task_id": id, "status": 1, "result": result.to_string() })
            };
            (200, json, wrap(serde_json::json!([entry])))
        }
        ("GET", "/v1/audio") => {
            let id = query
                .split("%2F")
                .last()
                .unwrap_or_default()
                .trim_end_matches(".wav")
                .to_string();
            match st.tasks.get(&id) {
                Some((_, wav)) => (200, "audio/wav", wav.clone()),
                None => (404, json, br#"{"detail":"no such file"}"#.to_vec()),
            }
        }
        _ => (404, json, br#"{"detail":"not found"}"#.to_vec()),
    }
}

/// The text fields and the `src_audio` part of a multipart body.
fn multipart(body: &[u8], boundary: &str) -> (HashMap<String, String>, Option<Vec<u8>>) {
    let mut fields = HashMap::new();
    let mut audio = None;
    let marker = format!("--{boundary}");
    let mut rest = body;
    while let Some(start) = find(rest, marker.as_bytes()) {
        rest = &rest[start + marker.len()..];
        if rest.starts_with(b"--") {
            break;
        }
        let Some(head_end) = find(rest, b"\r\n\r\n") else {
            break;
        };
        let head = String::from_utf8_lossy(&rest[..head_end]).to_string();
        let content = &rest[head_end + 4..];
        let end = find(content, format!("\r\n{marker}").as_bytes()).unwrap_or(content.len());
        let value = &content[..end];
        let name = head
            .split("name=\"")
            .nth(1)
            .and_then(|n| n.split('"').next())
            .unwrap_or_default()
            .to_string();
        if name == "src_audio" {
            audio = Some(value.to_vec());
        } else {
            fields.insert(name, String::from_utf8_lossy(value).to_string());
        }
        rest = &content[end..];
    }
    (fields, audio)
}

/// (rate, channels, frames) of a PCM WAV.
fn wav_shape(bytes: &[u8]) -> (u32, u16, usize) {
    let mut at = 12;
    let (mut rate, mut channels, mut align) = (48_000u32, 2u16, 6u16);
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        let body = at + 8;
        if id == b"fmt " {
            channels = u16::from_le_bytes([bytes[body + 2], bytes[body + 3]]);
            rate = u32::from_le_bytes(bytes[body + 4..body + 8].try_into().unwrap());
            align = u16::from_le_bytes([bytes[body + 12], bytes[body + 13]]);
        } else if id == b"data" {
            return (
                rate,
                channels,
                size.min(bytes.len() - body) / usize::from(align.max(1)),
            );
        }
        at = body + size + (size & 1);
    }
    (rate, channels, 0)
}

/// A 16-bit WAV of `frames`: each bar's frequencies held through it.
fn tones(
    frames: usize,
    rate: u32,
    channels: u16,
    bar: f64,
    cycle: &[Vec<f64>],
    silent: &[usize],
) -> Vec<u8> {
    let mut samples = vec![0i16; frames * usize::from(channels)];
    for n in 0..frames {
        let t = n as f64 / f64::from(rate);
        let index = ((t / bar) as usize) % cycle.len().max(1);
        if silent.contains(&index) {
            continue;
        }
        let freqs = &cycle[index];
        let v: f64 = freqs
            .iter()
            .map(|f| (2.0 * std::f64::consts::PI * f * t).sin())
            .sum::<f64>()
            * 0.3
            / freqs.len().max(1) as f64;
        for c in 0..usize::from(channels) {
            samples[n * usize::from(channels) + c] = (v * 32767.0) as i16;
        }
    }
    wav16(samples, rate, channels)
}

/// A 16-bit WAV of `frames`: a short 1 kHz burst on every beat, late by
/// `offset_ms`.
fn clicks(frames: usize, rate: u32, channels: u16, bpm: f64, offset_ms: f64) -> Vec<u8> {
    let period = 60.0 / bpm * f64::from(rate);
    let offset = offset_ms / 1000.0 * f64::from(rate);
    let burst = (0.03 * f64::from(rate)) as usize;
    let mut samples = vec![0i16; frames * usize::from(channels)];
    let mut k = 0.0;
    loop {
        let at = (k * period + offset).round() as isize;
        if at >= frames as isize {
            break;
        }
        for i in 0..burst {
            let n = at + i as isize;
            if n < 0 || n as usize >= frames {
                continue;
            }
            let t = i as f64 / f64::from(rate);
            let v = (2.0 * std::f64::consts::PI * 1000.0 * t).sin() * (-t * 120.0).exp() * 0.5;
            for c in 0..usize::from(channels) {
                samples[n as usize * usize::from(channels) + c] = (v * 32767.0) as i16;
            }
        }
        k += 1.0;
    }
    wav16(samples, rate, channels)
}

fn wav16(samples: Vec<i16>, rate: u32, channels: u16) -> Vec<u8> {
    let data = samples.len() * 2;
    let mut out = Vec::with_capacity(44 + data);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
    out.extend_from_slice(&(channels * 2).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data as u32).to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

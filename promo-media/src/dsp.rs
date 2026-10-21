//! The signal tools the beat tracker and the word aligner share
//! (docs/BEAT-SYNC.md §3.4, docs/SPEECH-SYNC.md §4.1): the core's own
//! resampler, a radix-2 FFT with `libm` twiddles, the downmix and the PCM
//! readers. Every transcendental is `libm`'s and every reduction runs in
//! one fixed order in `f64`, so the same samples give the same bits on
//! every machine.

use std::path::Path;

/// Taps of the resampling kernel.
const TAPS: usize = 64;
/// Fractional phases of the resampling kernel.
const PHASES: usize = 256;

/// One frame of interleaved PCM downmixed: the mean of its channels,
/// summed in channel order.
pub fn mix(frame: &[f32]) -> f32 {
    let mut sum = 0.0f64;
    for &s in frame {
        sum += f64::from(s);
    }
    (sum / frame.len() as f64) as f32
}

fn sinc(t: f64) -> f64 {
    if t == 0.0 {
        1.0
    } else {
        let x = std::f64::consts::PI * t;
        libm::sin(x) / x
    }
}

/// From `rate` to `target` with one fixed windowed-sinc kernel: 64 taps,
/// Blackman, cut off at the lower Nyquist, at 256 fractional phases, each
/// phase's taps summing to 1.
pub fn resample(input: &[f32], rate: f64, target: f64) -> Vec<f64> {
    if rate == target {
        return input.iter().map(|&s| f64::from(s)).collect();
    }
    let ratio = rate / target;
    let cutoff = (target / rate).min(1.0);
    let half = (TAPS / 2) as f64;
    let mut kernel = vec![[0.0f64; TAPS]; PHASES];
    for (q, taps) in kernel.iter_mut().enumerate() {
        let frac = q as f64 / PHASES as f64;
        let mut sum = 0.0;
        for (j, tap) in taps.iter_mut().enumerate() {
            let d = j as f64 - (half - 1.0) - frac;
            let w = if d.abs() < half {
                let a = std::f64::consts::PI * d / half;
                0.42 + 0.5 * libm::cos(a) + 0.08 * libm::cos(2.0 * a)
            } else {
                0.0
            };
            *tap = cutoff * sinc(cutoff * d) * w;
            sum += *tap;
        }
        if sum != 0.0 {
            for tap in taps.iter_mut() {
                *tap /= sum;
            }
        }
    }
    let count = (input.len() as f64 / ratio).floor() as usize;
    let mut out = Vec::with_capacity(count);
    for n in 0..count {
        let x = n as f64 * ratio;
        let mut base = x.floor() as i64;
        let mut q = ((x - x.floor()) * PHASES as f64).round() as usize;
        if q == PHASES {
            q = 0;
            base += 1;
        }
        let first = base - (half as i64 - 1);
        let mut acc = 0.0;
        for (j, tap) in kernel[q].iter().enumerate() {
            let k = first + j as i64;
            if k >= 0 && (k as usize) < input.len() {
                acc += f64::from(input[k as usize]) * tap;
            }
        }
        out.push(acc);
    }
    out
}

pub struct Fft {
    cos: Vec<f64>,
    sin: Vec<f64>,
    reversed: Vec<usize>,
}

impl Fft {
    pub fn new(n: usize) -> Fft {
        let bits = n.trailing_zeros();
        let cos = (0..n / 2)
            .map(|k| libm::cos(-2.0 * std::f64::consts::PI * k as f64 / n as f64))
            .collect();
        let sin = (0..n / 2)
            .map(|k| libm::sin(-2.0 * std::f64::consts::PI * k as f64 / n as f64))
            .collect();
        let reversed = (0..n)
            .map(|i| (i.reverse_bits() >> (usize::BITS - bits)) & (n - 1))
            .collect();
        Fft { cos, sin, reversed }
    }

    /// In place, iterative radix-2, decimation in time.
    pub fn run(&self, re: &mut [f64], im: &mut [f64]) {
        let n = re.len();
        for i in 0..n {
            let j = self.reversed[i];
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut size = 2;
        while size <= n {
            let step = n / size;
            for start in (0..n).step_by(size) {
                for k in 0..size / 2 {
                    let (c, s) = (self.cos[k * step], self.sin[k * step]);
                    let (a, b) = (start + k, start + k + size / 2);
                    let tr = re[b] * c - im[b] * s;
                    let ti = re[b] * s + im[b] * c;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            size *= 2;
        }
    }
}

/// Interleaved PCM, its channels and rate.
pub struct Pcm {
    pub samples: Vec<f32>,
    pub channels: usize,
    pub rate: f64,
}

/// A WAV file's PCM (16/24/32-bit integer or 32-bit float), read without
/// ffmpeg. `None` when it is not a WAV this reads.
pub fn read_wav(bytes: &[u8]) -> Option<Pcm> {
    let u16_at = |i: usize| {
        bytes
            .get(i..i + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let u32_at = |i: usize| {
        bytes
            .get(i..i + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    if bytes.get(0..4)? != b"RIFF" || bytes.get(8..12)? != b"WAVE" {
        return None;
    }
    let mut at = 12;
    let mut format: Option<(u16, usize, f64, u16)> = None;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32_at(at + 4)? as usize;
        let body = at + 8;
        if id == b"fmt " {
            let tag = u16_at(body)?;
            let channels = u16_at(body + 2)? as usize;
            let rate = f64::from(u32_at(body + 4)?);
            let bits = u16_at(body + 14)?;
            // WAVE_FORMAT_EXTENSIBLE carries the real tag in its sub-format.
            let tag = if tag == 0xFFFE {
                u16_at(body + 24)?
            } else {
                tag
            };
            format = Some((tag, channels, rate, bits));
        } else if id == b"data" {
            let (tag, channels, rate, bits) = format?;
            let data = bytes.get(body..(body + size).min(bytes.len()))?;
            let samples: Vec<f32> = match (tag, bits) {
                (1, 16) => data
                    .chunks_exact(2)
                    .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0)
                    .collect(),
                (1, 24) => data
                    .chunks_exact(3)
                    .map(|b| (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0)
                    .collect(),
                (1, 32) => data
                    .chunks_exact(4)
                    .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0)
                    .collect(),
                (3, 32) => data
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .collect(),
                _ => return None,
            };
            if channels == 0 {
                return None;
            }
            return Some(Pcm {
                samples,
                channels,
                rate,
            });
        }
        at = body + size + (size & 1);
    }
    None
}

/// The file's PCM at its own rate and channels: a WAV directly, anything
/// else through ffmpeg (f32le, no resampling — the tracker resamples).
pub fn read_pcm(path: &Path) -> Result<Pcm, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if let Some(pcm) = read_wav(&bytes) {
        return Ok(pcm);
    }
    let probe = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=sample_rate,channels",
            "-of",
            "csv=p=0",
        ])
        .arg(crate::tool_path(path))
        .output()
        .map_err(|e| format!("ffprobe is needed to read {}: {e}", path.display()))?;
    let text = String::from_utf8_lossy(&probe.stdout);
    let mut fields = text.trim().split(',');
    let rate: f64 = fields
        .next()
        .and_then(|f| f.trim().parse().ok())
        .ok_or_else(|| format!("{}: no audio stream", path.display()))?;
    let channels: usize = fields
        .next()
        .and_then(|f| f.trim().parse().ok())
        .ok_or_else(|| format!("{}: no audio stream", path.display()))?;
    let output = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(crate::tool_path(path))
        .args(["-vn", "-f", "f32le", "-acodec", "pcm_f32le", "-"])
        .output()
        .map_err(|e| format!("ffmpeg is needed to read {}: {e}", path.display()))?;
    if !output.status.success() {
        return Err(format!(
            "{}: ffmpeg could not read its audio",
            path.display()
        ));
    }
    let samples = output
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    Ok(Pcm {
        samples,
        channels,
        rate,
    })
}

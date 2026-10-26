//! One lego generation end to end, as ACE-Step's handler builds it for
//! `task_type=lego` (the server's `/release_task` path with the base
//! model): the context encoded by the VAE, the instruction "Generate the
//! <TRACK> track based on the audio context:" with the caption and metas,
//! the lyrics `[Instrumental]`, the silence latent as the timbre
//! reference, a full-range chunk mask, 50 Euler steps with APG at
//! guidance 7 and shift 3, then the VAE decode.

use std::path::{Path, PathBuf};
use std::time::Instant;

use candle_core::{DType, Device, Result as CResult, Tensor};
use candle_nn::VarBuilder;

use crate::dit::{ConditionEncoder, CrossKv, Dit, ACOUSTIC, TIMBRE_FRAMES};
use crate::rng::Normal;
use crate::sampler::{self, Conditioning};
use crate::text::{TextEncoder, Tokenizer, MAX_LYRIC_TOKENS, MAX_PROMPT_TOKENS};
use crate::vae::{self, HOP};

/// The checkpoint folders under a model root (the Hugging Face layout).
pub const DIT_DIR: &str = "acestep-v15-base";
pub const VAE_DIR: &str = "vae";
pub const TEXT_DIR: &str = "Qwen3-Embedding-0.6B";
pub const MODEL_NAME: &str = "acestep-v15-base";

/// The least context the model takes, in latents (the reference pads
/// shorter sources to 128 with silence and masks the rest; promo-midi
/// always sends ≥ 30 s, so shorter is refused here).
pub const MIN_LATENTS: usize = 128;

#[derive(Debug, Clone)]
pub struct Lego {
    /// 48 kHz stereo, a whole number of 1920-frame latents.
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    pub track_name: String,
    pub caption: String,
    pub keyscale: Option<String>,
    pub bpm: f64,
    pub time_signature: u32,
    pub seed: u64,
    pub steps: usize,
    pub guidance: f64,
    pub shift: f64,
}

/// The DiT prompt, exactly as `SFT_GEN_PROMPT` formats it.
pub fn prompt(
    track_name: &str,
    caption: &str,
    bpm: f64,
    time_signature: u32,
    keyscale: Option<&str>,
    seconds: usize,
) -> String {
    let bpm = if bpm > 0.0 {
        format!("{}", bpm.round() as i64)
    } else {
        "N/A".into()
    };
    let ts = if time_signature > 0 {
        time_signature.to_string()
    } else {
        "N/A".into()
    };
    let key = keyscale.filter(|k| !k.trim().is_empty()).unwrap_or("N/A");
    let instruction = format!(
        "Generate the {} track based on the audio context:",
        track_name.to_uppercase()
    );
    format!(
        "# Instruction\n{instruction}\n\n# Caption\n{caption}\n\n# Metas\n- bpm: {bpm}\n- timesignature: {ts}\n- keyscale: {key}\n- duration: {seconds} seconds\n<|endoftext|>\n"
    )
}

/// The lyric text for an instrumental layer.
pub fn lyrics() -> &'static str {
    "# Languages\nen\n\n# Lyric\n[Instrumental]<|endoftext|>"
}

/// A generated layer: (left, right) at 48 kHz.
pub type Layer = (Vec<f32>, Vec<f32>);

/// Timings and sizes of one run, for the logs and the perf report.
#[derive(Debug, Clone, Default)]
pub struct Timings {
    pub load_s: f64,
    pub encode_s: f64,
    pub text_s: f64,
    pub diffusion_s: f64,
    pub decode_s: f64,
    pub latents: usize,
}

/// The loaded model: everything a lego generation reads.
pub struct AceStep {
    pub device: Device,
    pub dtype: DType,
    tokenizer: Tokenizer,
    text: TextEncoder,
    encoder: ConditionEncoder,
    dit: Dit,
    vae_enc: vae::Encoder,
    vae_dec: vae::Decoder,
    /// (1, 15000, 64) on the CPU, f32.
    silence: Tensor,
    pub load_s: f64,
}

fn err(e: candle_core::Error) -> String {
    format!("music model: {e}")
}

fn vb(path: &Path, dtype: DType, dev: &Device) -> std::result::Result<VarBuilder<'static>, String> {
    if !path.is_file() {
        return Err(format!("music model file missing: {}", path.display()));
    }
    // Safety: the weights are memory-mapped read-only files the store
    // verified; nothing writes them while the model is loaded.
    unsafe { VarBuilder::from_mmaped_safetensors(&[path.to_path_buf()], dtype, dev) }.map_err(err)
}

/// Reads `silence_latent.pt` (a pickled (1, 64, T) f32 tensor, saved bare,
/// not in a state dict) as (1, T, 64).
pub fn silence_latent(path: &Path) -> std::result::Result<Tensor, String> {
    use candle_core::pickle::{Object, Stack};
    use std::io::Read;
    let bad = |e: String| format!("{}: {e}", path.display());
    let file = std::fs::File::open(path).map_err(|e| bad(e.to_string()))?;
    let mut zip =
        zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| bad(e.to_string()))?;
    let pkl = zip
        .file_names()
        .find(|f| f.ends_with("data.pkl"))
        .map(str::to_string)
        .ok_or_else(|| bad("no data.pkl".into()))?;
    let dir = std::path::PathBuf::from(pkl.trim_end_matches(".pkl"));
    let obj = {
        let mut reader =
            std::io::BufReader::new(zip.by_name(&pkl).map_err(|e| bad(e.to_string()))?);
        let mut stack = Stack::empty();
        stack
            .read_loop(&mut reader)
            .map_err(|e| bad(e.to_string()))?;
        stack.finalize().map_err(|e| bad(e.to_string()))?
    };
    let info = obj
        .into_tensor_info(Object::Unicode("silence".into()), &dir)
        .map_err(|e| bad(e.to_string()))?
        .ok_or_else(|| bad("not a tensor".into()))?;
    if !info.layout.is_contiguous() || info.layout.start_offset() != 0 {
        return Err(bad("unexpected tensor layout".into()));
    }
    let mut bytes = Vec::new();
    zip.by_name(&info.path)
        .map_err(|e| bad(e.to_string()))?
        .read_to_end(&mut bytes)
        .map_err(|e| bad(e.to_string()))?;
    let shape = info.layout.shape().clone();
    let t = Tensor::from_raw_buffer(
        &bytes[..shape.elem_count() * info.dtype.size_in_bytes()],
        info.dtype,
        shape.dims(),
        &Device::Cpu,
    )
    .map_err(err)?;
    t.to_dtype(DType::F32)
        .and_then(|t| t.transpose(1, 2)?.contiguous())
        .map_err(err)
}

impl AceStep {
    pub fn load(root: &Path, device: Device, dtype: DType) -> std::result::Result<AceStep, String> {
        let t0 = Instant::now();
        let p = |a: &str, b: &str| -> PathBuf { root.join(a).join(b) };
        let tokenizer = Tokenizer::load(&p(TEXT_DIR, "tokenizer.json"))?;
        let tvb = vb(&p(TEXT_DIR, "model.safetensors"), dtype, &device)?;
        let text = TextEncoder::load(&tvb).map_err(err)?;
        let dvb = vb(&p(DIT_DIR, "model.safetensors"), dtype, &device)?;
        let encoder = ConditionEncoder::load(&dvb).map_err(err)?;
        let dit = Dit::load(&dvb).map_err(err)?;
        let vvb = vb(
            &p(VAE_DIR, "diffusion_pytorch_model.safetensors"),
            dtype,
            &device,
        )?;
        let vae_enc = vae::Encoder::load(&vvb).map_err(err)?;
        let vae_dec = vae::Decoder::load(&vvb).map_err(err)?;
        let silence = silence_latent(&p(DIT_DIR, "silence_latent.pt"))?;
        Ok(AceStep {
            device,
            dtype,
            tokenizer,
            text,
            encoder,
            dit,
            vae_enc,
            vae_dec,
            silence,
            load_s: t0.elapsed().as_secs_f64(),
        })
    }

    pub fn tokenizer(&self) -> &Tokenizer {
        &self.tokenizer
    }
    pub fn text_encoder(&self) -> &TextEncoder {
        &self.text
    }
    pub fn condition_encoder(&self) -> &ConditionEncoder {
        &self.encoder
    }
    pub fn dit(&self) -> &Dit {
        &self.dit
    }
    pub fn vae_encoder(&self) -> &vae::Encoder {
        &self.vae_enc
    }
    pub fn vae_decoder(&self) -> &vae::Decoder {
        &self.vae_dec
    }

    /// The timbre reference with no reference audio: the first 30 s of
    /// the silence latent, (1, 750, 64).
    pub fn timbre(&self) -> CResult<Tensor> {
        self.silence
            .narrow(1, 0, TIMBRE_FRAMES)?
            .to_dtype(self.dtype)?
            .to_device(&self.device)
    }

    fn to_dev(&self, t: &Tensor) -> CResult<Tensor> {
        t.to_dtype(self.dtype)?.to_device(&self.device)
    }

    /// The cross-attention conditioning for a prompt: the packed encoder
    /// states (1, S, 2048), conditioned.
    pub fn condition(&self, prompt: &str) -> std::result::Result<Tensor, String> {
        let ids = self.tokenizer.encode(prompt, MAX_PROMPT_TOKENS)?;
        let lyric_ids = self.tokenizer.encode(lyrics(), MAX_LYRIC_TOKENS)?;
        let text = self.text.hidden(&ids).map_err(err)?;
        let lyric = self.text.embed(&lyric_ids).map_err(err)?;
        self.encoder
            .forward(&text, &lyric, &self.timbre().map_err(err)?)
            .map_err(err)
    }

    /// Both branches' K/V stacked as batch 2 (conditioned, null).
    pub fn cross_kv(&self, cond: &Tensor) -> CResult<CrossKv> {
        let null = self.dit.null_condition(cond)?;
        self.dit.cross_kv(&Tensor::cat(&[cond, &null], 0)?)
    }

    /// The context latents (1, T, 128): the VAE's posterior sample of the
    /// context, then the chunk mask (ones: lego regenerates every frame).
    pub fn context(&self, src: &Tensor) -> CResult<Tensor> {
        let ones = Tensor::ones(src.shape(), self.dtype, &self.device)?;
        Tensor::cat(&[&self.to_dev(src)?, &ones], 2)
    }

    /// (1, 2, N) audio → (1, T, 64) latents with posterior noise from `noise`.
    pub fn encode(
        &self,
        audio: &Tensor,
        noise: &mut dyn FnMut(usize) -> CResult<Tensor>,
    ) -> CResult<Tensor> {
        let a = self.to_dev(&audio.clamp(-1f32, 1f32)?)?;
        self.vae_enc
            .encode_tiled(&a, noise)?
            .transpose(1, 2)?
            .contiguous()
    }

    /// Decode window in latents: the reference's CPU choice on the CPU
    /// (`PROMOSHOT_MUSIC_DECODE_CHUNK` overrides), 512 on a GPU, whose
    /// per-dispatch cost makes small windows slow.
    pub fn decode_chunk(&self) -> usize {
        std::env::var("PROMOSHOT_MUSIC_DECODE_CHUNK")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&v: &usize| v > 2 * vae::DECODE_OVERLAP)
            .unwrap_or(if self.device.is_cpu() {
                vae::DECODE_CHUNK
            } else {
                512
            })
    }

    /// (1, T, 64) latents → (2, T·1920) audio on the CPU, f32.
    pub fn decode(&self, latents: &Tensor) -> CResult<Tensor> {
        let z = self.to_dev(latents)?.transpose(1, 2)?.contiguous()?;
        let wav = self
            .vae_dec
            .decode_tiled(&z, self.decode_chunk(), vae::DECODE_OVERLAP)?
            .squeeze(0)?;
        // As the reference: a decode peaking over full scale is scaled down to it.
        let peak = wav.abs()?.flatten_all()?.max(0)?.to_scalar::<f32>()?;
        if peak > 1.0 {
            wav / peak as f64
        } else {
            Ok(wav)
        }
    }

    /// The whole lego run. `progress` gets 0…1; `cancel` is polled
    /// between steps. Answers the layer (left, right) at 48 kHz.
    pub fn lego(
        &self,
        req: &Lego,
        progress: &mut dyn FnMut(f64),
        cancel: &dyn Fn() -> bool,
        timings: &mut Timings,
    ) -> std::result::Result<Option<Layer>, String> {
        let n = req.left.len();
        if req.right.len() != n {
            return Err("context channels differ in length".into());
        }
        let latents = n / HOP;
        if latents < MIN_LATENTS {
            return Err(format!(
                "context too short: {} s (the model needs at least {} s)",
                n / 48_000,
                MIN_LATENTS * HOP / 48_000
            ));
        }
        let n = latents * HOP;
        timings.latents = latents;
        let mut inter = Vec::with_capacity(2 * n);
        inter.extend_from_slice(&req.left[..n]);
        inter.extend_from_slice(&req.right[..n]);
        let audio = Tensor::from_vec(inter, (1, 2, n), &Device::Cpu).map_err(err)?;

        // The posterior noise and the initial noise: two streams of the seed.
        let mut post = Normal::new(req.seed ^ 0x5A5A_0F0F_C3C3_9696);
        let t = Instant::now();
        let src = self
            .encode(&audio, &mut |len| {
                Tensor::from_vec(post.fill(ACOUSTIC * len), (1, ACOUSTIC, len), &Device::Cpu)
            })
            .map_err(err)?;
        // Wait for the GPU before the context is used: on Metal, candle
        // recycles a dropped tensor's buffer (here the posterior noise
        // uploaded per window) while kernels queued on it may still be
        // pending, and the context came out wrong without this (the same
        // seed: −4.7 dB against the CPU; with it, 95 dB).
        self.device.synchronize().map_err(err)?;
        timings.encode_s = t.elapsed().as_secs_f64();
        progress(0.05);
        if cancel() {
            return Ok(None);
        }

        let t = Instant::now();
        let seconds = n / 48_000;
        let p = prompt(
            &req.track_name,
            &req.caption,
            req.bpm,
            req.time_signature,
            req.keyscale.as_deref(),
            seconds,
        );
        let cond = self.condition(&p)?;
        let kv = self.cross_kv(&cond).map_err(err)?;
        let context = self.context(&src).map_err(err)?;
        self.device.synchronize().map_err(err)?;
        timings.text_s = t.elapsed().as_secs_f64();

        let mut noise_rng = Normal::new(req.seed);
        let noise = Tensor::from_vec(
            noise_rng.fill(latents * ACOUSTIC),
            (1, latents, ACOUSTIC),
            &Device::Cpu,
        )
        .map_err(err)?;
        let c = Conditioning {
            dit: &self.dit,
            context: &context,
            kv: &kv,
        };
        let t = Instant::now();
        let steps = req.steps.max(1);
        let out = sampler::sample(
            &c,
            &noise,
            steps,
            req.guidance,
            req.shift as f32,
            &mut |i| {
                progress(0.05 + 0.85 * i as f64 / steps as f64);
                !cancel()
            },
        )
        .map_err(err)?;
        timings.diffusion_s = t.elapsed().as_secs_f64();
        let Some(out) = out else { return Ok(None) };

        let t = Instant::now();
        let wav = self.decode(&out).map_err(err)?;
        timings.decode_s = t.elapsed().as_secs_f64();
        progress(1.0);
        let left = wav.get(0).and_then(|t| t.to_vec1::<f32>()).map_err(err)?;
        let right = wav.get(1).and_then(|t| t.to_vec1::<f32>()).map_err(err)?;
        Ok(Some((left, right)))
    }
}

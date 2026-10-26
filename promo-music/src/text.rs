//! The text side: Qwen3-Embedding-0.6B (a 28-layer causal Qwen3) and its
//! tokenizer. ACE-Step conditions on two things from it: the prompt's
//! last hidden state (instruction, caption, metas) and, for the lyrics,
//! only the token embedding table.

use std::path::Path;

use candle_core::{DType, Device, Module, Result, Tensor};
use candle_nn::VarBuilder;

use crate::nn::{window_mask, AttnShape, EncoderLayer, RmsNorm, Rope};

pub const HIDDEN: usize = 1024;
const LAYERS: usize = 28;
const INTER: usize = 3072;
const VOCAB: usize = 151_669;
const SHAPE: AttnShape = AttnShape {
    hidden: HIDDEN,
    heads: 16,
    kv_heads: 8,
    head_dim: 128,
    eps: 1e-6,
};
/// The prompt's token limit (the reference truncates at 256) and the
/// lyrics' (2048).
pub const MAX_PROMPT_TOKENS: usize = 256;
pub const MAX_LYRIC_TOKENS: usize = 2048;

pub struct Tokenizer(tokenizers::Tokenizer);

impl Tokenizer {
    pub fn load(path: &Path) -> std::result::Result<Tokenizer, String> {
        tokenizers::Tokenizer::from_file(path)
            .map(Tokenizer)
            .map_err(|e| format!("tokenizer {}: {e}", path.display()))
    }

    /// Token ids as the reference's `tokenizer(text, truncation=True,
    /// max_length=max)` gives them.
    pub fn encode(&self, text: &str, max: usize) -> std::result::Result<Vec<u32>, String> {
        let enc = self.0.encode(text, true).map_err(|e| format!("tokenize: {e}"))?;
        let mut ids = enc.get_ids().to_vec();
        ids.truncate(max);
        Ok(ids)
    }
}

pub struct TextEncoder {
    embed: Tensor,
    layers: Vec<EncoderLayer>,
    norm: RmsNorm,
    rope: Rope,
    device: Device,
    dtype: DType,
}

impl TextEncoder {
    /// `vb` at the checkpoint root (keys `embed_tokens.*`, `layers.*`, `norm.*`).
    pub fn load(vb: &VarBuilder) -> Result<TextEncoder> {
        let layers = (0..LAYERS)
            .map(|i| EncoderLayer::load(&vb.pp("layers").pp(i), SHAPE, INTER))
            .collect::<Result<Vec<_>>>()?;
        Ok(TextEncoder {
            embed: vb.get((VOCAB, HIDDEN), "embed_tokens.weight")?,
            layers,
            norm: RmsNorm::load(&vb.pp("norm"), HIDDEN, SHAPE.eps)?,
            rope: Rope::new(SHAPE.head_dim, 1_000_000.0, MAX_LYRIC_TOKENS, vb.dtype(), vb.device())?,
            device: vb.device().clone(),
            dtype: vb.dtype(),
        })
    }

    fn ids(&self, ids: &[u32]) -> Result<Tensor> {
        Tensor::new(ids, &self.device)
    }

    /// The token embeddings (1, L, 1024): the lyric conditioning.
    pub fn embed(&self, ids: &[u32]) -> Result<Tensor> {
        self.embed.index_select(&self.ids(ids)?, 0)?.unsqueeze(0)
    }

    /// The last hidden state (1, L, 1024), final norm included, under a
    /// causal mask: the prompt conditioning.
    pub fn hidden(&self, ids: &[u32]) -> Result<Tensor> {
        let mut h = self.embed(ids)?;
        let mask = window_mask(ids.len(), None, true, self.dtype, &self.device)?;
        for layer in &self.layers {
            h = layer.forward(&h, &self.rope, mask.as_ref())?;
        }
        self.norm.forward(&h)
    }
}

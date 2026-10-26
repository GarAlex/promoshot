//! promo-midi's [`LayerGenerator`] over the native model. The weights are
//! loaded on the first generation and dropped after each one (about 12 GB
//! in f32; a reload from the page cache takes about a second), unless
//! `keep` is set — the CLI's one-shot process keeps them.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use promo_midi::generate::{self, LayerAudio, LayerGenerator, LegoRequest, CANCELLED};
use promo_midi::render::Stereo;

use crate::pipeline::{AceStep, Lego, Timings, MODEL_NAME};
use crate::store;

pub struct AceStepGenerator {
    root: PathBuf,
    keep: bool,
    loaded: Mutex<Option<AceStep>>,
}

impl AceStepGenerator {
    pub fn new(root: PathBuf, keep: bool) -> AceStepGenerator {
        AceStepGenerator {
            root,
            keep,
            loaded: Mutex::new(None),
        }
    }
}

impl LayerGenerator for AceStepGenerator {
    fn generate(&self, request: LegoRequest, progress: &mut dyn FnMut(f64), cancel: &dyn Fn() -> bool) -> Result<LayerAudio, String> {
        let mut slot = self.loaded.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            let dev = crate::default_device();
            let dtype = crate::default_dtype(&dev);
            *slot = Some(AceStep::load(&self.root, dev, dtype)?);
        }
        let model = slot.as_ref().expect("loaded");
        let lego = Lego {
            left: request.context.left,
            right: request.context.right,
            track_name: request.track_name,
            caption: request.caption,
            keyscale: request.keyscale,
            bpm: request.bpm,
            time_signature: request.time_signature,
            seed: request.seed as u64,
            steps: request.steps as usize,
            guidance: request.guidance,
            shift: request.shift,
        };
        let mut timings = Timings {
            load_s: model.load_s,
            ..Timings::default()
        };
        let out = model.lego(&lego, progress, cancel, &mut timings);
        if std::env::var_os("PROMOSHOT_MUSIC_TIMINGS").is_some() {
            eprintln!("music: {timings:?} on {:?} {:?}", model.device, model.dtype);
        }
        if !self.keep {
            *slot = None;
        }
        match out? {
            Some((left, right)) => Ok(LayerAudio {
                audio: Stereo { left, right },
                model: Some(MODEL_NAME.into()),
            }),
            None => Err(CANCELLED.into()),
        }
    }
}

/// Registers the native model with promo-midi when its weights are
/// installed in [`store::models_dir`]; answers whether it did. `keep`:
/// hold the weights between generations.
pub fn install_generator(keep: bool) -> bool {
    let dir = store::models_dir();
    if !store::installed(&dir, store::ACE_STEP_BASE) {
        return false;
    }
    let root = store::package_dir(&dir, store::ACE_STEP_BASE);
    generate::set_generator(Some(Arc::new(AceStepGenerator::new(root, keep))));
    true
}

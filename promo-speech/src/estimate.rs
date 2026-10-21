//! The estimate lives in `promo_model::estimate` (the timing pass reads it
//! too); this module re-exports it with the one part that reads a project's
//! receipts: how this voice's own takes ran against the estimate.

use serde_json::Value;

pub use promo_model::estimate::*;

/// How this project's own spoken takes in `provider`/`voice` ran against
/// the estimate: the median of actual ÷ estimated over every narration
/// whose receipt holds and whose length is measured — placeholders and
/// stale takes left out. 1 when there is no take yet; held to 0.6–1.6 so
/// one odd clip cannot run away with it.
pub fn calibration(doc: &Value, provider: &str, voice: &str) -> f64 {
    let mut ratios: Vec<f64> = doc
        .get("resources")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(|resource| {
            let speech = resource.get("speech")?;
            if speech.get("placeholder").and_then(Value::as_bool) == Some(true) {
                return None;
            }
            let text = speech.get("text")?.as_str()?;
            let p = speech
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or("openai");
            let v = speech
                .get("voiceID")
                .and_then(Value::as_str)
                .unwrap_or("alloy");
            if p != provider || v != voice {
                return None;
            }
            let receipt = speech.get("renderedHash")?.as_str()?;
            if receipt != crate::fingerprint(p, v, text) {
                return None;
            }
            let actual = resource.get("duration")?.as_f64()?;
            let (estimated, _, _) = promo_model::estimate::raw_seconds(
                text,
                promo_model::estimate::voice_language(v).as_deref(),
            );
            (estimated > 0.5 && actual > 0.0).then(|| actual / estimated)
        })
        .collect();
    if ratios.is_empty() {
        return 1.0;
    }
    ratios.sort_by(f64::total_cmp);
    let mid = ratios.len() / 2;
    let median = if ratios.len().is_multiple_of(2) {
        (ratios[mid - 1] + ratios[mid]) / 2.0
    } else {
        ratios[mid]
    };
    median.clamp(0.6, 1.6)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Calibration is the median of actual ÷ estimate over this voice's
    /// takes whose receipt holds; placeholders and other voices don't count.
    #[test]
    fn calibration_learns_the_voice() {
        let text = "Narration is written here, not recorded.";
        let estimated = estimate(text, None, 1.0).seconds;
        let take = |voice: &str, duration: f64, placeholder: bool| {
            serde_json::json!({"id": "a", "duration": duration, "speech": {
                "text": text, "provider": "openai", "voiceID": voice,
                "renderedHash": crate::fingerprint("openai", voice, text),
                "placeholder": placeholder }})
        };
        let doc = serde_json::json!({"resources": [
            take("nova", estimated * 1.3, false),
            take("nova", estimated * 1.25, false),
            take("nova", estimated * 1.35, false),
            take("nova", estimated * 9.0, true),
            take("alloy", estimated * 0.7, false),
        ]});
        assert!((calibration(&doc, "openai", "nova") - 1.3).abs() < 1e-9);
        assert!((calibration(&doc, "openai", "alloy") - 0.7).abs() < 1e-9);
        assert_eq!(calibration(&doc, "openai", "echo"), 1.0);
    }
}

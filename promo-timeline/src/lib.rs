//! promo-timeline: pure timeline math — trim/pause mapping, loop folding,
//! keyframe interpolation, layout (media/drawing rects, letterbox), audio
//! gain segments. No I/O, no GPU: this crate must build on every target.
//!
//! P0 seeds the crate with the first real port (loop folding) to establish
//! the pattern: each function mirrors its Swift twin and carries the same
//! test values, so the Phase-1 parity harness diffes them fixture-by-fixture.

/// Result of folding a layer-local time into a looped resource's window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoopFold {
    /// Time within the loop window (== input when not folded).
    pub local: f64,
    /// Unfolded start of the current iteration (0 on the first pass).
    pub offset: f64,
}

/// Folds a layer-local time into the loop window of a resource whose one-loop
/// output length is `period` seconds. Mirrors Swift
/// `ProjectResource.loopFolded(_:)`: identity when `period` is degenerate
/// (≤ 0.01 s) or the time is still inside the first iteration.
pub fn loop_fold(local: f64, period: f64) -> LoopFold {
    if period <= 0.01 || local < period {
        return LoopFold { local, offset: 0.0 };
    }
    let iterations = (local / period).floor();
    LoopFold {
        local: local - iterations * period,
        offset: iterations * period,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Values mirror ReVoiceTests/GifExportTests.testLoopedResource_timeMappingWraps.
    #[test]
    fn folds_into_third_iteration() {
        let fold = loop_fold(25.0, 10.0);
        assert!((fold.local - 5.0).abs() < 1e-9);
        assert!((fold.offset - 20.0).abs() < 1e-9);
    }

    #[test]
    fn identity_inside_first_iteration() {
        assert_eq!(
            loop_fold(3.0, 10.0),
            LoopFold {
                local: 3.0,
                offset: 0.0
            }
        );
    }

    #[test]
    fn identity_for_degenerate_period() {
        assert_eq!(
            loop_fold(25.0, 0.0),
            LoopFold {
                local: 25.0,
                offset: 0.0
            }
        );
        assert_eq!(
            loop_fold(25.0, 0.005),
            LoopFold {
                local: 25.0,
                offset: 0.0
            }
        );
    }

    #[test]
    fn exact_boundary_starts_next_iteration() {
        let fold = loop_fold(10.0, 10.0);
        assert!((fold.local - 0.0).abs() < 1e-9);
        assert!((fold.offset - 10.0).abs() < 1e-9);
    }
}

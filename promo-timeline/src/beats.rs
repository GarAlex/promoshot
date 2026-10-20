//! The beat grid on the OUTPUT timeline (docs/BEAT-SYNC.md §3.1, §3.7).
//!
//! A grid is closed-form: beat index `k` (0 is beat 1, the first downbeat;
//! negative indices are pickups) sits at
//! `t_k = clock + (offset + k · 60/bpm) / speed` — `clock` the output time of
//! the audio's source second 0, `offset` the first downbeat in source
//! seconds, `speed` the audio's playback rate. Plain IEEE arithmetic in a
//! fixed order, so a stated grid times the edit bit-identically on every
//! machine (acceptance criterion 3). Indices are fractional: half a beat is
//! `k + 0.5`. A bar is `meter` beats, and bar `n` starts at beat index
//! `(n − 1) · meter`.
//!
//! Detected grids with drifting tempo (slice 4) will add a piecewise form
//! behind the same methods.

use promo_model::musical::{BeatGrid, MusicalTime, SnapUnit};

/// The tempo musical values resolve against when no clock or no grid says
/// otherwise: 120 BPM, 4/4, beat 1 at 0 s.
pub const NOMINAL_BPM: f64 = 120.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    /// Output time of the audio's source second 0.
    pub clock: f64,
    /// The first downbeat, in source seconds.
    pub offset: f64,
    pub bpm: f64,
    /// The audio's playback rate.
    pub speed: f64,
    /// Beats per bar.
    pub meter: u32,
}

impl Grid {
    /// 120 BPM, 4/4, beat 1 at 0 s.
    pub fn nominal() -> Grid {
        Grid {
            clock: 0.0,
            offset: 0.0,
            bpm: NOMINAL_BPM,
            speed: 1.0,
            meter: 4,
        }
    }

    /// A grid stated on the resource (`beatGrid`), placed by its clock.
    /// Nonsense numbers (a tempo of zero, a zero-beat bar) fall back to the
    /// nominal values rather than dividing by zero.
    pub fn stated(grid: &BeatGrid, clock: f64, speed: f64) -> Grid {
        let bpm = if grid.bpm.is_finite() && grid.bpm > 0.0 {
            grid.bpm
        } else {
            NOMINAL_BPM
        };
        let meter = grid
            .time_signature
            .map(|[beats, _]| beats)
            .filter(|b| *b > 0)
            .unwrap_or(4);
        Grid {
            clock,
            offset: grid.offset.filter(|o| o.is_finite()).unwrap_or(0.0),
            bpm,
            speed: if speed.is_finite() && speed > 0.0 {
                speed
            } else {
                1.0
            },
            meter,
        }
    }

    /// Source seconds per beat.
    fn period(&self) -> f64 {
        60.0 / self.bpm
    }

    /// The output time of beat index `index`.
    pub fn time_at(&self, index: f64) -> f64 {
        self.clock + (self.offset + index * self.period()) / self.speed
    }

    /// The (fractional) beat index at output time `t` — the inverse of
    /// [`Grid::time_at`].
    pub fn index_at(&self, t: f64) -> f64 {
        ((t - self.clock) * self.speed - self.offset) / self.period()
    }

    /// Beats in `bars` bars.
    pub fn bars(&self, bars: f64) -> f64 {
        bars * f64::from(self.meter)
    }

    /// The index of the grid point nearest output time `near`: the nearest
    /// beat, or the nearest bar start. On an exact tie, the earlier one.
    pub fn snap(&self, unit: SnapUnit, near: f64) -> f64 {
        let step = match unit {
            SnapUnit::Beat => 1.0,
            SnapUnit::Downbeat | SnapUnit::Bar => f64::from(self.meter),
        };
        let x = self.index_at(near) / step;
        (x - 0.5).ceil() * step
    }

    /// The beat index a POSITION names: `beat` and `bar` are 1-based,
    /// `snap` the nearest point; a length (`beats`, `bars`) counts from the
    /// clock's beat 1, so `{"beats": n}` is `{"beat": n + 1}`.
    pub fn position(&self, value: &MusicalTime) -> f64 {
        match *value {
            MusicalTime::Beat(b) => b - 1.0,
            MusicalTime::Bar(n) => self.bars(n - 1.0),
            MusicalTime::Beats(n) => n,
            MusicalTime::Bars(n) => self.bars(n),
            MusicalTime::Snap { to, near } => self.snap(to, near),
        }
    }

    /// The beats a LENGTH spans; `None` for a position.
    pub fn length(&self, value: &MusicalTime) -> Option<f64> {
        match *value {
            MusicalTime::Beats(n) => Some(n),
            MusicalTime::Bars(n) => Some(self.bars(n)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nominal_grid_is_two_beats_a_second() {
        let grid = Grid::nominal();
        assert_eq!(grid.time_at(0.0), 0.0);
        assert_eq!(grid.time_at(3.0), 1.5);
        assert_eq!(grid.index_at(1.5), 3.0);
        assert_eq!(grid.position(&MusicalTime::Bar(2.0)), 4.0);
        assert_eq!(grid.position(&MusicalTime::Beat(1.0)), 0.0);
    }

    /// Snap lands on the nearest point, and a tie goes to the earlier one.
    #[test]
    fn snap_picks_the_nearest_and_the_earlier_on_a_tie() {
        let grid = Grid::nominal();
        assert_eq!(grid.snap(SnapUnit::Beat, 1.2), 2.0);
        assert_eq!(grid.snap(SnapUnit::Beat, 1.3), 3.0);
        assert_eq!(grid.snap(SnapUnit::Beat, 1.25), 2.0, "a tie goes earlier");
        assert_eq!(grid.snap(SnapUnit::Bar, 2.9), 4.0);
        assert_eq!(
            grid.snap(SnapUnit::Downbeat, 3.0),
            4.0,
            "a tie between bars 2 and 3 goes to bar 2"
        );
        assert_eq!(grid.snap(SnapUnit::Bar, 0.9), 0.0);
        assert_eq!(grid.snap(SnapUnit::Beat, -0.6), -1.0, "pickups snap too");
    }
}

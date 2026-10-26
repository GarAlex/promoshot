//! Beats to seconds through a tempo map: `[{beat, bpm}]`, each tempo
//! holding from its beat to the next, the first from beat 0.

use promo_model::midi::{MidiBlock, MidiTempo};

#[derive(Debug, Clone)]
pub struct TempoMap {
    /// (beat, bpm), sorted, the first at beat 0.
    points: Vec<(f64, f64)>,
}

fn sane(bpm: f64) -> Option<f64> {
    (bpm.is_finite() && bpm > 0.0).then(|| bpm.clamp(1.0, 1000.0))
}

impl TempoMap {
    /// The map from `points`, `first` before the first point.
    pub fn new(points: &[MidiTempo], first: f64) -> TempoMap {
        let mut out: Vec<(f64, f64)> = points
            .iter()
            .filter(|p| p.beat.is_finite() && p.beat >= 0.0)
            .filter_map(|p| sane(p.bpm).map(|bpm| (p.beat, bpm)))
            .collect();
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        out.dedup_by(|b, a| (a.0 - b.0).abs() < 1e-12);
        if out.first().is_none_or(|p| p.0 > 0.0) {
            out.insert(0, (0.0, sane(first).unwrap_or(120.0)));
        }
        TempoMap { points: out }
    }

    /// A block's map: its `tempoMap`, else `tempo` throughout.
    pub fn from_block(block: &MidiBlock) -> TempoMap {
        TempoMap::new(block.tempo_map.as_deref().unwrap_or(&[]), block.tempo())
    }

    /// Seconds from beat 0 to `beat`.
    pub fn seconds(&self, beat: f64) -> f64 {
        let mut total = 0.0;
        for (i, &(at, bpm)) in self.points.iter().enumerate() {
            let next = self.points.get(i + 1).map_or(f64::INFINITY, |p| p.0);
            if beat <= at {
                break;
            }
            total += (beat.min(next) - at) * 60.0 / bpm;
        }
        total
    }

    /// The tempo in force at `beat`.
    pub fn bpm_at(&self, beat: f64) -> f64 {
        self.points
            .iter()
            .rev()
            .find(|p| p.0 <= beat + 1e-9)
            .map_or(self.points[0].1, |p| p.1)
    }

    /// Whether the tempo changes within `[from, to)`.
    pub fn varies(&self, from: f64, to: f64) -> bool {
        let first = self.bpm_at(from);
        self.points
            .iter()
            .any(|p| p.0 > from + 1e-9 && p.0 < to - 1e-9 && (p.1 - first).abs() > 1e-9)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beats_to_seconds_through_changes() {
        let map = TempoMap::new(
            &[MidiTempo {
                beat: 4.0,
                bpm: 60.0,
                ..Default::default()
            }],
            120.0,
        );
        assert_eq!(map.seconds(0.0), 0.0);
        assert!((map.seconds(4.0) - 2.0).abs() < 1e-12);
        assert!((map.seconds(6.0) - 4.0).abs() < 1e-12);
        assert_eq!(map.bpm_at(5.0), 60.0);
        assert!(map.varies(0.0, 8.0));
        assert!(!map.varies(4.0, 8.0));
        assert!(!map.varies(0.0, 4.0));
    }
}

//! The activity meter: where the action is in a screen recording, from
//! the pixels alone.
//!
//! An iOS screen recording carries no touch data, so the picture is the
//! only witness of what the person did. Auto zoom needs to know where to
//! push in and when to stay wide; this condenses minutes of video into a
//! few KB of [`ActivityTrack`] it can re-plan from without decoding again.
//!
//! The meter is pure: a host decodes (the apps with AVAssetReader through
//! the FFI, the CLI with ffmpeg) and pushes luma planes in time order at
//! about [`MeterOptions::rate`] frames a second. Every frame is box-averaged
//! down to [`ANALYSIS_WIDTH`] px across and compared with the one before it
//! in cells of [`CELL`]×[`CELL`] px. Per frame, in this order:
//!
//! - fixed masks: the phone's status bar and home indicator, and — when the
//!   recorder kept a [`PointerTrack`] — the cursor baked into a Mac capture
//!   at both frames' times. A clock that ticks or a cursor that moves is
//!   not something the person wants zoomed;
//! - a cell has CHANGED when its mean absolute difference clears a noise
//!   floor measured from the recording itself (a low percentile of the
//!   cell differences, frame by frame and smoothed), never below
//!   [`ABS_FLOOR`] luma levels, so H.264 shimmer is not action;
//! - a KEYBOARD (phone portrait): a full-width panel that changes at the
//!   bottom of the picture in one run of frames. The action while it is up
//!   is the text field above, so events inside it are dropped until a
//!   second such run takes it away;
//! - a SCROLL: a large change that a whole-content shift explains (row or
//!   column mean profiles find the shift, the pixels confirm it). The
//!   frame makes no events — scrolling is a reason to stay wide;
//! - a CUT: most unmasked cells changed at once — a new screen. No events;
//! - otherwise the changed cells, minus AMBIENT ones (cells that keep
//!   changing on their own: a playing video, a spinner, a seconds clock),
//!   group into 8-connected components that link over time into events.
//!
//! Ambient needs the future as well as the past (a video that has just
//! started is ambient from its first frame), so frames are held back about
//! [`AMBIENT_WINDOW`] seconds before they make events. Memory stays bounded
//! however long the recording is: a few seconds of sparse frame records.
//!
//! Every threshold below is a named constant tuned against the synthetic
//! scenes in the tests; the meter is deterministic, and a change to any
//! rule that moves its output bumps [`METER_VERSION`] so a stored track is
//! re-measured rather than trusted.

use promo_model::{ActivityTrack, PointerTrack};
use std::collections::VecDeque;

/// The meter's version, written into every track. Bump it when a change
/// here would move the output for the same video.
pub const METER_VERSION: u32 = 1;

/// Frames are analysed this wide, keeping the source's aspect. Enough to
/// see a button light up on a phone or a 1080p desktop; small enough that
/// an hour of recording measures in seconds and averages away pixel noise.
pub const ANALYSIS_WIDTH: usize = 320;

/// Cell side in analysis px: the unit of "something changed here".
pub const CELL: usize = 8;

/// The least mean difference (luma levels, 0–255) a cell must show to have
/// changed, however clean the recording: below this is codec shimmer.
const ABS_FLOOR: f32 = 4.0;
/// The noise floor is this many times the recording's low-percentile cell
/// difference, plus [`NOISE_ADD`] levels.
const NOISE_K: f32 = 2.0;
const NOISE_ADD: f32 = 1.5;
/// Which percentile of a frame's cell differences stands for its noise.
/// Low enough to sit in unchanged cells even when a third of the picture
/// changed; high enough that an I-frame pulse lifting every cell a little
/// lifts the floor with it.
const NOISE_PERCENTILE: f32 = 0.3;
/// How quickly the recording's noise estimate follows each frame's.
const NOISE_SMOOTHING: f32 = 0.05;

/// A cut is at least this share of the unmasked cells changing at once.
/// Higher than a keyboard (about 40% of a phone) or a sheet, lower than
/// any real change of screen.
const CUT_FRACTION: f32 = 0.55;
/// A push animation or a dissolve spans frames: cuts this close are one.
const CUT_MERGE_S: f64 = 0.5;

/// A frame is only tested for a scroll when at least this share of the
/// unmasked cells (and at least [`SCROLL_MIN_CELLS`]) changed.
const SCROLL_MIN_FRACTION: f32 = 0.10;
const SCROLL_MIN_CELLS: usize = 12;
/// The shifted picture must leave at most this share of the unshifted
/// difference for the shift to explain the change.
const SCROLL_EXPLAINED: f32 = 0.4;
/// Scrolling frames this close join into one span, and a large change this
/// soon after a scroll is its momentum settling rather than an event.
const SCROLL_JOIN_S: f64 = 0.35;

/// Ambient is judged in buckets of this many seconds: "changed in this
/// second" is the same evidence at any frame rate, and a seconds clock
/// that changes once a second counts as fully as a video.
const AMBIENT_BUCKET_S: f64 = 1.0;
/// A cell is ambient when, over this many buckets before OR after the
/// frame, it changed in at least [`AMBIENT_MIN_CHANGED`] (and at least
/// three quarters) of the buckets that were measured.
const AMBIENT_WINDOW: i64 = 4;
const AMBIENT_MIN_CHANGED: u32 = 3;
/// A cell is reported as an ambient box once it was ambient this long.
const AMBIENT_REPORT_S: f64 = 1.0;

/// A component links to an event last seen at most this long ago; an
/// event idle longer is closed. About a tap's highlight-and-release.
const LINK_GAP_S: f64 = 0.5;
/// …when their boxes touch (the event's grown by a cell) or their centres
/// are this many cells apart.
const NEAR_CELLS: f64 = 3.0;
/// A single changed cell on its own is usually noise; it is kept when it
/// clears the floor by this many levels — a small button on a big screen.
const STRONG_SINGLE: f32 = 20.0;
/// A cell's contribution to an event's weight saturates at this many
/// levels over the floor.
const EXCESS_FULL: f32 = 40.0;
/// `weight = 1 - exp(-energy / ENERGY_SCALE)`, energy summed over the
/// event's cells and frames in saturated-cell units: a button lighting up
/// and going out on a desktop is about 0.5, a typed sentence 0.8 and up, a
/// faint flicker of a couple of cells next to nothing.
const ENERGY_SCALE: f64 = 24.0;
/// Events this close in time whose boxes overlap by this share of the
/// smaller one are one event.
const MERGE_OVERLAP: f64 = 0.6;

/// A keyboard row has at least this share of its cells changed…
const KEYBOARD_ROW_FRACTION: f32 = 0.5;
/// …and the panel is this tall, as a share of the picture: iPhone
/// keyboards with the suggestion bar are 35–45%.
const KEYBOARD_MIN_HEIGHT: f32 = 0.22;
const KEYBOARD_MAX_HEIGHT: f32 = 0.52;
/// Changes above the panel in the same frame (the text field lighting up)
/// may be at most this share of the panel's.
const KEYBOARD_ABOVE_SHARE: f32 = 0.25;
/// The share of a shown keyboard's cells a cut must change to take it away.
const KEYBOARD_CUT_SHARE: f32 = 0.6;

/// iPhone portrait: the status bar's share of the height from the top, and
/// the home indicator's from the bottom. In landscape the status bar is
/// hidden and the indicator sits in a taller share of the short side.
const PHONE_STATUS_BAR: f64 = 0.055;
const PHONE_HOME_INDICATOR: f64 = 0.025;
const PHONE_HOME_INDICATOR_LANDSCAPE: f64 = 0.05;
/// A picture this long for its width is a phone (iPhones are 2.17; iPads,
/// Macs and 16:9 desktops are well under).
const PHONE_ASPECT: f64 = 1.9;

/// The cursor's box round the pointer position, as shares of the long
/// side: the hotspot is the arrow's tip, so the arrow hangs down-right.
/// Generous, because the track's clock and the frame's are a sample apart.
const POINTER_PAD_BEFORE: f64 = 0.015;
const POINTER_PAD_AFTER: f64 = 0.045;
/// The pointer is masked over this much time either side of each frame.
const POINTER_SLACK_S: f64 = 0.05;

/// Which fixed regions are not action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Masks {
    /// [`Masks::Phone`] for a picture at least 1.9 times as long as it is
    /// wide (unless a pointer track says a Mac recorded it), otherwise
    /// [`Masks::Desktop`].
    #[default]
    Auto,
    /// The status bar and home indicator are ignored.
    Phone,
    /// Nothing fixed: a menu-bar clock is caught as ambient.
    Desktop,
    /// Nothing at all.
    None,
}

/// How the host will feed the meter.
#[derive(Debug, Clone)]
pub struct MeterOptions {
    /// Frames per second the host pushes; recorded in the track. The rules
    /// are in seconds, so another rate measures the same video alike.
    pub rate: f64,
    pub masks: Masks,
    /// The recorder's pointer track, when there is one: its cursor is
    /// masked out of the change.
    pub pointer: Option<PointerTrack>,
}

impl Default for MeterOptions {
    fn default() -> Self {
        Self {
            rate: 10.0,
            masks: Masks::Auto,
            pointer: None,
        }
    }
}

/// The analysis frame for a source of this displayed size: [`ANALYSIS_WIDTH`]
/// across, the source's aspect down. A host that can scale while decoding
/// (ffmpeg) asks for exactly this and saves the meter the averaging.
pub fn analysis_size(source_width: u32, source_height: u32) -> (usize, usize) {
    let w = source_width.max(1) as f64;
    let h = source_height.max(1) as f64;
    let ah = (ANALYSIS_WIDTH as f64 * h / w).round().max(CELL as f64) as usize;
    (ANALYSIS_WIDTH, ah)
}

/// What a measured frame turned out to be.
#[derive(Debug, Clone, Copy, PartialEq)]
enum FrameKind {
    Normal,
    Cut,
    Scroll,
}

/// One measured frame, held back until its ambient window is complete.
struct FrameRecord {
    t: f64,
    dt: f64,
    epoch: u32,
    bucket: i64,
    kind: FrameKind,
    /// Changed cells after masks, with how far each cleared the floor.
    changed: Vec<(u32, f32)>,
}

/// Which cells changed during one second of one screen (an epoch is the
/// stretch between cuts: a new screen starts its ambient history afresh).
struct Bucket {
    epoch: u32,
    index: i64,
    frames: u32,
    changed: Vec<bool>,
}

/// An event while it may still grow. Boxes are cells, `[x0, y0, x1, y1)`.
#[derive(Debug, Clone)]
struct Event {
    t0: f64,
    t1: f64,
    cells: [usize; 4],
    energy: f64,
}

#[derive(Default)]
struct Keyboard {
    /// The top cell row of the keyboard while it is up.
    shown_top: Option<usize>,
    /// The highest top seen in the current run of panel frames.
    run_top: Option<usize>,
}

/// See the module documentation.
pub struct ActivityMeter {
    rate: f64,
    aw: usize,
    ah: usize,
    cw: usize,
    ch: usize,
    phone_portrait: bool,
    static_mask: Vec<bool>,
    pointer: Option<PointerTrack>,
    prev: Vec<u8>,
    cur: Vec<u8>,
    prev_t: Option<f64>,
    column_sums: Vec<u32>,
    noise: Option<f32>,
    keyboard: Keyboard,
    last_scroll_t: Option<f64>,
    last_cut_t: Option<f64>,
    epoch: u32,
    buckets: VecDeque<Bucket>,
    pending: VecDeque<FrameRecord>,
    open: Vec<Event>,
    done: Vec<Event>,
    cuts: Vec<f64>,
    scrolls: Vec<[f64; 2]>,
    ambient_seconds: Vec<f64>,
}

impl ActivityMeter {
    /// A meter for a source of this DISPLAYED size (after rotation).
    pub fn new(source_width: u32, source_height: u32, options: MeterOptions) -> Self {
        let (aw, ah) = analysis_size(source_width, source_height);
        let cw = aw.div_ceil(CELL);
        let ch = ah.div_ceil(CELL);
        let long = source_width.max(source_height).max(1) as f64;
        let short = source_width.min(source_height).max(1) as f64;
        let masks = match options.masks {
            Masks::Auto if long / short >= PHONE_ASPECT && options.pointer.is_none() => {
                Masks::Phone
            }
            Masks::Auto => Masks::Desktop,
            other => other,
        };
        let portrait = ah > aw;
        let mut static_mask = vec![false; cw * ch];
        if masks == Masks::Phone {
            let (top, bottom) = if portrait {
                (PHONE_STATUS_BAR, PHONE_HOME_INDICATOR)
            } else {
                (0.0, PHONE_HOME_INDICATOR_LANDSCAPE)
            };
            let top_px = top * ah as f64;
            let bottom_px = (1.0 - bottom) * ah as f64;
            for cy in 0..ch {
                // A cell touching the band is masked whole: half a clock
                // digit is still a clock.
                let y0 = (cy * CELL) as f64;
                let y1 = ((cy + 1) * CELL).min(ah) as f64;
                if y0 < top_px || y1 > bottom_px {
                    for cx in 0..cw {
                        static_mask[cy * cw + cx] = true;
                    }
                }
            }
        }
        Self {
            rate: if options.rate.is_finite() && options.rate > 0.0 {
                options.rate
            } else {
                10.0
            },
            aw,
            ah,
            cw,
            ch,
            phone_portrait: masks == Masks::Phone && portrait,
            static_mask,
            pointer: options.pointer.filter(|p| !p.samples.is_empty()),
            prev: vec![0; aw * ah],
            cur: vec![0; aw * ah],
            prev_t: None,
            column_sums: Vec::new(),
            noise: None,
            keyboard: Keyboard::default(),
            last_scroll_t: None,
            last_cut_t: None,
            epoch: 0,
            buckets: VecDeque::new(),
            pending: VecDeque::new(),
            open: Vec::new(),
            done: Vec::new(),
            cuts: Vec::new(),
            scrolls: Vec::new(),
            ambient_seconds: vec![0.0; cw * ch],
        }
    }

    /// The analysis frame this meter averages every push down to.
    pub fn analysis_size(&self) -> (usize, usize) {
        (self.aw, self.ah)
    }

    /// One luma frame at source time `t`, any size (`stride` bytes per
    /// row); a frame of another aspect is stretched to the source's. Frames
    /// must arrive in time order: one at or before the last is ignored, as
    /// is a buffer too short for its stated size — a host bug must not
    /// read out of bounds or invent a cut.
    pub fn push(&mut self, t: f64, luma: &[u8], width: usize, height: usize, stride: usize) {
        if width == 0
            || height == 0
            || stride < width
            || luma.len() < (height - 1) * stride + width
            || !t.is_finite()
            || self.prev_t.is_some_and(|p| t <= p)
        {
            return;
        }
        self.downsample(luma, width, height, stride);
        if let Some(prev_t) = self.prev_t {
            self.measure(prev_t, t);
        }
        std::mem::swap(&mut self.prev, &mut self.cur);
        self.prev_t = Some(t);
        self.drain(false);
    }

    /// Everything measured, condensed.
    pub fn finish(mut self) -> ActivityTrack {
        self.drain(true);
        self.done.append(&mut self.open);
        let events = merge_events(std::mem::take(&mut self.done));
        let ambient = self.ambient_boxes();
        ActivityTrack {
            meter: METER_VERSION,
            rate: self.rate,
            events: events
                .iter()
                .map(|e| {
                    let [x, y, w, h] = self.unit_box(e.cells);
                    [
                        round_time(e.t0),
                        round_time(e.t1),
                        round_unit(x),
                        round_unit(y),
                        round_unit(w),
                        round_unit(h),
                        round_unit(1.0 - (-e.energy / ENERGY_SCALE).exp()),
                    ]
                })
                .collect(),
            cuts: self.cuts.iter().map(|&t| round_time(t)).collect(),
            scrolls: self
                .scrolls
                .iter()
                .map(|s| [round_time(s[0]), round_time(s[1])])
                .collect(),
            ambient,
        }
    }

    /// Box-averages the pushed frame into `cur` at the analysis size.
    fn downsample(&mut self, luma: &[u8], width: usize, height: usize, stride: usize) {
        let (aw, ah) = (self.aw, self.ah);
        self.column_sums.clear();
        self.column_sums.resize(width, 0);
        for oy in 0..ah {
            let y0 = oy * height / ah;
            let y1 = ((oy + 1) * height / ah).max(y0 + 1).min(height);
            self.column_sums.iter_mut().for_each(|s| *s = 0);
            for y in y0..y1 {
                let row = &luma[y * stride..y * stride + width];
                for (sum, &v) in self.column_sums.iter_mut().zip(row) {
                    *sum += v as u32;
                }
            }
            for ox in 0..aw {
                let x0 = ox * width / aw;
                let x1 = ((ox + 1) * width / aw).max(x0 + 1).min(width);
                let total: u32 = self.column_sums[x0..x1].iter().sum();
                let count = ((x1 - x0) * (y1 - y0)) as u32;
                self.cur[oy * aw + ox] = ((total + count / 2) / count) as u8;
            }
        }
    }

    /// Compares `cur` (at `t`) with `prev` (at `prev_t`) and records what
    /// the frame was.
    fn measure(&mut self, prev_t: f64, t: f64) {
        let n = self.cw * self.ch;
        let diffs = self.cell_diffs();

        // The floor: this frame's low percentile, and the recording's.
        let mut open_diffs: Vec<f32> = (0..n)
            .filter(|&i| !self.static_mask[i])
            .map(|i| diffs[i])
            .collect();
        let frame_noise = percentile(&mut open_diffs, NOISE_PERCENTILE);
        let noise = self.noise.unwrap_or(frame_noise);
        let floor = ABS_FLOOR.max(NOISE_K * noise + NOISE_ADD);
        let frame_floor = floor.max(NOISE_K * frame_noise + NOISE_ADD);

        let mut mask = self.static_mask.clone();
        self.mask_pointer(&mut mask, prev_t, t);
        let unmasked = mask.iter().filter(|m| !**m).count().max(1);
        // `broad` decides what kind of frame this is against the
        // recording's floor — a cut must not raise its own floor out of
        // sight. `changed` is what events are made of, against the floor
        // this frame's own noise allows.
        let broad: Vec<bool> = (0..n).map(|i| !mask[i] && diffs[i] > floor).collect();
        let mut changed: Vec<bool> = (0..n).map(|i| !mask[i] && diffs[i] > frame_floor).collect();
        let broad_count = broad.iter().filter(|c| **c).count();

        let band = if self.phone_portrait {
            self.keyboard_band(&broad)
        } else {
            None
        };
        let kind = if let Some(top) = band {
            self.keyboard.run_top = Some(self.keyboard.run_top.map_or(top, |r| r.min(top)));
            self.clear_rows_from(&mut changed, top);
            FrameKind::Normal
        } else {
            if let Some(top) = self.keyboard.run_top.take() {
                self.keyboard.shown_top = match self.keyboard.shown_top {
                    None => Some(top),
                    Some(_) => None,
                };
            }
            let large = broad_count >= SCROLL_MIN_CELLS
                && broad_count as f32 >= SCROLL_MIN_FRACTION * unmasked as f32;
            let settling = self
                .last_scroll_t
                .is_some_and(|s| t - s <= SCROLL_JOIN_S + 1e-9);
            if large && (settling || self.explained_by_shift(&broad)) {
                FrameKind::Scroll
            } else if broad_count as f32 >= CUT_FRACTION * unmasked as f32 {
                FrameKind::Cut
            } else {
                if let Some(top) = self.keyboard.shown_top {
                    self.clear_rows_from(&mut changed, top);
                }
                FrameKind::Normal
            }
        };

        let bucket = (t / AMBIENT_BUCKET_S).floor() as i64;
        let mut record = FrameRecord {
            t,
            dt: t - prev_t,
            epoch: self.epoch,
            bucket,
            kind,
            changed: Vec::new(),
        };
        match kind {
            FrameKind::Cut => {
                if self.last_cut_t.is_none_or(|c| t - c > CUT_MERGE_S + 1e-9) {
                    self.cuts.push(t);
                }
                self.last_cut_t = Some(t);
                if let Some(top) = self.keyboard.shown_top {
                    let band_cells = (top * self.cw..n).filter(|&i| !mask[i]).count().max(1);
                    let band_changed = (top * self.cw..n).filter(|&i| broad[i]).count();
                    if band_changed as f32 >= KEYBOARD_CUT_SHARE * band_cells as f32 {
                        self.keyboard.shown_top = None;
                    }
                }
                // A new screen: its ambient history starts afresh.
                self.epoch += 1;
                record.epoch = self.epoch;
            }
            FrameKind::Scroll => {
                // A scroll's first frames move less than a pixel at the
                // analysis size: a shift cannot be confirmed, and every
                // edge on the screen shimmers into an "event". Changed
                // frames just before a scroll are its onset — the mirror
                // of the settling rule after it. They are still held back,
                // so this rewrites what they will make.
                let mut onset = prev_t;
                for held in self.pending.iter_mut().rev() {
                    if held.t < t - SCROLL_JOIN_S - 1e-9 {
                        break;
                    }
                    if held.kind == FrameKind::Normal && !held.changed.is_empty() {
                        held.kind = FrameKind::Scroll;
                        held.changed.clear();
                        onset = onset.min(held.t - held.dt);
                    }
                }
                match self.scrolls.last_mut() {
                    Some(span) if onset - span[1] <= SCROLL_JOIN_S + 1e-9 => span[1] = t,
                    _ => self.scrolls.push([onset, t]),
                }
                self.last_scroll_t = Some(t);
            }
            FrameKind::Normal => {
                self.noise = Some(match self.noise {
                    None => frame_noise,
                    Some(n) => n + NOISE_SMOOTHING * (frame_noise - n),
                });
                record.changed = (0..n)
                    .filter(|&i| changed[i])
                    .map(|i| (i as u32, diffs[i] - frame_floor))
                    .collect();
                let epoch = self.epoch;
                let bucket_slot = match self.buckets.back() {
                    Some(b) if b.epoch == epoch && b.index == bucket => {
                        self.buckets.back_mut().expect("just seen")
                    }
                    _ => {
                        self.buckets.push_back(Bucket {
                            epoch,
                            index: bucket,
                            frames: 0,
                            changed: vec![false; n],
                        });
                        self.buckets.back_mut().expect("just pushed")
                    }
                };
                bucket_slot.frames += 1;
                for &(i, _) in &record.changed {
                    bucket_slot.changed[i as usize] = true;
                }
            }
        }
        self.pending.push_back(record);
    }

    /// Mean absolute difference per cell between `prev` and `cur`.
    fn cell_diffs(&self) -> Vec<f32> {
        let (aw, ah, cw) = (self.aw, self.ah, self.cw);
        let mut sums = vec![0u32; self.cw * self.ch];
        let mut counts = vec![0u32; self.cw * self.ch];
        for y in 0..ah {
            let row = (y / CELL) * cw;
            let p = &self.prev[y * aw..(y + 1) * aw];
            let c = &self.cur[y * aw..(y + 1) * aw];
            for x in 0..aw {
                let i = row + x / CELL;
                sums[i] += p[x].abs_diff(c[x]) as u32;
                counts[i] += 1;
            }
        }
        sums.iter()
            .zip(&counts)
            .map(|(&s, &c)| s as f32 / c.max(1) as f32)
            .collect()
    }

    /// Masks the cells round the cursor at both frames' times.
    fn mask_pointer(&self, mask: &mut [bool], prev_t: f64, t: f64) {
        let Some(track) = &self.pointer else {
            return;
        };
        let long = self.aw.max(self.ah) as f64;
        let before = POINTER_PAD_BEFORE * long;
        let after = POINTER_PAD_AFTER * long;
        for centre in [prev_t, t] {
            for step in 0..=4 {
                let when = centre - POINTER_SLACK_S + POINTER_SLACK_S * 0.5 * step as f64;
                let Some((x, y)) = pointer_at(track, when) else {
                    continue;
                };
                let (px, py) = (x * self.aw as f64, y * self.ah as f64);
                let cx0 = ((px - before) / CELL as f64).floor().max(0.0) as usize;
                let cy0 = ((py - before) / CELL as f64).floor().max(0.0) as usize;
                let cx1 = (((px + after) / CELL as f64).ceil().max(0.0) as usize).min(self.cw);
                let cy1 = (((py + after) / CELL as f64).ceil().max(0.0) as usize).min(self.ch);
                for cy in cy0..cy1 {
                    for cx in cx0..cx1 {
                        mask[cy * self.cw + cx] = true;
                    }
                }
            }
        }
    }

    /// The top cell row of a full-width panel that changed at the bottom of
    /// a phone portrait picture, if this frame shows one of keyboard height
    /// with little else changing.
    fn keyboard_band(&self, broad: &[bool]) -> Option<usize> {
        let cw = self.cw;
        let open_row = |cy: usize| (0..cw).any(|cx| !self.static_mask[cy * cw + cx]);
        let bottom = (0..self.ch).rev().find(|&cy| open_row(cy))?;
        let row_share = |cy: usize| {
            let changed = (0..cw).filter(|&cx| broad[cy * cw + cx]).count();
            changed as f32 / cw as f32
        };
        let mut top = bottom;
        if row_share(bottom) < KEYBOARD_ROW_FRACTION {
            return None;
        }
        while top > 0 && open_row(top - 1) && row_share(top - 1) >= KEYBOARD_ROW_FRACTION {
            top -= 1;
        }
        let height = ((bottom + 1 - top) * CELL) as f32 / self.ah as f32;
        if !(KEYBOARD_MIN_HEIGHT..=KEYBOARD_MAX_HEIGHT).contains(&height) {
            return None;
        }
        let inside = broad[top * cw..].iter().filter(|c| **c).count();
        let above = broad[..top * cw].iter().filter(|c| **c).count();
        (above as f32 <= KEYBOARD_ABOVE_SHARE * inside as f32).then_some(top)
    }

    fn clear_rows_from(&self, changed: &mut [bool], top: usize) {
        changed[top * self.cw..].iter_mut().for_each(|c| *c = false);
    }

    /// Whether a whole-content shift explains this frame's change: the
    /// best shift of the row (or column) mean profiles over the changed
    /// region, confirmed on the pixels themselves.
    fn explained_by_shift(&self, broad: &[bool]) -> bool {
        let (mut cx0, mut cy0, mut cx1, mut cy1) = (usize::MAX, usize::MAX, 0, 0);
        for (i, _) in broad.iter().enumerate().filter(|(_, c)| **c) {
            let (cx, cy) = (i % self.cw, i / self.cw);
            cx0 = cx0.min(cx);
            cy0 = cy0.min(cy);
            cx1 = cx1.max(cx + 1);
            cy1 = cy1.max(cy + 1);
        }
        if cx1 == 0 {
            return false;
        }
        let region = [
            cx0 * CELL,
            cy0 * CELL,
            (cx1 * CELL).min(self.aw),
            (cy1 * CELL).min(self.ah),
        ];
        let [x0, y0, x1, y1] = region;
        let aw = self.aw;
        let rows = |frame: &[u8]| -> Vec<f32> {
            (y0..y1)
                .map(|y| {
                    let s: u32 = frame[y * aw + x0..y * aw + x1]
                        .iter()
                        .map(|&v| v as u32)
                        .sum();
                    s as f32 / (x1 - x0) as f32
                })
                .collect()
        };
        let columns = |frame: &[u8]| -> Vec<f32> {
            (x0..x1)
                .map(|x| {
                    let s: u32 = (y0..y1).map(|y| frame[y * aw + x] as u32).sum();
                    s as f32 / (y1 - y0) as f32
                })
                .collect()
        };
        let vertical = best_shift(&rows(&self.prev), &rows(&self.cur));
        let horizontal = best_shift(&columns(&self.prev), &columns(&self.cur));
        let mut candidates = Vec::new();
        if let Some((s, ratio)) = vertical {
            candidates.push((ratio, 0isize, s));
        }
        if let Some((s, ratio)) = horizontal {
            candidates.push((ratio, s, 0isize));
        }
        candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
        candidates.into_iter().any(|(_, dx, dy)| {
            let (shifted, still) = shifted_error(&self.prev, &self.cur, aw, region, dx, dy);
            still > 0.0 && shifted <= SCROLL_EXPLAINED * still
        })
    }

    /// Makes events from the held-back frames whose ambient window is
    /// complete (all of them when `all`).
    fn drain(&mut self, all: bool) {
        let Some(now_t) = self.prev_t else {
            return;
        };
        let now_bucket = (now_t / AMBIENT_BUCKET_S).floor() as i64;
        while let Some(front) = self.pending.front() {
            let ready =
                all || self.epoch > front.epoch || now_bucket >= front.bucket + AMBIENT_WINDOW;
            if !ready {
                break;
            }
            let record = self.pending.pop_front().expect("front");
            self.process(record);
        }
        // Buckets nothing pending can look back on any more.
        let (keep_epoch, keep_from) = match self.pending.front() {
            Some(f) => (f.epoch, f.bucket - AMBIENT_WINDOW + 1),
            None => (self.epoch, now_bucket - AMBIENT_WINDOW + 1),
        };
        while self
            .buckets
            .front()
            .is_some_and(|b| b.epoch < keep_epoch || (b.epoch == keep_epoch && b.index < keep_from))
        {
            self.buckets.pop_front();
        }
    }

    fn process(&mut self, record: FrameRecord) {
        let t = record.t;
        if record.kind != FrameKind::Normal {
            // The picture moved or was replaced: what was open is over.
            self.done.append(&mut self.open);
            return;
        }
        let n = self.cw * self.ch;
        let mut active = vec![false; n];
        let mut excess = vec![0f32; n];
        let mut ambient_cells = Vec::new();
        {
            let past: Vec<&Bucket> = self
                .buckets
                .iter()
                .filter(|b| {
                    b.epoch == record.epoch
                        && b.index <= record.bucket
                        && b.index > record.bucket - AMBIENT_WINDOW
                })
                .collect();
            let future: Vec<&Bucket> = self
                .buckets
                .iter()
                .filter(|b| {
                    b.epoch == record.epoch
                        && b.index >= record.bucket
                        && b.index < record.bucket + AMBIENT_WINDOW
                })
                .collect();
            let ambient = |cell: usize, window: &[&Bucket]| {
                let measured = window.iter().filter(|b| b.frames > 0).count() as u32;
                let changed = window.iter().filter(|b| b.changed[cell]).count() as u32;
                changed >= AMBIENT_MIN_CHANGED && changed * 4 >= measured * 3
            };
            for &(i, e) in &record.changed {
                let i = i as usize;
                if ambient(i, &past) || ambient(i, &future) {
                    ambient_cells.push(i);
                } else {
                    active[i] = true;
                    excess[i] = e;
                }
            }
        }
        for i in ambient_cells {
            self.ambient_seconds[i] += record.dt;
        }

        for (cells, energy) in components(&active, &excess, self.cw, self.ch) {
            self.link(t, cells, energy);
        }
        let (still_open, idle): (Vec<Event>, Vec<Event>) = std::mem::take(&mut self.open)
            .into_iter()
            .partition(|e| t - e.t1 <= LINK_GAP_S + 1e-9);
        self.open = still_open;
        self.done.extend(idle);
    }

    /// Adds a component seen at `t` to the open event it continues, or
    /// opens one.
    fn link(&mut self, t: f64, cells: [usize; 4], energy: f64) {
        let centre = |b: [usize; 4]| ((b[0] + b[2]) as f64 / 2.0, (b[1] + b[3]) as f64 / 2.0);
        let (cx, cy) = centre(cells);
        let best = self
            .open
            .iter()
            .enumerate()
            .filter(|(_, e)| t - e.t1 <= LINK_GAP_S + 1e-9)
            .filter_map(|(k, e)| {
                let touches = cells[0] < e.cells[2] + 1
                    && cells[2] + 1 > e.cells[0]
                    && cells[1] < e.cells[3] + 1
                    && cells[3] + 1 > e.cells[1];
                let (ex, ey) = centre(e.cells);
                let distance = ((ex - cx).powi(2) + (ey - cy).powi(2)).sqrt();
                (touches || distance <= NEAR_CELLS).then_some((k, distance))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1));
        match best {
            Some((k, _)) => {
                let e = &mut self.open[k];
                e.t1 = t;
                e.cells = union(e.cells, cells);
                e.energy += energy;
            }
            None => self.open.push(Event {
                t0: t,
                t1: t,
                cells,
                energy,
            }),
        }
    }

    /// Boxes of cells that were ambient long enough to report.
    fn ambient_boxes(&self) -> Vec<[f64; 4]> {
        let active: Vec<bool> = self
            .ambient_seconds
            .iter()
            .map(|&s| s >= AMBIENT_REPORT_S - 1e-9)
            .collect();
        let excess = vec![EXCESS_FULL; active.len()];
        let mut boxes: Vec<[usize; 4]> = components(&active, &excess, self.cw, self.ch)
            .into_iter()
            .map(|(b, _)| b)
            .collect();
        boxes.sort();
        boxes
            .into_iter()
            .map(|b| self.unit_box(b).map(round_unit))
            .collect()
    }

    /// Cells `[x0, y0, x1, y1)` to unit `[x, y, w, h]`.
    fn unit_box(&self, cells: [usize; 4]) -> [f64; 4] {
        let x0 = (cells[0] * CELL) as f64;
        let y0 = (cells[1] * CELL) as f64;
        let x1 = (cells[2] * CELL).min(self.aw) as f64;
        let y1 = (cells[3] * CELL).min(self.ah) as f64;
        let (aw, ah) = (self.aw as f64, self.ah as f64);
        [x0 / aw, y0 / ah, (x1 - x0) / aw, (y1 - y0) / ah]
    }
}

/// The pointer's position at `t`, interpolated between samples and held
/// at either end.
fn pointer_at(track: &PointerTrack, t: f64) -> Option<(f64, f64)> {
    let s = &track.samples;
    let first = s.first()?;
    if t <= first[0] {
        return Some((first[1], first[2]));
    }
    let k = s.partition_point(|p| p[0] <= t);
    if k >= s.len() {
        let last = s[s.len() - 1];
        return Some((last[1], last[2]));
    }
    let (a, b) = (s[k - 1], s[k]);
    let span = b[0] - a[0];
    let u = if span > 0.0 { (t - a[0]) / span } else { 1.0 };
    Some((a[1] + (b[1] - a[1]) * u, a[2] + (b[2] - a[2]) * u))
}

/// The `q` percentile of `values` (reordered in place); 0 when empty.
fn percentile(values: &mut [f32], q: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let k = ((values.len() - 1) as f32 * q).round() as usize;
    let (_, v, _) = values.select_nth_unstable_by(k, |a, b| a.total_cmp(b));
    *v
}

/// The non-zero shift `s` that best maps `prev` onto `cur`
/// (`cur[i] ≈ prev[i + s]`, content moving towards lower indices for
/// positive `s`), over at least half the profile, with its error relative
/// to no shift at all (smaller is better). `None` for a profile too short
/// to shift.
fn best_shift(prev: &[f32], cur: &[f32]) -> Option<(isize, f32)> {
    let n = prev.len() as isize;
    let error = |s: isize| {
        let (mut sum, mut count) = (0f32, 0u32);
        for i in 0..n {
            let j = i + s;
            if (0..n).contains(&j) {
                sum += (cur[i as usize] - prev[j as usize]).abs();
                count += 1;
            }
        }
        sum / count.max(1) as f32
    };
    let still = error(0);
    let max = n / 2;
    let (s, e) = (-max..=max)
        .filter(|&s| s != 0)
        .map(|s| (s, error(s)))
        .min_by(|a, b| a.1.total_cmp(&b.1).then(a.0.abs().cmp(&b.0.abs())))?;
    // The constant keeps a flat profile (no structure that way) from
    // reading as a perfect match at every shift.
    Some((s, (e + 0.5) / (still + 0.5)))
}

/// Mean absolute pixel error over `region` with `prev` shifted by
/// `(dx, dy)`, and without the shift, over the same pixels (every other
/// one each way: the verdict needs the ratio, not every pixel).
fn shifted_error(
    prev: &[u8],
    cur: &[u8],
    aw: usize,
    region: [usize; 4],
    dx: isize,
    dy: isize,
) -> (f32, f32) {
    let [x0, y0, x1, y1] = region.map(|v| v as isize);
    let (mut shifted, mut still, mut count) = (0u64, 0u64, 0u64);
    for y in (y0..y1).step_by(2) {
        let sy = y + dy;
        if sy < y0 || sy >= y1 {
            continue;
        }
        for x in (x0..x1).step_by(2) {
            let sx = x + dx;
            if sx < x0 || sx >= x1 {
                continue;
            }
            let c = cur[y as usize * aw + x as usize];
            shifted += c.abs_diff(prev[sy as usize * aw + sx as usize]) as u64;
            still += c.abs_diff(prev[y as usize * aw + x as usize]) as u64;
            count += 1;
        }
    }
    let count = count.max(1) as f32;
    (shifted as f32 / count, still as f32 / count)
}

/// 8-connected components of `active` cells as `([x0, y0, x1, y1), energy)`,
/// in scan order. A component of one cell is dropped unless it clears the
/// floor by [`STRONG_SINGLE`].
fn components(active: &[bool], excess: &[f32], cw: usize, ch: usize) -> Vec<([usize; 4], f64)> {
    let mut seen = vec![false; active.len()];
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for start in 0..active.len() {
        if !active[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let mut b = [usize::MAX, usize::MAX, 0, 0];
        let (mut size, mut energy, mut peak) = (0usize, 0f64, 0f32);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % cw, i / cw);
            b = union(b, [x, y, x + 1, y + 1]);
            size += 1;
            energy += (excess[i] / EXCESS_FULL).clamp(0.0, 1.0) as f64;
            peak = peak.max(excess[i]);
            for ny in y.saturating_sub(1)..=(y + 1).min(ch - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(cw - 1) {
                    let j = ny * cw + nx;
                    if active[j] && !seen[j] {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
        }
        if size > 1 || peak >= STRONG_SINGLE {
            out.push((b, energy));
        }
    }
    out
}

fn union(a: [usize; 4], b: [usize; 4]) -> [usize; 4] {
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
}

/// Folds events that are one burst seen twice: close in time, boxes mostly
/// overlapping. Sorted by start.
fn merge_events(mut events: Vec<Event>) -> Vec<Event> {
    let area = |b: [usize; 4]| ((b[2] - b[0]) * (b[3] - b[1])) as f64;
    loop {
        events.sort_by(|a, b| a.t0.total_cmp(&b.t0).then(a.cells.cmp(&b.cells)));
        let mut merged = false;
        'outer: for i in 0..events.len() {
            for j in i + 1..events.len() {
                let (a, b) = (&events[i], &events[j]);
                let gap = b.t0.max(a.t0) - a.t1.min(b.t1);
                if gap > LINK_GAP_S + 1e-9 {
                    continue;
                }
                let ix = a.cells[2].min(b.cells[2]) as f64 - a.cells[0].max(b.cells[0]) as f64;
                let iy = a.cells[3].min(b.cells[3]) as f64 - a.cells[1].max(b.cells[1]) as f64;
                if ix <= 0.0 || iy <= 0.0 {
                    continue;
                }
                if ix * iy >= MERGE_OVERLAP * area(a.cells).min(area(b.cells)) {
                    let b = events.remove(j);
                    let a = &mut events[i];
                    a.t0 = a.t0.min(b.t0);
                    a.t1 = a.t1.max(b.t1);
                    a.cells = union(a.cells, b.cells);
                    a.energy += b.energy;
                    merged = true;
                    break 'outer;
                }
            }
        }
        if !merged {
            return events;
        }
    }
}

/// Times to the millisecond, unit values to a ten-thousandth: the track
/// is stored in the project, and more digits are noise.
fn round_time(t: f64) -> f64 {
    (t * 1000.0).round() / 1000.0
}

fn round_unit(v: f64) -> f64 {
    (v * 10000.0).round() / 10000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A luma picture to draw synthetic screens on.
    #[derive(Clone)]
    struct Canvas {
        w: usize,
        h: usize,
        px: Vec<u8>,
    }

    impl Canvas {
        fn new(w: usize, h: usize, v: u8) -> Self {
            Self {
                w,
                h,
                px: vec![v; w * h],
            }
        }

        fn rect(&mut self, x: usize, y: usize, w: usize, h: usize, v: u8) {
            for yy in y..(y + h).min(self.h) {
                for xx in x..(x + w).min(self.w) {
                    self.px[yy * self.w + xx] = v;
                }
            }
        }

        /// The same picture `k` times larger, rows padded to `stride` —
        /// what a host pushing full-resolution planes hands over.
        fn scaled(&self, k: usize, stride: usize) -> Vec<u8> {
            let mut out = vec![7u8; stride * self.h * k];
            for y in 0..self.h * k {
                for x in 0..self.w * k {
                    out[y * stride + x] = self.px[(y / k) * self.w + x / k];
                }
            }
            out
        }
    }

    /// A small deterministic generator: tests must not flake.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn range(&mut self, lo: i32, hi: i32) -> i32 {
            lo + (self.next() % (hi - lo + 1) as u64) as i32
        }
    }

    /// A plausible app screen: a header, cards, lines of "text".
    fn ui(w: usize, h: usize, seed: u64) -> Canvas {
        let mut c = Canvas::new(w, h, 236);
        let mut rng = Rng(seed);
        c.rect(0, 0, w, h / 12, 205);
        let mut y = h / 12 + 10;
        while y + 30 < h {
            c.rect(10, y, w - 20, 26, 250);
            let len = rng.range((w / 5) as i32, (w * 3 / 5) as i32) as usize;
            c.rect(16, y + 6, len, 5, 60);
            c.rect(16, y + 15, len / 2, 4, 140);
            y += 34;
        }
        c
    }

    fn feed(meter: &mut ActivityMeter, t: f64, c: &Canvas) {
        meter.push(t, &c.px, c.w, c.h, c.w);
    }

    /// Runs `seconds` of frames at 10 fps, each drawn by `scene(t)`.
    fn measure(
        source: (u32, u32),
        options: MeterOptions,
        seconds: f64,
        scene: impl Fn(f64) -> Canvas,
    ) -> ActivityTrack {
        let mut meter = ActivityMeter::new(source.0, source.1, options);
        let frames = (seconds * 10.0).round() as usize;
        for i in 0..=frames {
            let t = i as f64 / 10.0;
            feed(&mut meter, t, &scene(t));
        }
        meter.finish()
    }

    fn inside(event: &[f64; 7], x: f64, y: f64, w: f64, h: f64) -> bool {
        event[2] <= x + 1e-9
            && event[3] <= y + 1e-9
            && event[2] + event[4] >= x + w - 1e-9
            && event[3] + event[5] >= y + h - 1e-9
    }

    const DESKTOP: (u32, u32) = (1280, 800);
    const PHONE: (u32, u32) = (1170, 2532);

    fn button_scene(t: f64) -> Canvas {
        let mut c = ui(320, 200, 1);
        if (1.0..1.3).contains(&t) {
            c.rect(100, 60, 24, 16, 120);
        }
        c
    }

    /// The button lights at 1.0 and goes out at 1.3: one event, there and
    /// then, its box no more than a cell round the button.
    #[test]
    fn a_button_highlight_is_one_event_there_and_then() {
        let track = measure(DESKTOP, MeterOptions::default(), 3.0, button_scene);
        assert_eq!(track.meter, METER_VERSION);
        assert_eq!(track.events.len(), 1, "{:?}", track.events);
        let e = track.events[0];
        assert_eq!((e[0], e[1]), (1.0, 1.3), "{e:?}");
        let (bx, by, bw, bh) = (100.0 / 320.0, 60.0 / 200.0, 24.0 / 320.0, 16.0 / 200.0);
        assert!(inside(&e, bx, by, bw, bh), "{e:?}");
        let cell_w = CELL as f64 / 320.0;
        let cell_h = CELL as f64 / 200.0;
        assert!(e[4] <= bw + 2.0 * cell_w + 1e-9 && e[5] <= bh + 2.0 * cell_h + 1e-9);
        assert!(e[6] > 0.2 && e[6] < 0.95, "weight {}", e[6]);
        assert!(track.cuts.is_empty() && track.scrolls.is_empty() && track.ambient.is_empty());
    }

    /// The same scene pushed as full-resolution planes with padded rows
    /// measures the same: the host's frame size is the meter's business.
    #[test]
    fn any_frame_size_measures_alike() {
        let small = measure(DESKTOP, MeterOptions::default(), 3.0, button_scene);
        let mut meter = ActivityMeter::new(DESKTOP.0, DESKTOP.1, MeterOptions::default());
        for i in 0..=30 {
            let t = i as f64 / 10.0;
            let big = button_scene(t).scaled(4, 1296);
            meter.push(t, &big, 1280, 800, 1296);
        }
        assert_eq!(meter.finish(), small);
    }

    /// A keyboard comes up (here in one frame), then each keystroke lights
    /// a key and adds a letter to the field above: the events are the
    /// field's, none inside the keyboard.
    #[test]
    fn typing_over_a_keyboard_is_the_text_field() {
        let (w, h) = analysis_size(PHONE.0, PHONE.1);
        let kb_top = (h as f64 * 0.58) as usize;
        let field_y = (h as f64 * 0.3) as usize;
        let scene = |t: f64| {
            let mut c = ui(w, h, 2);
            c.rect(20, field_y - 6, w - 40, 24, 252);
            if t >= 1.0 {
                c.rect(0, kb_top, w, h - kb_top, 200);
                for row in 0..4 {
                    for col in 0..10 {
                        c.rect(4 + col * 31, kb_top + 30 + row * 52, 26, 40, 250);
                    }
                }
            }
            if t >= 1.5 {
                let typed = (((t - 1.5) / 0.3).floor() as usize + 1).min(6);
                for k in 0..typed {
                    c.rect(30 + k * 10, field_y, 6, 10, 30);
                }
                // The key under the finger pops while it is down.
                let key = typed % 10;
                c.rect(4 + key * 31, kb_top + 30 + 52, 26, 40, 120);
            }
            c
        };
        let track = measure(PHONE, MeterOptions::default(), 4.0, scene);
        let kb = kb_top as f64 / h as f64;
        assert!(!track.events.is_empty(), "typing is action");
        for e in &track.events {
            assert!(e[3] + e[5] <= kb + 1e-9, "inside the keyboard: {e:?}");
        }
        let field = track
            .events
            .iter()
            .find(|e| inside(e, 30.0 / w as f64, field_y as f64 / h as f64, 0.1, 0.0));
        assert!(field.is_some(), "{:?}", track.events);
        assert!(track.cuts.is_empty(), "a keyboard is not a new screen");
    }

    /// A long list under a fixed header, scrolled `offset` px (fractions
    /// blend neighbouring rows, as a scaler does with a sub-pixel move).
    fn list_screen(offset: f64) -> Canvas {
        let (w, h) = (320usize, 400usize);
        let mut rng = Rng(5);
        let list_h = 1200;
        let mut list = Canvas::new(w, list_h, 240);
        let mut y = 0;
        while y + 60 < list_h {
            let item = rng.range(30, 56) as usize;
            let len = rng.range(60, 280) as usize;
            let x = rng.range(8, 30) as usize;
            list.rect(x, y + 8, len, 6, rng.range(30, 120) as u8);
            list.rect(x, y + 18, len / 2, 4, 150);
            list.rect(0, y + item - 1, w, 1, 200);
            y += item;
        }
        let mut c = Canvas::new(w, h, 240);
        c.rect(0, 0, w, 48, 190);
        c.rect(12, 18, 90, 10, 40);
        let (whole, frac) = (offset.floor() as usize, offset.fract());
        for yy in 48..h {
            let a = (yy - 48 + whole) * w;
            let b = a + w;
            for x in 0..w {
                let v = list.px[a + x] as f64 * (1.0 - frac) + list.px[b + x] as f64 * frac;
                c.px[yy * w + x] = v.round() as u8;
            }
        }
        c
    }

    /// A list scrolls under a fixed header for a second: one span, no
    /// events, no cut.
    #[test]
    fn a_scrolling_list_is_a_scroll_span() {
        let scene = |t: f64| {
            let steps = ((t.clamp(1.0, 2.0) - 1.0) * 10.0).round();
            list_screen(steps * 9.0)
        };
        let track = measure((1280, 1600), MeterOptions::default(), 3.0, scene);
        assert_eq!(track.scrolls, vec![[1.0, 2.0]], "{track:?}");
        assert!(track.events.is_empty(), "{:?}", track.events);
        assert!(track.cuts.is_empty(), "{:?}", track.cuts);
    }

    /// A slow drag starts with a move under a pixel: no shift can be
    /// confirmed on that frame, and every edge shimmers. It is the scroll's
    /// onset, not a screenful of events.
    #[test]
    fn a_slow_scrolls_first_sub_pixel_frame_is_the_scroll() {
        let scene = |t: f64| {
            let steps = ((t.clamp(0.9, 2.5) - 0.9) * 10.0).round();
            list_screen(if steps == 0.0 { 0.0 } else { steps - 0.6 })
        };
        let track = measure((1280, 1600), MeterOptions::default(), 3.5, scene);
        assert_eq!(track.scrolls.len(), 1, "{track:?}");
        assert_eq!(track.scrolls[0][0], 0.9, "{track:?}");
        assert!(track.events.is_empty(), "{:?}", track.events);
        assert!(track.cuts.is_empty(), "{:?}", track.cuts);
    }

    /// A new screen at 2.0: a cut, and no event from it.
    #[test]
    fn a_new_screen_is_a_cut() {
        let scene = |t: f64| {
            if t < 2.0 {
                ui(320, 200, 3)
            } else {
                let mut c = Canvas::new(320, 200, 40);
                c.rect(30, 30, 200, 100, 180);
                c
            }
        };
        let track = measure(DESKTOP, MeterOptions::default(), 4.0, scene);
        assert_eq!(track.cuts, vec![2.0]);
        assert!(track.events.is_empty(), "{:?}", track.events);
        assert!(track.scrolls.is_empty());
    }

    /// The status-bar clock ticks every second on a phone: nothing.
    #[test]
    fn a_phone_status_bar_clock_is_ignored() {
        let (w, h) = analysis_size(PHONE.0, PHONE.1);
        let scene = |t: f64| {
            let mut c = ui(w, h, 4);
            c.rect(0, 0, w, 36, 236);
            let second = t.floor() as usize;
            for d in 0..4 {
                let v = if (second + d).is_multiple_of(2) {
                    20
                } else {
                    236
                };
                c.rect(24 + d * 9, 12, 7, 14, v);
            }
            c
        };
        let track = measure(PHONE, MeterOptions::default(), 10.0, scene);
        assert_eq!(
            track,
            ActivityTrack {
                meter: METER_VERSION,
                rate: 10.0,
                ..Default::default()
            }
        );
    }

    /// Something animating on its own (a spinner) is ambient: no events
    /// from it, its box reported, and a tap elsewhere still an event.
    #[test]
    fn something_animating_on_its_own_is_ambient() {
        let scene = |t: f64| {
            let mut c = ui(320, 200, 6);
            let mut rng = Rng(1 + (t * 10.0).round() as u64);
            for y in 140..156 {
                for x in 260..276 {
                    c.px[y * 320 + x] = rng.range(0, 255) as u8;
                }
            }
            if (4.0..4.3).contains(&t) {
                c.rect(40, 60, 30, 14, 90);
            }
            c
        };
        let track = measure(DESKTOP, MeterOptions::default(), 8.0, scene);
        assert_eq!(track.ambient.len(), 1, "{:?}", track.ambient);
        let a = track.ambient[0];
        let (sx, sy, sw, sh) = (260.0 / 320.0, 140.0 / 200.0, 16.0 / 320.0, 16.0 / 200.0);
        assert!(
            a[0] <= sx && a[1] <= sy && a[0] + a[2] >= sx + sw && a[1] + a[3] >= sy + sh,
            "{a:?}"
        );
        assert_eq!(track.events.len(), 1, "{:?}", track.events);
        let e = track.events[0];
        assert!(inside(&e, 40.0 / 320.0, 0.3, 30.0 / 320.0, 0.07), "{e:?}");
        assert_eq!(e[0], 4.0);
    }

    /// A seconds clock in a desktop menu bar: ambient too, at the rate it
    /// ticks rather than the rate frames arrive.
    #[test]
    fn a_ticking_desktop_clock_is_ambient() {
        let scene = |t: f64| {
            let mut c = ui(320, 200, 10);
            let second = t.floor() as usize;
            for d in 0..3 {
                let v = if (second + d).is_multiple_of(2) {
                    20
                } else {
                    205
                };
                c.rect(280 + d * 9, 3, 7, 10, v);
            }
            c
        };
        let track = measure(DESKTOP, MeterOptions::default(), 10.0, scene);
        assert_eq!(track.ambient.len(), 1, "{:?}", track.ambient);
        assert!(track.events.is_empty(), "{:?}", track.events);
    }

    /// A static screen through a noisy codec: nothing at all.
    #[test]
    fn codec_noise_on_a_still_screen_is_nothing() {
        let base = ui(320, 200, 7);
        let scene = |t: f64| {
            let mut c = base.clone();
            let mut rng = Rng(1000 + (t * 10.0).round() as u64);
            // Per-pixel shimmer, plus whole blocks nudged as a codec's
            // quantiser does from frame to frame.
            for v in c.px.iter_mut() {
                *v = (*v as i32 + rng.range(-5, 5)).clamp(0, 255) as u8;
            }
            for by in (0..200).step_by(8) {
                for bx in (0..320).step_by(8) {
                    let nudge = rng.range(-2, 2);
                    for y in by..by + 8 {
                        for x in bx..bx + 8 {
                            let p = &mut c.px[y * 320 + x];
                            *p = (*p as i32 + nudge).clamp(0, 255) as u8;
                        }
                    }
                }
            }
            c
        };
        let track = measure(DESKTOP, MeterOptions::default(), 6.0, scene);
        assert!(track.events.is_empty(), "{:?}", track.events);
        assert!(track.cuts.is_empty() && track.scrolls.is_empty() && track.ambient.is_empty());
    }

    /// A Mac capture with the cursor baked in: with the recorder's pointer
    /// track the cursor's travel is not action; without it, it is.
    #[test]
    fn the_baked_in_cursor_is_masked_by_the_pointer_track() {
        let at = |t: f64| {
            let u = ((t - 0.5) / 3.0).clamp(0.0, 1.0);
            (0.2 + 0.6 * u, 0.3 + 0.4 * u)
        };
        let scene = |t: f64| {
            let mut c = ui(320, 200, 8);
            let (x, y) = at(t);
            let (px, py) = ((x * 320.0) as usize, (y * 200.0) as usize);
            c.rect(px, py, 4, 6, 10);
            c.rect(px + 1, py + 1, 2, 3, 255);
            c
        };
        let pointer = PointerTrack {
            samples: (0..=240)
                .map(|i| {
                    let t = i as f64 / 60.0;
                    let (x, y) = at(t);
                    [t, x, y]
                })
                .collect(),
            clicks: Vec::new(),
        };
        let without = measure(DESKTOP, MeterOptions::default(), 4.0, scene);
        assert!(!without.events.is_empty(), "the moving cursor is change");
        let options = MeterOptions {
            pointer: Some(pointer),
            ..Default::default()
        };
        let with = measure(DESKTOP, options, 4.0, scene);
        assert!(with.events.is_empty(), "{:?}", with.events);
    }

    /// A notification badge on a 4K desktop fits inside one cell: a single
    /// cell is noise unless it changes this much, and this one does.
    #[test]
    fn a_tiny_strong_change_is_still_an_event() {
        let scene = |t: f64| {
            let mut c = ui(320, 180, 11);
            if t >= 1.0 {
                c.rect(201, 41, 5, 5, 20);
            }
            c
        };
        let track = measure((3840, 2160), MeterOptions::default(), 3.0, scene);
        assert_eq!(track.events.len(), 1, "{:?}", track.events);
        let e = track.events[0];
        assert_eq!((e[0], e[1]), (1.0, 1.0));
        assert!(inside(
            &e,
            201.0 / 320.0,
            41.0 / 180.0,
            5.0 / 320.0,
            5.0 / 180.0
        ));
        assert!(e[4] <= CELL as f64 / 320.0 + 1e-9, "one cell: {e:?}");
    }

    /// Frames out of order or too short for their size are ignored, not
    /// read out of bounds.
    #[test]
    fn bad_pushes_are_ignored() {
        let mut meter = ActivityMeter::new(DESKTOP.0, DESKTOP.1, MeterOptions::default());
        let c = ui(320, 200, 9);
        feed(&mut meter, 0.0, &c);
        meter.push(0.1, &c.px[..100], 320, 200, 320);
        let mut other = c.clone();
        other.rect(0, 0, 320, 200, 0);
        feed(&mut meter, 0.0, &other);
        feed(&mut meter, 0.1, &c);
        assert!(meter.finish().cuts.is_empty());
    }

    #[test]
    fn analysis_keeps_the_aspect() {
        assert_eq!(analysis_size(1170, 2532), (320, 693));
        assert_eq!(analysis_size(1920, 1080), (320, 180));
        assert_eq!(analysis_size(100, 10), (320, 32));
    }
}

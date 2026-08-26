//! The project model as persisted in `metadata.json`, mirroring the Swift
//! Codable implementations field-for-field — including their tolerant-decode
//! fallbacks and legacy migrations — so decode → encode round-trips are
//! value-identical with the Swift app.
//!
//! Wire conventions (Swift `JSONEncoder`/`JSONDecoder` defaults):
//! - Dates are `f64` seconds since 2001-01-01 (Apple reference date).
//! - UUIDs are uppercase hyphenated strings (kept as opaque `String`s here).
//! - Optionals encoded with `encodeIfPresent` are omitted when nil — modeled
//!   as `Option<T>` with `skip_serializing_if = "Option::is_none"`.
//! - Swift `Float` fields (volume/gain/opacity) are `f32`; `CGFloat` is `f64`.

use crate::geometry::{Point, Rect};
use serde::{Deserialize, Deserializer, Serialize};

fn is_none<T>(v: &Option<T>) -> bool {
    v.is_none()
}

/// Tolerant string-enum decode helper: any unknown raw value maps to the
/// given fallback (mirrors the Swift `init(from:)` overrides that keep old
/// app versions able to open newer projects).
macro_rules! tolerant_enum {
    ($name:ident, $fallback:ident, [$(($variant:ident, $raw:literal)),+ $(,)?]) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
        pub enum $name {
            $(#[serde(rename = $raw)] $variant,)+
        }

        impl $name {
            /// The wire string, from the same literal serde uses.
            pub fn as_str(&self) -> &'static str {
                match self {
                    $($name::$variant => $raw,)+
                }
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(d)?;
                Ok(match raw.as_str() {
                    $($raw => $name::$variant,)+
                    _ => $name::$fallback,
                })
            }
        }
    };
}

/// Strict string enums (Swift uses the synthesized RawRepresentable decode,
/// which throws on unknown values — decoding must fail the same way).
macro_rules! strict_enum {
    ($name:ident, [$(($variant:ident, $raw:literal)),+ $(,)?]) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $raw)] $variant,)+
        }

        impl $name {
            /// The wire string, from the same literal serde uses.
            pub fn as_str(&self) -> &'static str {
                match self {
                    $($name::$variant => $raw,)+
                }
            }
        }
    };
}

strict_enum!(
    ProjectLayerKind,
    [
        (Background, "background"),
        (Video, "video"),
        (Image, "image"),
        (Caption, "caption"),
        (Drawing, "drawing"),
        (Audio, "audio"),
    ]
);
strict_enum!(
    ProjectResourceKind,
    [
        (Video, "video"),
        (Image, "image"),
        (Caption, "caption"),
        (Drawing, "drawing"),
        (Audio, "audio"),
        // A motion path. Never drawn — it is the route a layer takes, not
        // content, which is why it is its own kind rather than a drawing
        // that happens to be used as one. Adding a kind is why any project
        // containing one carries a minReaderVersion: this enum decodes
        // strictly, so an older binary must refuse the file rather than
        // throw halfway through.
        // (Background — a colour/gradient/image plate — rides the same
        // rule at rung 16; see the entry after Path below.)
        (Path, "path"),
        (Background, "background"),
    ]
);
tolerant_enum!(
    ProjectExportKind,
    Images,
    [(Video, "video"), (Images, "images"), (Gif, "gif")]
);
// The shape of a ramp.
//
// Tolerant, and falling back to `Linear`: a file written by a newer build
// that knows more curves should still play here, and playing it with the
// wrong FEEL is a far better failure than refusing to open it. That is also
// why no reader-version gate goes with this — an older reader dropping
// `easing` changes how a move looks, never what the composition contains.
tolerant_enum!(
    Easing,
    Linear,
    [
        (Linear, "linear"),
        (EaseIn, "easeIn"),
        (EaseOut, "easeOut"),
        (EaseInOut, "easeInOut"),
    ]
);

impl Easing {
    /// Reshapes ramp progress. In at 0, out at 1, monotonic between — a
    /// curve that overshot would let a layer arrive somewhere its keyframes
    /// never mention.
    pub fn apply(self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            // Quadratic rather than cubic: gentle enough to be the answer to
            // "this looks mechanical" without becoming a gesture of its own.
            Easing::EaseIn => t * t,
            Easing::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
            // Smoothstep — the workhorse for camera moves.
            Easing::EaseInOut => t * t * (3.0 - 2.0 * t),
        }
    }
}

// The nine places a drawn box can hang from.
tolerant_enum!(
    Anchor,
    Center,
    [
        (TopLeft, "topLeft"),
        (Top, "top"),
        (TopRight, "topRight"),
        (Left, "left"),
        (Center, "center"),
        (Right, "right"),
        (BottomLeft, "bottomLeft"),
        (Bottom, "bottom"),
        (BottomRight, "bottomRight"),
    ]
);

// Sizing relative to the whole canvas.
tolerant_enum!(PlacementMode, Fit, [(Fit, "fit"), (Fill, "fill")]);

/// Where a layer's drawn box sits and how big it is — as a RULE, not as
/// numbers. `height`/`width` are the drawn size in canvas pixels (`mode`
/// sizes against the whole canvas instead); `anchor` hangs the box on a
/// nine-point grid, `offset` nudges it in pixels from there.
///
/// The rule is stored and resolved at every read, never baked into
/// zoom/shift — the same contract as a `@name` colour: swap the screenshot
/// for one of another aspect and "centered, 620 tall" is still exactly
/// that. Resolution needs the source's aspect, which comes from the model
/// (`pixelWidth`/`videoNaturalWidth` on the resource, a sprite's cell), so
/// every reader computes the same numbers. A placement with none of
/// `height`/`width`/`mode` is a position-only rule: the box keeps the
/// keyframe's own zoom and only hangs from the anchor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Placement {
    /// Drawn height in canvas pixels.
    #[serde(default, skip_serializing_if = "is_none")]
    pub height: Option<f64>,
    /// Drawn width in canvas pixels. Needs the source aspect.
    #[serde(default, skip_serializing_if = "is_none")]
    pub width: Option<f64>,
    /// Contain/cover the canvas. Wins under `height`, which wins under
    /// `width`, when more than one is given (validation names the conflict).
    #[serde(default, skip_serializing_if = "is_none")]
    pub mode: Option<PlacementMode>,
    /// Absent means `center`.
    #[serde(default, skip_serializing_if = "is_none")]
    pub anchor: Option<Anchor>,
    /// Pixels from the anchor, `[x, y]`.
    #[serde(default, skip_serializing_if = "is_none")]
    pub offset: Option<[f64; 2]>,
}

impl Placement {
    /// Whether this rule decides the box's SIZE (and so carries the zoom
    /// track) or only where it hangs.
    pub fn sizes(&self) -> bool {
        self.height.is_some() || self.width.is_some() || self.mode.is_some()
    }
}
strict_enum!(
    ImageOrientation,
    [
        (Original, "original"),
        (RotateLeft, "rotateLeft"),
        (RotateRight, "rotateRight"),
        (UpsideDown, "upsideDown"),
    ]
);
tolerant_enum!(
    TransitionKind,
    Fade,
    [
        (Fade, "fade"),
        (Wipe, "wipe"),
        (Slide, "slide"),
        (Push, "push"),
        (Scale, "scale"),
    ]
);
tolerant_enum!(
    TransitionEdge,
    Left,
    [
        (Left, "left"),
        (Right, "right"),
        (Top, "top"),
        (Bottom, "bottom"),
    ]
);

tolerant_enum!(
    BlendMode,
    Normal,
    [
        // Source-over: what every layer does unless it says otherwise.
        (Normal, "normal"),
        // Darkens: white drops out. Vignettes, shadows, paper grain.
        (Multiply, "multiply"),
        // Lightens: black drops out. The mode that makes light-effect
        // packs usable at all — glows, flares and leaks ship on black.
        (Screen, "screen"),
        // Sums: pure light. Hotter than screen, clips sooner.
        (Add, "add"),
    ]
);
tolerant_enum!(
    RevealUnit,
    Word,
    [(Character, "character"), (Word, "word"), (Line, "line"),]
);
tolerant_enum!(
    RevealMode,
    Wipe,
    [
        // Write-on: each unit simply appears. The same walk as the others
        // with an arrival of zero — a typewriter is a stagger that does not
        // animate.
        (Wipe, "wipe"),
        // Each unit arrives with its own little motion, one after another.
        (Fade, "fade"),
        (Rise, "rise"),
        (Scale, "scale"),
        // Karaoke: everything is visible and the current unit is tinted.
        (Highlight, "highlight"),
    ]
);

/// Text arriving a piece at a time — a typewriter, a word-by-word caption,
/// or a karaoke highlight walking the line.
///
/// A RULE, not keyframes. Baking a per-letter reveal would be dozens of
/// keyframes per caption, would go stale the moment the text or the font
/// changed, and would bloat exactly the JSON that automation writes.
///
/// The pace is `secondsPer` (per unit) or `seconds` (for the whole caption),
/// one or the other; `promo validate` names the conflict when a caption
/// states both. With neither, the reveal spreads across the layer's own
/// duration, so it lands with the caption rather than at some fixed rate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextReveal {
    #[serde(default = "reveal_unit_default")]
    pub by: RevealUnit,
    #[serde(default = "reveal_mode_default")]
    pub mode: RevealMode,
    /// Seconds per unit.
    #[serde(default, skip_serializing_if = "is_none")]
    pub seconds_per: Option<f64>,
    /// Seconds for the whole reveal.
    #[serde(default, skip_serializing_if = "is_none")]
    pub seconds: Option<f64>,
    /// How long ONE unit takes to arrive, for the animated modes.
    ///
    /// This is what separates a typewriter from kinetic type: `wipe` is this
    /// at zero — the unit is simply there — and `rise`/`fade`/`scale` give
    /// each unit its own overlapping little move. Absent, each unit overlaps
    /// its neighbour by half an arrival, so several are in flight together —
    /// which is what makes a stagger read as one motion rather than a queue.
    #[serde(default, skip_serializing_if = "is_none")]
    pub unit_seconds: Option<f64>,
    /// How far a `rise` travels, in multiples of the line height — a
    /// proportion rather than pixels, so it survives being drawn at any
    /// density. Absent, half a line: far enough to read as movement, near
    /// enough not to fly.
    #[serde(default, skip_serializing_if = "is_none")]
    pub rise: Option<f64>,
    /// The colour the active unit takes in `highlight` mode. Absent uses the
    /// caption's own text colour, which makes the highlight invisible — so
    /// it is worth stating.
    #[serde(default, skip_serializing_if = "is_none")]
    pub highlight_color_hex: Option<String>,
    /// Shapes the walk across units, not each unit's own arrival.
    #[serde(default, skip_serializing_if = "is_none")]
    pub easing: Option<Easing>,
}

fn reveal_unit_default() -> RevealUnit {
    RevealUnit::Word
}

fn reveal_mode_default() -> RevealMode {
    RevealMode::Wipe
}

impl TextReveal {
    /// Whether each unit ANIMATES in, or simply appears. A typewriter is a
    /// stagger whose units arrive instantly — the same walk, no motion.
    pub fn animates(&self) -> bool {
        matches!(
            self.mode,
            RevealMode::Fade | RevealMode::Rise | RevealMode::Scale
        )
    }

    /// How long the whole reveal takes, given how many units there are and
    /// how long the caption is on screen.
    pub fn total_seconds(&self, units: usize, layer_duration: Option<f64>) -> f64 {
        if let Some(total) = self.seconds.filter(|s| *s > 0.0) {
            return total;
        }
        if let Some(each) = self.seconds_per.filter(|s| *s > 0.0) {
            return each * units.max(1) as f64;
        }
        // Neither stated: spread across the caption's life, so the last unit
        // lands as the caption leaves rather than at an unrelated moment.
        layer_duration.filter(|d| *d > 0.0).unwrap_or(1.0)
    }
}

/// A per-layer colour grade, applied to the layer's own pixels only —
/// the screenshot goes black-and-white while everything around it keeps
/// its colour. NOT an adjustment layer: nothing beneath is touched.
///
/// `saturation` 1 is untouched and 0 is grey; `contrast` 1 is untouched;
/// `brightness` is additive around 0; `tint` multiplies `tintHex` in at
/// `tintAmount` (a gel — 1 is fully gelled). Applied in that order, so
/// saturation 0 plus a warm tint reads as a duotone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerAdjustments {
    #[serde(default, skip_serializing_if = "is_none")]
    pub saturation: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub contrast: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub brightness: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub tint_hex: Option<String>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub tint_amount: Option<f64>,
}

/// A per-layer camera shutter.
///
/// `shutter` is the fraction of one frame interval the shutter stays open —
/// 0.5 is the classic 180 degrees. Frame-relative rather than seconds,
/// because that is the number that keeps the LOOK stable against judder:
/// a cinematographer changing frame rate keeps the angle, not the exposure
/// time. The engine samples the layer's resolved GEOMETRY across that
/// window and averages; the source pixels are decoded once, at the frame's
/// own time. That is a choice, not a shortcut — this blurs the motion the
/// EDITOR added (moves, zooms, pans, rotations, rising words), which is
/// what reads as "rendered" without it. Footage carries its own camera
/// blur already, and re-deriving it would mean N decodes per output frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MotionBlur {
    pub shutter: f64,
}

/// How a layer ENTERS or LEAVES.
///
/// A wipe reveals the layer from an edge without moving it; a slide brings it
/// in from off-canvas; a push does the same and shoves what it is replacing
/// out the opposite side; a scale grows it into place; a fade is the same ramp
/// `fadeIn`/`fadeOut` describe in one number. `from` is the edge the motion
/// starts at and is ignored by fade and scale.
///
/// Push only has something to push at a resource SWAP, where the outgoing
/// material is known. At a layer's own edge there is nothing underneath that
/// belongs to it, so it behaves as a slide.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerTransition {
    pub kind: TransitionKind,
    #[serde(default, skip_serializing_if = "is_none")]
    pub from: Option<TransitionEdge>,
    /// Seconds. A transition longer than the layer is clamped to it.
    pub duration: f64,
    #[serde(default, skip_serializing_if = "is_none")]
    pub easing: Option<Easing>,
}

impl LayerTransition {
    /// The edge the motion comes from, defaulting per kind: a wipe reads
    /// left-to-right like text, a slide comes up from the bottom, which is
    /// how a lower third arrives.
    pub fn edge(&self) -> TransitionEdge {
        self.from.unwrap_or(match self.kind {
            TransitionKind::Slide => TransitionEdge::Bottom,
            // A push reads left-to-right, like reading: the new material
            // comes in from the right and shoves the old one out to the left.
            TransitionKind::Push => TransitionEdge::Right,
            _ => TransitionEdge::Left,
        })
    }
}

strict_enum!(SubtitleVoiceKind, [(Recorded, "recorded")]);
strict_enum!(
    SubtitleTextAlignment,
    [
        (Leading, "leading"),
        (Center, "center"),
        (Trailing, "trailing")
    ]
);
strict_enum!(
    SubtitleFontFamily,
    [
        (System, "system"),
        (Rounded, "rounded"),
        (Serif, "serif"),
        (Monospaced, "monospaced"),
        (HelveticaNeue, "helveticaNeue"),
        (AvenirNext, "avenirNext"),
        (GillSans, "gillSans"),
        (Futura, "futura"),
        (TrebuchetMS, "trebuchetMS"),
        (Georgia, "georgia"),
        (Palatino, "palatino"),
        (TimesNewRoman, "timesNewRoman"),
        (AmericanTypewriter, "americanTypewriter"),
        (CourierNew, "courierNew"),
        (Chalkboard, "chalkboard"),
        (MarkerFelt, "markerFelt"),
        (SnellRoundhand, "snellRoundhand"),
    ]
);
strict_enum!(
    DrawingShapeKind,
    [(Pen, "pen"), (Line, "line"), (Oval, "oval")]
);

// `ResourceFrame.Kind` — legacy "phone" (and any unknown) folds into Device.
tolerant_enum!(
    ResourceFrameKind,
    Device,
    [(None, "none"), (Border, "border"), (Device, "device")]
);

// `ResourceFrame.Material` — unknown values fall back to Space Black.
tolerant_enum!(
    FrameMaterial,
    SpaceBlack,
    [
        (SpaceBlack, "spaceBlack"),
        (NaturalTitanium, "naturalTitanium"),
        (Silver, "silver"),
        (Gold, "gold"),
        (DeepBlue, "deepBlue"),
        (PlasticWhite, "plasticWhite"),
        (PlasticBlack, "plasticBlack"),
        (PlasticBlue, "plasticBlue"),
        (PlasticRed, "plasticRed"),
        (PlasticGreen, "plasticGreen"),
        (PlasticYellow, "plasticYellow"),
        (PlasticPink, "plasticPink"),
    ]
);

// ---------------------------------------------------------------------------
// Subtitles

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SubtitleStyle {
    #[serde(default, skip_serializing_if = "is_none")]
    pub left_margin: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub right_margin: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub vertical_margin: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub font_size: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub alignment: Option<SubtitleTextAlignment>,
    /// The plate around THIS caption. Composition-wide versions of both live
    /// on `CompositionSettings`; these override them, the way every other
    /// look field here does.
    #[serde(default, skip_serializing_if = "is_none")]
    pub padding: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub corner_radius: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub font_family: Option<SubtitleFontFamily>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub is_bold: Option<bool>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub is_italic: Option<bool>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub text_color_hex: Option<String>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub background_color_hex: Option<String>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub background_opacity: Option<f64>,
    /// Outline round the glyphs, in canvas pixels. What lets a caption sit
    /// on FOOTAGE with no plate — the social-caption look — where plain
    /// text turns to mush over a bright frame. Drawn inside the padding, so
    /// a stroke wider than `subtitleBackgroundPadding` is clipped rather
    /// than moving the caption.
    #[serde(default, skip_serializing_if = "is_none")]
    pub stroke_color_hex: Option<String>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub stroke_width: Option<f64>,
    /// A soft shadow under everything else, on the same padding budget.
    #[serde(default, skip_serializing_if = "is_none")]
    pub shadow_color_hex: Option<String>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub shadow_opacity: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub shadow_radius: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub shadow_offset: Option<[f64; 2]>,
    /// Text arriving a piece at a time. Absent means the caption appears
    /// whole, which is what every caption did before this existed — so an
    /// older reader dropping it degrades correctly and no version gate is
    /// needed.
    #[serde(default, skip_serializing_if = "is_none")]
    pub reveal: Option<TextReveal>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubtitleVoiceClip {
    pub filename: String,
    pub kind: SubtitleVoiceKind,
}

/// Mirrors `SubtitleEntry` custom Codable: decode accepts legacy
/// `startTime`/`endTime`; encode emits only `id`/`text`/`time`/`style`
/// (+`voiceClip` when present).
#[derive(Debug, Clone, PartialEq)]
pub struct SubtitleEntry {
    pub id: String,
    pub text: String,
    pub time: f64,
    pub style: SubtitleStyle,
    pub voice_clip: Option<SubtitleVoiceClip>,
    /// Legacy `endTime`, kept in memory for `migratedKeyframes` — never
    /// re-encoded (matches Swift).
    pub legacy_end_time: Option<f64>,
}

#[derive(Deserialize)]
struct SubtitleEntryWire {
    #[serde(default)]
    id: Option<String>,
    text: String,
    #[serde(default)]
    time: Option<f64>,
    #[serde(default, rename = "startTime")]
    start_time: Option<f64>,
    #[serde(default, rename = "endTime")]
    end_time: Option<f64>,
    #[serde(default)]
    style: Option<SubtitleStyle>,
    #[serde(default, rename = "voiceClip")]
    voice_clip: Option<SubtitleVoiceClip>,
}

impl<'de> Deserialize<'de> for SubtitleEntry {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = SubtitleEntryWire::deserialize(d)?;
        Ok(SubtitleEntry {
            // Swift substitutes a random UUID when absent; fixtures always
            // carry one, so an empty id only ever means a hand-built value.
            id: w.id.unwrap_or_default(),
            text: w.text,
            time: w.time.or(w.start_time).unwrap_or(0.0),
            style: w.style.unwrap_or_default(),
            voice_clip: w.voice_clip,
            legacy_end_time: w.end_time,
        })
    }
}

impl Serialize for SubtitleEntry {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire<'a> {
            id: &'a str,
            text: &'a str,
            time: f64,
            style: &'a SubtitleStyle,
            #[serde(skip_serializing_if = "is_none", rename = "voiceClip")]
            voice_clip: &'a Option<SubtitleVoiceClip>,
        }
        Wire {
            id: &self.id,
            text: &self.text,
            time: self.time,
            style: &self.style,
            voice_clip: &self.voice_clip,
        }
        .serialize(s)
    }
}

// ---------------------------------------------------------------------------
// Composition settings + keyframes

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoKeyframe {
    #[serde(default)]
    pub id: String,
    pub time: f64,
    pub zoom: f64,
    pub vertical_shift: f64,
    #[serde(default)]
    pub horizontal_shift: f64,
    #[serde(default = "default_transition")]
    pub transition_duration: f64,
}

fn default_transition() -> f64 {
    0.5
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundKeyframe {
    #[serde(default)]
    pub id: String,
    pub time: f64,
    #[serde(default = "default_black")]
    pub color_hex: String,
    #[serde(default = "default_transition")]
    pub transition_duration: f64,
}

fn default_black() -> String {
    "000000".into()
}

/// Mirrors `CompositionSettings` — every field falls back to its default on
/// decode (Swift uses `try?` per field), and every field is re-encoded.
#[derive(Debug, Clone, PartialEq)]
pub struct CompositionSettings {
    pub canvas_width: f64,
    pub canvas_height: f64,
    /// Frames per second for video output. `None` renders at 30.
    ///
    /// It belongs with the canvas rather than on the command line: a project
    /// that renders differently depending on which flag someone remembered is
    /// not reproducible. Fractional rates are honoured, which matters more
    /// than it sounds — screen recordings are commonly 60000/1001 (59.94), and
    /// rendering those at exactly 30 resamples a source that would have gone
    /// 2:1 into 29.97 untouched.
    pub fps: Option<f64>,
    pub subtitle_left_margin: f64,
    pub subtitle_right_margin: f64,
    pub subtitle_vertical_margin: f64,
    pub subtitle_font_size: f64,
    pub subtitle_font_family: SubtitleFontFamily,
    pub subtitle_bold: bool,
    pub subtitle_italic: bool,
    pub subtitle_color_hex: String,
    pub background_color_hex: String,
    /// The project's default background. When present it replaces the flat
    /// `background_color_hex` — which stays as the fallback, so a project
    /// that never wanted a gradient is unaffected.
    pub background_gradient: Option<BackgroundGradient>,
    /// Named colours any colour field may reference as `@name`.
    pub palette: Option<Vec<PaletteColor>>,
    pub subtitle_background_color_hex: String,
    pub subtitle_background_opacity: f64,
    pub subtitle_background_padding: f64,
    /// Composition-wide caption outline and shadow — the defaults a caption
    /// falls back to, exactly like the colours above.
    pub subtitle_stroke_color_hex: String,
    pub subtitle_stroke_width: f64,
    pub subtitle_shadow_color_hex: String,
    pub subtitle_shadow_opacity: f64,
    pub subtitle_shadow_radius: f64,
    /// Where the shadow falls, in canvas pixels. `None` keeps the derived
    /// drop — straight down by half the blur — which is what every project
    /// authored before this field expects.
    pub subtitle_shadow_offset: Option<[f64; 2]>,
    /// The reveal every caption falls back to — a project that types all its
    /// captions says it once here.
    pub subtitle_reveal: Option<TextReveal>,
    pub subtitle_background_corner_radius: f64,
    /// What a caption that never chose an alignment gets. Centre, which is
    /// what the renderer already fell back to.
    pub subtitle_alignment: SubtitleTextAlignment,
    pub video_border_color_hex: String,
    pub video_border_width: f64,
    pub video_corner_radius: f64,
    /// Composition-wide drop shadow under media layers (video/image) — the
    /// default a resource frame's own shadow fields fall back to, exactly
    /// as captions fall back to `subtitle_shadow_*`. OFF by default: a
    /// shadow appearing under every existing layer would change what every
    /// project already renders.
    pub video_shadow_color_hex: String,
    pub video_shadow_opacity: f64,
    /// Penumbra length in canvas px: the shadow fades out over this
    /// distance beyond the layer's edge.
    pub video_shadow_radius: f64,
    /// Where the shadow falls, in canvas pixels. `None` keeps the derived
    /// drop — straight down by half the blur — the caption rule.
    pub video_shadow_offset: Option<[f64; 2]>,
    pub video_keyframes: Vec<VideoKeyframe>,
    pub background_keyframes: Vec<BackgroundKeyframe>,
    pub image_export_scale_percent: f64,
    pub gif_export_scale_percent: f64,
    pub gif_export_fps: f64,
    pub video_export_width: Option<f64>,
    pub video_export_height: Option<f64>,
}

impl Default for CompositionSettings {
    fn default() -> Self {
        CompositionSettings {
            canvas_width: 1920.0,
            canvas_height: 1080.0,
            fps: None,
            // Symmetric, and low in the frame. The old defaults — 720 left
            // against 60 right — came from captions set beside a phone
            // screenshot: on a 1920 canvas they put the text panel at
            // x=720..1860, centred 330px right of the frame's centre, with
            // its top edge 80px down. Every one of the ten shipped templates
            // corrected them to 120/120, and promo-text's own TextStyle
            // default was symmetric all along.
            subtitle_left_margin: 120.0,
            subtitle_right_margin: 120.0,
            // Measured from the TOP, so this is a position, not an inset:
            // 880 on the default 1080-tall canvas is the lower third, where
            // a caption belongs. 80 put it against the top edge.
            subtitle_vertical_margin: 880.0,
            subtitle_font_size: 72.0,
            subtitle_font_family: SubtitleFontFamily::System,
            subtitle_bold: true,
            subtitle_italic: false,
            subtitle_color_hex: "FFFFFF".into(),
            background_color_hex: "000000".into(),
            background_gradient: None,
            palette: None,
            subtitle_background_color_hex: "000000".into(),
            subtitle_background_opacity: 0.7,
            subtitle_background_padding: 16.0,
            // Off by default: a stroke appearing on every existing caption
            // would change what every project already renders.
            subtitle_stroke_color_hex: "000000".into(),
            subtitle_stroke_width: 0.0,
            subtitle_shadow_color_hex: "000000".into(),
            subtitle_shadow_opacity: 0.0,
            subtitle_shadow_radius: 0.0,
            subtitle_shadow_offset: None,
            subtitle_reveal: None,
            subtitle_alignment: SubtitleTextAlignment::Center,
            subtitle_background_corner_radius: 8.0,
            video_border_color_hex: "FFFFFF".into(),
            video_border_width: 0.0,
            video_corner_radius: 0.0,
            video_shadow_color_hex: "000000".into(),
            video_shadow_opacity: 0.0,
            video_shadow_radius: 0.0,
            video_shadow_offset: None,
            // The layer timeline owns keyframes now, and SkillDriftTests
            // lists these two as fields an assistant must not write — yet
            // every new project was born carrying one of each. Empty is what
            // a project with no legacy timeline should say; both readers
            // already treat an empty list as "no legacy timeline" (identity
            // transform, and the flat background colour).
            video_keyframes: Vec::new(),
            background_keyframes: Vec::new(),
            image_export_scale_percent: 100.0,
            gif_export_scale_percent: 33.0,
            gif_export_fps: 10.0,
            video_export_width: None,
            video_export_height: None,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct CompositionSettingsWire {
    background_gradient: Option<BackgroundGradient>,
    palette: Option<Vec<PaletteColor>>,
    canvas_width: Option<f64>,
    fps: Option<f64>,
    canvas_height: Option<f64>,
    subtitle_left_margin: Option<f64>,
    subtitle_right_margin: Option<f64>,
    subtitle_vertical_margin: Option<f64>,
    subtitle_font_size: Option<f64>,
    subtitle_font_family: Option<SubtitleFontFamily>,
    subtitle_bold: Option<bool>,
    subtitle_italic: Option<bool>,
    subtitle_color_hex: Option<String>,
    background_color_hex: Option<String>,
    subtitle_background_color_hex: Option<String>,
    subtitle_background_opacity: Option<f64>,
    subtitle_background_padding: Option<f64>,
    subtitle_stroke_color_hex: Option<String>,
    subtitle_stroke_width: Option<f64>,
    subtitle_shadow_color_hex: Option<String>,
    subtitle_shadow_opacity: Option<f64>,
    subtitle_shadow_radius: Option<f64>,
    subtitle_shadow_offset: Option<[f64; 2]>,
    subtitle_reveal: Option<TextReveal>,
    subtitle_background_corner_radius: Option<f64>,
    subtitle_alignment: Option<SubtitleTextAlignment>,
    video_border_color_hex: Option<String>,
    video_border_width: Option<f64>,
    video_corner_radius: Option<f64>,
    video_shadow_color_hex: Option<String>,
    video_shadow_opacity: Option<f64>,
    video_shadow_radius: Option<f64>,
    video_shadow_offset: Option<[f64; 2]>,
    video_keyframes: Option<Vec<VideoKeyframe>>,
    background_keyframes: Option<Vec<BackgroundKeyframe>>,
    image_export_scale_percent: Option<f64>,
    gif_export_scale_percent: Option<f64>,
    #[serde(rename = "gifExportFPS")]
    gif_export_fps: Option<f64>,
    video_export_width: Option<f64>,
    video_export_height: Option<f64>,
}

impl<'de> Deserialize<'de> for CompositionSettings {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = CompositionSettingsWire::deserialize(d)?;
        let dflt = CompositionSettings::default();
        let background_color_hex = w
            .background_color_hex
            .unwrap_or(dflt.background_color_hex.clone());
        Ok(CompositionSettings {
            background_gradient: w.background_gradient,
            palette: w.palette,
            canvas_width: w.canvas_width.unwrap_or(dflt.canvas_width),
            // Absent means "renders at 30", which is not the same as 30 being
            // written down — so it stays None rather than defaulting here.
            fps: w.fps,
            canvas_height: w.canvas_height.unwrap_or(dflt.canvas_height),
            subtitle_left_margin: w.subtitle_left_margin.unwrap_or(dflt.subtitle_left_margin),
            subtitle_right_margin: w
                .subtitle_right_margin
                .unwrap_or(dflt.subtitle_right_margin),
            subtitle_vertical_margin: w
                .subtitle_vertical_margin
                .unwrap_or(dflt.subtitle_vertical_margin),
            subtitle_font_size: w.subtitle_font_size.unwrap_or(dflt.subtitle_font_size),
            subtitle_font_family: w.subtitle_font_family.unwrap_or(dflt.subtitle_font_family),
            subtitle_bold: w.subtitle_bold.unwrap_or(dflt.subtitle_bold),
            subtitle_italic: w.subtitle_italic.unwrap_or(dflt.subtitle_italic),
            subtitle_color_hex: w.subtitle_color_hex.unwrap_or(dflt.subtitle_color_hex),
            subtitle_background_color_hex: w
                .subtitle_background_color_hex
                .unwrap_or(dflt.subtitle_background_color_hex),
            subtitle_background_opacity: w
                .subtitle_background_opacity
                .unwrap_or(dflt.subtitle_background_opacity),
            subtitle_background_padding: w
                .subtitle_background_padding
                .unwrap_or(dflt.subtitle_background_padding),
            subtitle_stroke_color_hex: w
                .subtitle_stroke_color_hex
                .unwrap_or(dflt.subtitle_stroke_color_hex.clone()),
            subtitle_stroke_width: w
                .subtitle_stroke_width
                .unwrap_or(dflt.subtitle_stroke_width),
            subtitle_shadow_color_hex: w
                .subtitle_shadow_color_hex
                .unwrap_or(dflt.subtitle_shadow_color_hex.clone()),
            subtitle_shadow_opacity: w
                .subtitle_shadow_opacity
                .unwrap_or(dflt.subtitle_shadow_opacity),
            subtitle_shadow_radius: w
                .subtitle_shadow_radius
                .unwrap_or(dflt.subtitle_shadow_radius),
            subtitle_shadow_offset: w.subtitle_shadow_offset,
            subtitle_reveal: w.subtitle_reveal,
            subtitle_alignment: w.subtitle_alignment.unwrap_or(dflt.subtitle_alignment),
            subtitle_background_corner_radius: w
                .subtitle_background_corner_radius
                .unwrap_or(dflt.subtitle_background_corner_radius),
            video_border_color_hex: w
                .video_border_color_hex
                .unwrap_or(dflt.video_border_color_hex),
            video_border_width: w.video_border_width.unwrap_or(dflt.video_border_width),
            video_corner_radius: w.video_corner_radius.unwrap_or(dflt.video_corner_radius),
            video_shadow_color_hex: w
                .video_shadow_color_hex
                .unwrap_or(dflt.video_shadow_color_hex.clone()),
            video_shadow_opacity: w
                .video_shadow_opacity
                .unwrap_or(dflt.video_shadow_opacity),
            video_shadow_radius: w.video_shadow_radius.unwrap_or(dflt.video_shadow_radius),
            video_shadow_offset: w.video_shadow_offset,
            video_keyframes: w.video_keyframes.unwrap_or(dflt.video_keyframes),
            // Swift's fallback anchors the default background keyframe on the
            // *decoded* backgroundColorHex, not the constant.
            background_keyframes: w.background_keyframes.unwrap_or_else(|| {
                vec![BackgroundKeyframe {
                    id: String::new(),
                    time: 0.0,
                    color_hex: background_color_hex.clone(),
                    transition_duration: 0.5,
                }]
            }),
            background_color_hex,
            image_export_scale_percent: w
                .image_export_scale_percent
                .unwrap_or(dflt.image_export_scale_percent),
            gif_export_scale_percent: w
                .gif_export_scale_percent
                .unwrap_or(dflt.gif_export_scale_percent),
            gif_export_fps: w.gif_export_fps.unwrap_or(dflt.gif_export_fps),
            video_export_width: w.video_export_width,
            video_export_height: w.video_export_height,
        })
    }
}

impl Serialize for CompositionSettings {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire<'a> {
            canvas_width: f64,
            #[serde(skip_serializing_if = "Option::is_none")]
            fps: Option<f64>,
            canvas_height: f64,
            subtitle_left_margin: f64,
            subtitle_right_margin: f64,
            subtitle_vertical_margin: f64,
            subtitle_font_size: f64,
            subtitle_font_family: SubtitleFontFamily,
            subtitle_bold: bool,
            subtitle_italic: bool,
            subtitle_color_hex: &'a str,
            background_color_hex: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            background_gradient: &'a Option<BackgroundGradient>,
            #[serde(skip_serializing_if = "Option::is_none")]
            palette: &'a Option<Vec<PaletteColor>>,
            subtitle_background_color_hex: &'a str,
            subtitle_background_opacity: f64,
            subtitle_background_padding: f64,
            subtitle_stroke_color_hex: &'a str,
            subtitle_stroke_width: f64,
            subtitle_shadow_color_hex: &'a str,
            subtitle_shadow_opacity: f64,
            subtitle_shadow_radius: f64,
            #[serde(skip_serializing_if = "Option::is_none")]
            subtitle_shadow_offset: &'a Option<[f64; 2]>,
            #[serde(skip_serializing_if = "Option::is_none")]
            subtitle_reveal: &'a Option<TextReveal>,
            subtitle_background_corner_radius: f64,
            subtitle_alignment: SubtitleTextAlignment,
            video_border_color_hex: &'a str,
            video_border_width: f64,
            video_corner_radius: f64,
            video_shadow_color_hex: &'a str,
            video_shadow_opacity: f64,
            video_shadow_radius: f64,
            #[serde(skip_serializing_if = "Option::is_none")]
            video_shadow_offset: &'a Option<[f64; 2]>,
            video_keyframes: &'a [VideoKeyframe],
            background_keyframes: &'a [BackgroundKeyframe],
            image_export_scale_percent: f64,
            gif_export_scale_percent: f64,
            #[serde(rename = "gifExportFPS")]
            gif_export_fps: f64,
            #[serde(skip_serializing_if = "is_none")]
            video_export_width: &'a Option<f64>,
            #[serde(skip_serializing_if = "is_none")]
            video_export_height: &'a Option<f64>,
        }
        Wire {
            canvas_width: self.canvas_width,
            fps: self.fps,
            canvas_height: self.canvas_height,
            subtitle_left_margin: self.subtitle_left_margin,
            subtitle_right_margin: self.subtitle_right_margin,
            subtitle_vertical_margin: self.subtitle_vertical_margin,
            subtitle_font_size: self.subtitle_font_size,
            subtitle_font_family: self.subtitle_font_family,
            subtitle_bold: self.subtitle_bold,
            subtitle_italic: self.subtitle_italic,
            subtitle_color_hex: &self.subtitle_color_hex,
            background_color_hex: &self.background_color_hex,
            background_gradient: &self.background_gradient,
            palette: &self.palette,
            subtitle_background_color_hex: &self.subtitle_background_color_hex,
            subtitle_background_opacity: self.subtitle_background_opacity,
            subtitle_background_padding: self.subtitle_background_padding,
            subtitle_stroke_color_hex: &self.subtitle_stroke_color_hex,
            subtitle_stroke_width: self.subtitle_stroke_width,
            subtitle_shadow_color_hex: &self.subtitle_shadow_color_hex,
            subtitle_shadow_opacity: self.subtitle_shadow_opacity,
            subtitle_shadow_radius: self.subtitle_shadow_radius,
            subtitle_shadow_offset: &self.subtitle_shadow_offset,
            subtitle_reveal: &self.subtitle_reveal,
            subtitle_alignment: self.subtitle_alignment,
            subtitle_background_corner_radius: self.subtitle_background_corner_radius,
            video_border_color_hex: &self.video_border_color_hex,
            video_border_width: self.video_border_width,
            video_corner_radius: self.video_corner_radius,
            video_shadow_color_hex: &self.video_shadow_color_hex,
            video_shadow_opacity: self.video_shadow_opacity,
            video_shadow_radius: self.video_shadow_radius,
            video_shadow_offset: &self.video_shadow_offset,
            video_keyframes: &self.video_keyframes,
            background_keyframes: &self.background_keyframes,
            image_export_scale_percent: self.image_export_scale_percent,
            gif_export_scale_percent: self.gif_export_scale_percent,
            gif_export_fps: self.gif_export_fps,
            video_export_width: &self.video_export_width,
            video_export_height: &self.video_export_height,
        }
        .serialize(s)
    }
}

// ---------------------------------------------------------------------------
// Layers

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectLayerKeyframe {
    pub id: String,
    pub time: f64,
    #[serde(default, skip_serializing_if = "is_none")]
    pub zoom: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub vertical_shift: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub horizontal_shift: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub color_hex: Option<String>,
    /// What the layer shows from this keyframe on — a SWAP, not a ramp.
    ///
    /// The layer's own `resource_id` is the value before the first of these,
    /// so a project with no swaps is untouched. Deliberately a step: there is
    /// no halfway between two images, and dissolving needs both drawn at
    /// once, which one layer cannot do. Honoured on image, caption and
    /// drawing layers — for video and audio a mid-layer swap would silently
    /// turn the layer into a playlist and force an answer to "where does
    /// the second clip start", which is a sequence model rather than a
    /// keyframe.
    /// Spelled `resourceID` on the wire, matching the layer's own field —
    /// camelCase would give `resourceId`, which is not what the format says.
    #[serde(default, skip_serializing_if = "is_none", rename = "resourceID")]
    pub resource_id: Option<String>,
    /// How the swap above happens. Absent, it is a cut — which is what every
    /// swap was before this field existed.
    ///
    /// This is the transition BETWEEN two pieces of material, as opposed to
    /// `transitionIn`/`transitionOut`, which are how the whole layer arrives
    /// and leaves. For its duration the layer draws both resources: the
    /// outgoing one whole, and the incoming one wiping, sliding or fading in
    /// over it. Distinct from `transitionDuration`, which is the ramp between
    /// two keyframes' VALUES and has nothing to do with material.
    #[serde(default, skip_serializing_if = "is_none")]
    pub transition: Option<LayerTransition>,
    /// A background layer's gradient at this keyframe. Animating it is how a
    /// gradient scrolls: shift `start` and `end` along the axis with a
    /// repeating or mirrored ramp and the pattern travels without an edge
    /// ever showing.
    #[serde(default, skip_serializing_if = "is_none")]
    pub gradient: Option<BackgroundGradient>,
    /// Swift `Float` — absolute playback volume 0…1 (legacy name `gain`).
    #[serde(default, skip_serializing_if = "is_none")]
    pub gain: Option<f32>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub rotation: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub tilt_x: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub tilt_y: Option<f64>,
    /// 0–1, 1 when unkeyed. Added 2026-08-14 so a composition can express a
    /// fade; the compositor has always had per-quad opacity, only the
    /// keyframe was missing.
    #[serde(default, skip_serializing_if = "is_none")]
    pub opacity: Option<f64>,
    /// The layer's shutter at this keyframe, for a blur that RAMPS — held
    /// then eased into like every other scalar track. The full form of
    /// `layer.motionBlur`, which is the shorthand; when any keyframe
    /// carries a shutter, the keyframes win.
    #[serde(default, skip_serializing_if = "is_none")]
    pub shutter: Option<f64>,
    /// The grade's scalar tracks, for adjustments that RAMP — fade to
    /// grey, warm as the sun sets. Same both-present rule as the shutter:
    /// a keyframed field beats the layer constant of the same name.
    #[serde(default, skip_serializing_if = "is_none")]
    pub saturation: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub contrast: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub brightness: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub tint_amount: Option<f64>,
    /// The mask's own placement — the WINDOW flies while the footage holds
    /// still, the inverse of `viewport`. Offsets in canvas px, zoom a
    /// uniform factor about the rect's centre (1 = as placed), rotation
    /// clockwise degrees; each rides the eased scalar clock like `shutter`.
    /// Absent means the mask sits where the rect put it, which is what
    /// every masked layer did before these existed.
    #[serde(default, skip_serializing_if = "is_none")]
    pub mask_offset_x: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub mask_offset_y: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub mask_zoom: Option<f64>,
    /// The window's VERTICAL scale, when it should differ from `mask_zoom`.
    /// Absent means "same as the horizontal" — a mask keeps its own shape
    /// unless something deliberately stretches it, which is why a circle
    /// stays a circle on a layer of any proportion.
    #[serde(default, skip_serializing_if = "is_none")]
    pub mask_zoom_y: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub mask_rotation: Option<f64>,
    pub transition_duration: f64,
    /// The share of the gap from the PREVIOUS keyframe spent moving, as a
    /// percentage: 100 starts moving immediately and arrives exactly here,
    /// 0 holds still and is simply at this value when the keyframe lands.
    /// The same quantity as `transitionDuration` in units that survive
    /// retiming — stretch the layer or swap in a longer take and a
    /// percentage still means what it meant. Wins when both are present.
    #[serde(default, skip_serializing_if = "is_none")]
    pub transition_percent: Option<f64>,
    /// The route taken to reach this keyframe. Without one the layer moves
    /// in a straight line, which is what every project does today.
    #[serde(default, skip_serializing_if = "is_none")]
    pub motion_path: Option<MotionPath>,
    /// The shape of the ramp INTO this keyframe — absent means linear, which
    /// is what every ramp was before this existed. It applies to whatever
    /// this keyframe carries: zoom, position (including along a motion path,
    /// where it eases the SPEED through the curve), viewport, rotation,
    /// opacity, gradient. One keyframe can ease while its neighbour on
    /// another track does not, since each track's ramp is its own.
    #[serde(default, skip_serializing_if = "is_none")]
    pub easing: Option<Easing>,
    /// Which part of the source this layer shows — `[x, y, w, h]` in UNIT
    /// source coordinates (0…1), absent meaning the whole frame. Unit rather
    /// than pixels so the window survives a re-recorded source at another
    /// resolution, or a resource swap. Unlike a swap it RAMPS: the four
    /// numbers interpolate on the ordinary hold-then-ramp rule, and that
    /// ramp IS the zoom-and-pan. The layer's own rect on the canvas is
    /// untouched — a framed video window keeps its place, corner radius and
    /// border while its content travels. Image and video layers only;
    /// `promo-timeline`'s `layer_viewport` owns clamping and the rule.
    #[serde(default, skip_serializing_if = "is_none")]
    pub viewport: Option<[f64; 4]>,
    /// Where and how big the drawn box is, as a rule resolved at every read
    /// (see `Placement`). Wins over raw `zoom`/shifts on the same keyframe.
    /// Reader-gated: an older save would drop the field and bake nothing in
    /// its place. Image and video layers (sprites included).
    #[serde(default, skip_serializing_if = "is_none")]
    pub placement: Option<Placement>,
}
/// What a layer does once its local time runs past the end of its source.
///
/// One question rather than a family of features: looping is not a property of
/// a file, it is what this layer does when it runs out of material. Putting it
/// on the layer means the same recording can loop under one layer and freeze
/// under another.
///
/// `PingPong` is deliberately absent. It is expressible — reverse the cut once
/// with ffmpeg (about 0.3s for a few seconds of 1440x900) and play the reversed
/// copy forward — but as a cached derived artifact, not as a playback mode:
/// decoding an inter-frame codec backwards costs a seek per frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BeyondEnd {
    /// Freeze on the last frame. The default, and what happens today.
    Hold,
    /// Start over. Replaces the resource-level `looped` flag for layers that
    /// set it.
    Loop,
    /// Stop drawing. Useful when a layer is sized by something else — an
    /// attachment, say — and should simply not be there once its material is
    /// spent, rather than sitting on a frozen still.
    Hide,
}

/// A motion path: two anchors and the curve between them.
///
/// Deliberately the smallest thing that can be a route — a start, a finish,
/// and up to two control points. No colour, no width, no fill: a path is a
/// tool, not artwork, and every property that would only matter if it were
/// drawn is one more thing to explain. One path per resource, so there is
/// never a question of WHICH stroke a layer follows; drawing again replaces
/// what was there.
///
/// With no controls it is a straight line (the same route a layer takes with
/// no path at all, which makes "add a path" a safe, visible first step). One
/// control is a quadratic curve, two a cubic — the pen-tool shape everyone
/// already knows from vector editors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathDocument {
    pub start: Point,
    pub end: Point,
    /// 0, 1 or 2 control points. Anything beyond the first two is ignored
    /// rather than refused — a future multi-segment path should not make
    /// today's files unreadable.
    #[serde(default)]
    pub controls: Vec<Point>,
}

impl CompositionSettings {
    /// Turns `@name` into the colour it names; anything else passes through.
    ///
    /// The ONE place a reference becomes a value, so every colour in the
    /// document gains palette support at once and none of them can disagree
    /// about what `@accent` means. An unknown name passes through unchanged
    /// and therefore fails to parse as hex, landing on whatever fallback the
    /// site already had — `promo_validate` is what names it, rather than the
    /// renderer guessing a colour nobody chose.
    pub fn resolve_color<'a>(&'a self, value: &'a str) -> &'a str {
        let Some(name) = value.strip_prefix('@') else {
            return value;
        };
        self.palette
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(name))
            .map(|entry| entry.color_hex.as_str())
            .unwrap_or(value)
    }
}

/// The shape of a background gradient.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GradientKind {
    /// Colours run along the line from `start` to `end`.
    Linear,
    /// Colours run outward from `start`, reaching the last stop at the
    /// distance of `end`.
    Radial,
}

/// What happens outside the 0…1 span of the gradient.
///
/// `Repeat` and `Mirror` are what make a gradient SCROLL: with either of
/// them, animating `start` and `end` along the axis by exactly one period
/// moves the pattern without the ends ever coming into view. Under `Clamp`
/// the same animation just drags two flat regions across the canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GradientRepeat {
    /// The end colours extend outward forever. The default.
    Clamp,
    /// The ramp tiles. Seamless only if the first and last stop match.
    Repeat,
    /// The ramp tiles, reversing each time, so it is seamless whatever the
    /// end colours are — the cheap way to a scroll that cannot band.
    Mirror,
}

/// One colour along a gradient.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GradientStop {
    pub color_hex: String,
    /// Where it sits, 0…1 along the gradient.
    pub at: f64,
}

/// A multi-stop background gradient.
///
/// `start` and `end` are in UNIT canvas coordinates — (0,0) is the top-left
/// corner and (1,1) the bottom-right — so a gradient survives a change of
/// canvas size, and an App Store set rendered at several sizes keeps one
/// look. They also make a scroll expressible as a number rather than a pixel
/// count: shifting `start` and `end` by 1.0 along the axis is exactly one
/// period.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundGradient {
    pub kind: GradientKind,
    /// Ordered by position. Two or more; a gradient of one colour is a fill,
    /// and should be written as `backgroundColorHex` instead.
    pub stops: Vec<GradientStop>,
    /// Linear: where the ramp begins. Radial: the centre.
    pub start: Point,
    /// Linear: where it ends. Radial: a point at the outer edge, so the
    /// distance between the two is the radius.
    pub end: Point,
    #[serde(default, skip_serializing_if = "is_none")]
    pub repeat: Option<GradientRepeat>,
}

impl BackgroundGradient {
    /// The most stops the renderer carries. Eight is far past what a
    /// background wants and keeps the per-frame uniform a fixed size.
    pub const MAX_STOPS: usize = 8;

    pub fn effective_repeat(&self) -> GradientRepeat {
        self.repeat.unwrap_or(GradientRepeat::Clamp)
    }

    /// Stops as the renderer needs them: sorted, clamped into 0…1, capped,
    /// and never fewer than two — a malformed gradient still has to draw
    /// something rather than divide by an empty range.
    pub fn resolved_stops(&self) -> Vec<GradientStop> {
        let mut stops: Vec<GradientStop> = self
            .stops
            .iter()
            .take(Self::MAX_STOPS)
            .map(|stop| GradientStop {
                color_hex: stop.color_hex.clone(),
                at: stop.at.clamp(0.0, 1.0),
            })
            .collect();
        stops.sort_by(|a, b| a.at.partial_cmp(&b.at).unwrap_or(std::cmp::Ordering::Equal));
        match stops.len() {
            0 => vec![
                GradientStop {
                    color_hex: "000000".into(),
                    at: 0.0,
                },
                GradientStop {
                    color_hex: "000000".into(),
                    at: 1.0,
                },
            ],
            1 => {
                let only = stops[0].clone();
                vec![
                    GradientStop {
                        color_hex: only.color_hex.clone(),
                        at: 0.0,
                    },
                    GradientStop {
                        color_hex: only.color_hex,
                        at: 1.0,
                    },
                ]
            }
            _ => stops,
        }
    }

    /// Whether two gradients can be interpolated into one another, rather
    /// than cut between. Shape has to match: a three-stop linear cannot
    /// become a five-stop radial halfway.
    pub fn is_compatible_with(&self, other: &BackgroundGradient) -> bool {
        self.kind == other.kind
            && self.effective_repeat() == other.effective_repeat()
            && self.resolved_stops().len() == other.resolved_stops().len()
    }
}

/// How a resource's pixels are sampled when the renderer scales them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResourceSampling {
    /// Bilinear. The default everywhere, and what a photograph wants.
    Smooth,
    /// Nearest-neighbour: a pixel stays a pixel however far it is scaled up.
    /// Pixel art needs this to look like itself, and a sprite sheet needs it
    /// so a cell's edge does not blend in the frame beside it.
    Nearest,
}

/// An image read as a GRID OF FRAMES rather than one picture.
///
/// A sprite sheet is not a new kind of media — it is an image plus the
/// arithmetic for which part of it to show. That is deliberate: the layer
/// path, the inventory rules and the still-image cache all keep working
/// unchanged, and a sprite layer moves, zooms, rotates and follows a motion
/// path exactly as any image layer does, because the frame is chosen when
/// SAMPLING and the movement happens in the geometry. The two never meet.
///
/// Frames run left to right, top to bottom — the order every exporter writes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpriteSheet {
    pub columns: u32,
    pub rows: u32,
    /// Frames actually used, for a sheet whose last row is short. `None`
    /// means the whole grid.
    #[serde(default, skip_serializing_if = "is_none")]
    pub frame_count: Option<u32>,
    /// Frames per second. `None` plays at 12, the rate hand-drawn animation
    /// has used for a century and a sane default for a sprite.
    #[serde(default, skip_serializing_if = "is_none")]
    pub fps: Option<f64>,
    /// Per-frame durations in seconds, when the source did not use a uniform
    /// rate — a GIF holding one frame for a beat says so here rather than
    /// having the hold averaged away. Length must match the frame count to
    /// be used; anything else falls back to `fps`.
    #[serde(default, skip_serializing_if = "is_none")]
    pub frame_durations: Option<Vec<f64>>,
}

impl SpriteSheet {
    pub const DEFAULT_FPS: f64 = 12.0;

    /// Frames in the sheet: the declared count, clamped to what the grid can
    /// actually hold, and never zero — a malformed sheet still has to render
    /// something rather than divide by zero.
    pub fn frames(&self) -> u32 {
        let capacity = self.columns.max(1).saturating_mul(self.rows.max(1));
        self.frame_count.unwrap_or(capacity).clamp(1, capacity)
    }

    /// Seconds for one full cycle.
    pub fn cycle_duration(&self) -> f64 {
        match self.durations() {
            Some(durations) => durations.iter().sum(),
            None => self.frames() as f64 / self.effective_fps(),
        }
    }

    pub fn effective_fps(&self) -> f64 {
        match self.fps {
            Some(fps) if fps.is_finite() && fps > 0.0 => fps,
            _ => Self::DEFAULT_FPS,
        }
    }

    /// Per-frame durations, only when they are usable: right length, all
    /// finite and positive, summing to something. A half-written array is
    /// worse than none, so it is ignored rather than partially honoured.
    fn durations(&self) -> Option<&[f64]> {
        let durations = self.frame_durations.as_deref()?;
        if durations.len() != self.frames() as usize {
            return None;
        }
        if durations.iter().any(|d| !d.is_finite() || *d <= 0.0) {
            return None;
        }
        Some(durations)
    }

    /// The frame showing at `elapsed` seconds into the animation, looping.
    /// Negative time reads as the first frame rather than wrapping backwards.
    pub fn frame_at(&self, elapsed: f64) -> u32 {
        let frames = self.frames();
        if frames <= 1 || !elapsed.is_finite() || elapsed <= 0.0 {
            return 0;
        }
        match self.durations() {
            Some(durations) => {
                let cycle: f64 = durations.iter().sum();
                let mut remaining = elapsed % cycle;
                for (index, duration) in durations.iter().enumerate() {
                    if remaining < *duration {
                        return index as u32;
                    }
                    remaining -= duration;
                }
                frames - 1
            }
            None => {
                let index = (elapsed * self.effective_fps()).floor();
                // The modulo is taken on the FRAME index, not the time: at
                // 12fps a layer 3600s in is frame 43200, which stays exact in
                // f64 where `elapsed % cycle` would have drifted by then.
                let wrapped = index.rem_euclid(frames as f64);
                wrapped as u32
            }
        }
    }

    /// The part of the sheet frame `index` occupies, as `[u, v, w, h]` in
    /// 0…1 — what the compositor's `uv_rect` takes.
    pub fn uv_rect(&self, index: u32) -> [f64; 4] {
        let columns = self.columns.max(1);
        let rows = self.rows.max(1);
        let index = index.min(self.frames().saturating_sub(1));
        let (w, h) = (1.0 / columns as f64, 1.0 / rows as f64);
        let column = index % columns;
        let row = (index / columns).min(rows - 1);
        [column as f64 * w, row as f64 * h, w, h]
    }
}

/// A path a layer follows between two keyframes — its SHAPE, not its place.
///
/// The endpoints stay whatever the keyframes already say: the path's start is
/// fitted onto the previous keyframe's position and its end onto this one's,
/// which absorbs the path's own scale, rotation and place on the canvas. So
/// one curve is a swoop for any pair of keyframes, at any distance or angle,
/// and removing it leaves a straight line — exactly today's behaviour.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MotionPath {
    /// The `path` resource to follow.
    #[serde(rename = "pathResourceID")]
    pub path_resource_id: String,
    /// Mirror the curve across the straight line between the keyframes, for
    /// when the arc bulges the wrong way and redrawing it is a chore.
    #[serde(default, skip_serializing_if = "is_none")]
    pub flipped: Option<bool>,
    /// Which stretch of the path to use, as fractions of its length. Absent
    /// is the whole of it (0 → 1).
    ///
    /// Two numbers instead of a pile of flags: `startAt` above `endAt` runs
    /// it backwards, a partial range trims a tail, and a closed path used
    /// from 0 to 0.5 is a half-circle arc rather than a degenerate loop. In
    /// the editor they are the two handles you drag along the drawn curve.
    #[serde(default, skip_serializing_if = "is_none")]
    pub start_at: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub end_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectLayer {
    pub id: String,
    pub name: String,
    pub sort_index: i64,
    pub kind: ProjectLayerKind,
    pub is_enabled: bool,
    pub start_time: f64,
    #[serde(default, skip_serializing_if = "is_none")]
    pub duration: Option<f64>,
    /// Seconds of ramp from transparent at the layer's start, and to
    /// transparent at its end. Four opacity keyframes per fading layer was
    /// about 40% of every hand-written project, and they all said the same
    /// thing.
    ///
    /// An ENVELOPE: it multiplies whatever the opacity keyframes resolve to,
    /// so the two compose instead of one silently winning. A fade-out needs a
    /// known end, so it does nothing on a layer with no `duration` — that
    /// layer runs to the end of the project, which the layer itself cannot
    /// see.
    #[serde(default, skip_serializing_if = "is_none")]
    pub fade_in: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub fade_out: Option<f64>,
    /// How the layer enters and leaves, when a plain fade is not it.
    ///
    /// `fadeIn: 0.3` and `transitionIn: {"kind": "fade", "duration": 0.3}`
    /// say the same thing; the shorthand exists because that is what most
    /// layers want and it is one number instead of an object. When a layer
    /// carries both, this one wins and `promo validate` says so, rather than
    /// two fields quietly fighting.
    #[serde(default, skip_serializing_if = "is_none")]
    pub transition_in: Option<LayerTransition>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub transition_out: Option<LayerTransition>,
    /// This layer's own camera shutter. Absent means sharp — nothing
    /// inherits it, and a layer added tomorrow stays sharp until someone
    /// asks. Per LAYER by design: a composite never shared one exposure,
    /// and a caption that smears over sharp footage is the usual mistake,
    /// not the usual intent.
    #[serde(default, skip_serializing_if = "is_none")]
    pub motion_blur: Option<MotionBlur>,
    /// This layer's own colour grade — its pixels and nobody else's.
    /// Absent means untouched. The constants are the shorthand; the
    /// keyframe fields of the same names are the full form, and win
    /// per field when present.
    #[serde(default, skip_serializing_if = "is_none")]
    pub adjustments: Option<LayerAdjustments>,
    /// How this layer's pixels COMBINE with what is beneath them. Absent
    /// means source-over. A static attribute, deliberately not keyframed:
    /// there is nothing to interpolate between two blend functions —
    /// animate the layer's opacity or its grade instead.
    #[serde(default, skip_serializing_if = "is_none")]
    pub blend_mode: Option<BlendMode>,
    /// The drawing whose ink is this layer's WINDOW: rasterized and
    /// stretched over the layer's rect, the layer only shows where the
    /// drawing has ink. Absent means the whole rect, as always. A static
    /// attribute (like `blend_mode`), and deliberately split from the
    /// keyframe `viewport`: the mask holds the window still on the canvas
    /// while the viewport pans and zooms the content behind it.
    #[serde(default, skip_serializing_if = "is_none", rename = "maskResourceID")]
    pub mask_resource_id: Option<String>,
    /// Flips the mask: the ink becomes the hole instead of the window.
    #[serde(default, skip_serializing_if = "is_none")]
    pub mask_inverted: Option<bool>,
    #[serde(default, skip_serializing_if = "is_none", rename = "resourceID")]
    pub resource_id: Option<String>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub image_filename: Option<String>,
    #[serde(default, skip_serializing_if = "is_none", rename = "imageCutID")]
    pub image_cut_id: Option<String>,
    /// Which named sub-range of the resource this layer plays. `None` plays
    /// the resource's own trim, which is what every existing project does.
    #[serde(default, skip_serializing_if = "is_none", rename = "mediaCutID")]
    pub media_cut_id: Option<String>,
    /// What happens once the layer outlives its source material. `None` holds
    /// the last frame, which is what every existing project does.
    #[serde(default, skip_serializing_if = "is_none")]
    pub beyond_end: Option<BeyondEnd>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub image_orientation: Option<ImageOrientation>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub image_border_color_hex: Option<String>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub image_border_width: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub caption_text: Option<String>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub caption_style: Option<SubtitleStyle>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub caption_voice_clip: Option<SubtitleVoiceClip>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub audio_focus: Option<bool>,
    /// Duration derived by rule instead of stated outright. Like `timing`,
    /// the stored `duration` stays the resolved answer (and the floor, for
    /// `fitDependents`).
    #[serde(default, skip_serializing_if = "is_none", rename = "durationRule")]
    pub duration_rule: Option<DurationRule>,
    /// Timing derived from a neighbouring layer instead of stated outright.
    /// Authorable — documented in `schema.md`, guarded by `schema_doc_tests`.
    ///
    /// `start_time` and `duration` remain the resolved answer — every renderer
    /// keeps reading plain numbers, and this is only the rule that produced
    /// them. That split is deliberate: two renderers interpreting one spec
    /// differently is exactly how caption placement diverged.
    #[serde(default, skip_serializing_if = "is_none")]
    pub timing: Option<LayerTiming>,
    #[serde(default)]
    pub keyframes: Vec<ProjectLayerKeyframe>,
    /// Unknown keys, preserved — see `ProjectMetadata::extra`.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Where a layer's start and end come from, when they come from its
/// neighbour.
///
/// Anchors point at an ADJACENT layer — the next-lower or next-higher
/// `sortIndex`, whatever kind it is. Reaching both ways matters because
/// z-order is fixed by what has to draw on top: a caption sits above its
/// clip, and without a forward anchor the relationship between them could
/// only be written from one side.
///
/// Only neighbours, though, and that is what keeps it cheap. Every dependency
/// joins adjacent layers, so a connected group of them is always a CONTIGUOUS
/// run — a UI can treat one as a group without storing a group. Cycles become
/// possible (two neighbours each waiting on the other) and are found while
/// resolving rather than prevented by the shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LayerTiming {
    /// Absent means the layer's own `start_time` stands.
    #[serde(default, skip_serializing_if = "is_none")]
    pub start: Option<TimingAnchor>,
    /// Absent means the layer's own `duration` stands. When present, duration
    /// is derived and whatever is stored is only a cache.
    #[serde(default, skip_serializing_if = "is_none")]
    pub end: Option<TimingAnchor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimingAnchor {
    pub from: TimingReference,
    #[serde(default)]
    pub offset: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TimingReference {
    PreviousStart,
    PreviousEnd,
    NextStart,
    NextEnd,
    /// The nearest layer of the SAME KIND in that direction — what lets a
    /// slide chain to the previous slide across the narration between them,
    /// so no slide's start ever depends on how long a sound turned out.
    PreviousPeerStart,
    PreviousPeerEnd,
    NextPeerStart,
    NextPeerEnd,
}

/// How a layer's DURATION is derived, when it is a rule rather than a number.
///
/// The time-twin of `placement`: stored, never baked, resolved on every open.
/// `start_time`/`duration` remain the resolved answer — every renderer keeps
/// reading plain numbers, exactly as with anchors.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DurationRule {
    pub kind: DurationRuleKind,
    /// Seconds of air after whatever the rule fits. Absent means none.
    #[serde(default, skip_serializing_if = "is_none")]
    pub tail: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DurationRuleKind {
    /// Duration is the RESOURCE's content length — an audio layer that plays
    /// its file out, however long the file turns out to be. Falls back to
    /// the stored duration while the content's length is unknown (a speech
    /// draft not yet synthesized).
    FitContent,
    /// Duration is AT LEAST the stored number, extended so every layer whose
    /// START is anchored to this one finishes inside it, plus `tail`. The
    /// stored duration is the floor — "the slide stays N seconds or waits
    /// for its narration", as a rule instead of a policy.
    FitDependents,
}

impl ProjectLayer {
    pub fn is_audio_focused(&self) -> bool {
        self.audio_focus.unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Resources

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoTrimKeyframe {
    pub id: String,
    pub time: f64,
    pub is_included: bool,
    #[serde(default, skip_serializing_if = "is_none")]
    pub extended_pause_duration: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VideoTrimRange {
    pub start: f64,
    pub end: f64,
}

impl VideoTrimRange {
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }
}
/// A named sub-range of a video or audio resource.
///
/// The same idea as an image cut, in time rather than space: one recording
/// becomes several usable pieces without copying a file. A cut carries its own
/// trim — including the keyframed include/exclude ranges — so "the part where
/// the formula autocompletes" is a thing a layer can point at.
///
/// A cut's fields shadow the resource's. Nothing else changes: a layer that
/// names one is mapped through exactly the same code as a layer that does
/// not, because the cut is resolved into a resource before mapping sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaCut {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "is_none")]
    pub trim_start: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub trim_end: Option<f64>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub trim_keyframes: Option<Vec<VideoTrimKeyframe>>,
    /// Playback rate. 1.5 plays half again as fast, so the cut occupies
    /// two thirds of the timeline it otherwise would. `None` is 1.0.
    ///
    /// Audio is stretched with pitch preserved, so a narration line can be
    /// nudged to fit its beat without the voice changing — which beats
    /// re-synthesizing it, since TTS returns a different duration each time
    /// it is asked.
    #[serde(default, skip_serializing_if = "is_none")]
    pub speed: Option<f64>,
    /// Unknown keys, preserved — see `ProjectMetadata::extra`.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Mirrors `ProjectImageCut` custom decode: `filename` defaults to "", the
/// rect is re-normalized into the unit square.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectImageCut {
    pub id: String,
    pub rect: Rect,
    pub filename: String,
    pub created_at: f64,
    #[serde(skip_serializing_if = "is_none")]
    pub frame: Option<ResourceFrame>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectImageCutWire {
    id: String,
    rect: Rect,
    #[serde(default)]
    filename: Option<String>,
    created_at: f64,
    #[serde(default)]
    frame: Option<ResourceFrame>,
}

impl<'de> Deserialize<'de> for ProjectImageCut {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = ProjectImageCutWire::deserialize(d)?;
        Ok(ProjectImageCut {
            id: w.id,
            rect: w.rect.normalized_unit_rect(),
            filename: w.filename.unwrap_or_default(),
            created_at: w.created_at,
            frame: w.frame,
        })
    }
}

/// Mirrors `ResourceFrame`: every field decodes tolerantly to its default and
/// every field is always encoded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ResourceFrame {
    pub kind: ResourceFrameKind,
    pub border_color_hex: String,
    pub border_width: f64,
    pub corner_radius: f64,
    pub material: FrameMaterial,
    pub tilt_y: f64,
    pub tilt_x: f64,
    pub bezel_fraction: f64,
    pub depth_fraction: f64,
    /// Per-resource drop shadow, each field falling back to the
    /// composition's `video_shadow_*` default when `None` — the caption
    /// per-field-fallback rule. Radius and offset here are authored against
    /// the same 1080-wide reference as `border_width`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadow_color_hex: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadow_opacity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadow_radius: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadow_offset: Option<[f64; 2]>,
}

impl Default for ResourceFrame {
    fn default() -> Self {
        ResourceFrame {
            kind: ResourceFrameKind::None,
            border_color_hex: "FFFFFF".into(),
            border_width: 12.0,
            corner_radius: 0.0,
            material: FrameMaterial::SpaceBlack,
            tilt_y: 0.0,
            tilt_x: 0.0,
            bezel_fraction: 0.03,
            depth_fraction: 0.06,
            shadow_color_hex: None,
            shadow_opacity: None,
            shadow_radius: None,
            shadow_offset: None,
        }
    }
}

// Drawings ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DrawingShape {
    pub id: String,
    pub kind: DrawingShapeKind,
    pub points: Vec<Point>,
    pub stroke_color_hex: String,
    pub stroke_width: f64,
    /// Swift `Float?`.
    #[serde(default, skip_serializing_if = "is_none")]
    pub stroke_opacity: Option<f32>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub fill_color_hex: Option<String>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub fill_opacity: Option<f32>,
    pub arrow_start: bool,
    pub arrow_end: bool,
    #[serde(default, skip_serializing_if = "is_none")]
    pub even_odd_fill: Option<bool>,
    #[serde(default, skip_serializing_if = "is_none", rename = "groupID")]
    pub group_id: Option<String>,
}

/// Mirrors `DrawingDocument`: legacy `canvasSize` is ignored on decode and
/// never re-encoded.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DrawingDocument {
    pub shapes: Vec<DrawingShape>,
    #[serde(skip_serializing_if = "is_none")]
    pub background_color_hex: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DrawingDocumentWire {
    #[serde(default)]
    shapes: Option<Vec<DrawingShape>>,
    #[serde(default)]
    background_color_hex: Option<String>,
}

impl<'de> Deserialize<'de> for DrawingDocument {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = DrawingDocumentWire::deserialize(d)?;
        Ok(DrawingDocument {
            shapes: w.shapes.unwrap_or_default(),
            background_color_hex: w.background_color_hex,
        })
    }
}

/// Mirrors `ProjectResource` including its decode-time migrations: legacy
/// What a BACKGROUND resource paints: a plain colour, a gradient (wins
/// over the colour), and optionally an image drawn per `fill`. A
/// background is scenery, not media — it never takes borders, corner
/// radius or shadows. Rung 16 rides on the KIND, not these fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BackgroundStyle {
    pub fill: BackgroundFill,
    #[serde(skip_serializing_if = "is_none")]
    pub color_hex: Option<String>,
    #[serde(skip_serializing_if = "is_none")]
    pub gradient: Option<BackgroundGradient>,
    /// The tiling's starting point in UNIT canvas coordinates (the
    /// gradient precedent). The background layer's shift keyframes MOVE it
    /// — the eased position track scrolls the pattern.
    #[serde(skip_serializing_if = "is_none")]
    pub anchor: Option<[f64; 2]>,
}

impl Default for BackgroundStyle {
    fn default() -> Self {
        BackgroundStyle {
            fill: BackgroundFill::Stretch,
            color_hex: None,
            gradient: None,
            anchor: None,
        }
    }
}

tolerant_enum!(
    BackgroundFill,
    Stretch,
    [(Stretch, "stretch"), (Fit, "fit"), (Tile, "tile"),]
);

impl Default for BackgroundFill {
    fn default() -> Self {
        BackgroundFill::Stretch
    }
}

/// `audioGain` (0…4) collapses into `volume` (0…1) when `volume` is absent,
/// and `disabledAudioTrackIndices` is deduped / filtered / sorted.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectResource {
    pub id: String,
    pub kind: ProjectResourceKind,
    pub filename: String,
    pub display_name: String,
    pub added_at: f64,
    #[serde(skip_serializing_if = "is_none")]
    pub duration: Option<f64>,
    #[serde(skip_serializing_if = "is_none")]
    pub trim_start: Option<f64>,
    #[serde(skip_serializing_if = "is_none")]
    pub trim_end: Option<f64>,
    #[serde(skip_serializing_if = "is_none")]
    pub trim_keyframes: Option<Vec<VideoTrimKeyframe>>,
    #[serde(skip_serializing_if = "is_none")]
    pub caption_text: Option<String>,
    #[serde(skip_serializing_if = "is_none")]
    pub caption_style: Option<SubtitleStyle>,
    #[serde(skip_serializing_if = "is_none")]
    pub caption_voice_clip: Option<SubtitleVoiceClip>,
    #[serde(skip_serializing_if = "is_none")]
    pub drawing: Option<DrawingDocument>,
    /// Set on `path` resources, and only on them.
    #[serde(default, skip_serializing_if = "is_none")]
    pub path: Option<PathDocument>,
    /// Present when an IMAGE is a sprite sheet: the same file, read as a grid
    /// of frames that cycle over the layer's local time.
    #[serde(default, skip_serializing_if = "is_none")]
    pub sprite: Option<SpriteSheet>,
    /// What a BACKGROUND-kind resource paints.
    #[serde(default, skip_serializing_if = "is_none")]
    pub background: Option<BackgroundStyle>,
    /// How the renderer samples this resource. `None` is smooth, which is
    /// right for photographs and screen captures and wrong for pixel art.
    #[serde(default, skip_serializing_if = "is_none")]
    pub sampling: Option<ResourceSampling>,
    pub image_cuts: Vec<ProjectImageCut>,
    /// Playback rate for the resource's own trim, as `MediaCut::speed` is for
    /// a cut. `None` is 1.0.
    #[serde(default, skip_serializing_if = "is_none")]
    pub speed: Option<f64>,
    /// Named sub-ranges of this video or audio. Empty for most resources.
    ///
    /// Always serialized, even when empty, because `imageCuts` is and the
    /// Swift↔Rust parity harness compares key sets exactly — one side quietly
    /// omitting a field is precisely what it exists to catch.
    #[serde(default)]
    pub media_cuts: Vec<MediaCut>,
    /// Swift `Float?` — legacy field, retained on the wire.
    #[serde(skip_serializing_if = "is_none")]
    pub audio_gain: Option<f32>,
    #[serde(skip_serializing_if = "is_none")]
    pub volume: Option<f32>,
    pub disabled_audio_track_indices: Vec<i64>,
    #[serde(skip_serializing_if = "is_none")]
    pub video_natural_width: Option<f64>,
    #[serde(skip_serializing_if = "is_none")]
    pub video_natural_height: Option<f64>,
    /// Pixel size of an IMAGE resource, stamped at import. Placement intents
    /// resolve width/anchoring against it (videos use `videoNatural*`, a
    /// sprite divides by its grid). Optional — a hand-written project may
    /// omit it, and placement then assumes a square source, which
    /// `promo_validate` names.
    #[serde(skip_serializing_if = "is_none")]
    pub pixel_width: Option<f64>,
    #[serde(skip_serializing_if = "is_none")]
    pub pixel_height: Option<f64>,
    #[serde(skip_serializing_if = "is_none")]
    pub frame: Option<ResourceFrame>,
    #[serde(skip_serializing_if = "is_none")]
    pub looped: Option<bool>,
    /// Unknown keys, preserved — see `ProjectMetadata::extra`.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectResourceWire {
    id: String,
    kind: ProjectResourceKind,
    filename: String,
    display_name: String,
    added_at: f64,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    trim_start: Option<f64>,
    #[serde(default)]
    trim_end: Option<f64>,
    #[serde(default)]
    trim_keyframes: Option<Vec<VideoTrimKeyframe>>,
    #[serde(default)]
    caption_text: Option<String>,
    #[serde(default)]
    caption_style: Option<SubtitleStyle>,
    #[serde(default)]
    background: Option<BackgroundStyle>,
    #[serde(default)]
    caption_voice_clip: Option<SubtitleVoiceClip>,
    #[serde(default)]
    drawing: Option<DrawingDocument>,
    #[serde(default)]
    path: Option<PathDocument>,
    #[serde(default)]
    sprite: Option<SpriteSheet>,
    #[serde(default)]
    sampling: Option<ResourceSampling>,
    #[serde(default)]
    image_cuts: Option<Vec<ProjectImageCut>>,
    #[serde(default)]
    media_cuts: Option<Vec<MediaCut>>,
    #[serde(default)]
    speed: Option<f64>,
    #[serde(default)]
    audio_gain: Option<f32>,
    #[serde(default)]
    volume: Option<f32>,
    #[serde(default)]
    disabled_audio_track_indices: Option<Vec<i64>>,
    #[serde(default)]
    video_natural_width: Option<f64>,
    #[serde(default)]
    pixel_width: Option<f64>,
    #[serde(default)]
    pixel_height: Option<f64>,
    #[serde(default)]
    video_natural_height: Option<f64>,
    #[serde(default)]
    frame: Option<ResourceFrame>,
    #[serde(default)]
    looped: Option<bool>,
    /// Unknown keys captured on decode; the conversion below carries them
    /// into the model so encode preserves them.
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

impl<'de> Deserialize<'de> for ProjectResource {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = ProjectResourceWire::deserialize(d)?;
        let volume = w.volume.or_else(|| w.audio_gain.map(|g| g.clamp(0.0, 1.0)));
        let mut indices: Vec<i64> = w
            .disabled_audio_track_indices
            .unwrap_or_default()
            .into_iter()
            .filter(|&i| i >= 0)
            .collect();
        indices.sort_unstable();
        indices.dedup();
        Ok(ProjectResource {
            id: w.id,
            kind: w.kind,
            background: w.background,
            media_cuts: w.media_cuts.unwrap_or_default(),
            speed: w.speed,
            filename: w.filename,
            display_name: w.display_name,
            added_at: w.added_at,
            duration: w.duration,
            trim_start: w.trim_start,
            trim_end: w.trim_end,
            trim_keyframes: w.trim_keyframes,
            caption_text: w.caption_text,
            caption_style: w.caption_style,
            caption_voice_clip: w.caption_voice_clip,
            drawing: w.drawing,
            path: w.path,
            sprite: w.sprite,
            sampling: w.sampling,
            image_cuts: w.image_cuts.unwrap_or_default(),
            audio_gain: w.audio_gain,
            volume,
            disabled_audio_track_indices: indices,
            video_natural_width: w.video_natural_width,
            pixel_width: w.pixel_width,
            pixel_height: w.pixel_height,
            video_natural_height: w.video_natural_height,
            frame: w.frame,
            looped: w.looped,
            extra: w.extra,
        })
    }
}

impl ProjectResource {
    pub fn is_looped(&self) -> bool {
        self.looped.unwrap_or(false)
    }

    /// Resolved playback volume in 0…1 (Swift `effectiveVolume`).
    pub fn effective_volume(&self) -> f32 {
        self.volume.unwrap_or(1.0).clamp(0.0, 1.0)
    }
}

// ---------------------------------------------------------------------------
// Palette + exports + top level

/// A colour with a name, so a project can say a thing ONCE.
///
/// Ten hand-authored templates carried 106 colour values between them, 40 of
/// them distinct, spread across gradient stops, borders, caption styles and
/// background keyframes. Re-skinning one meant finding every occurrence.
/// A palette entry is the indirection that turns that into a single edit.
///
/// Deliberately NOT a resource: resources are things a LAYER points at by id
/// and that mostly have files. A colour is referenced by fields all over the
/// document, and giving six characters of hex a UUID and a row next to the
/// videos would be noise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaletteColor {
    /// Referenced as `@name`. Matched case-insensitively, because a person
    /// typing `@Accent` after naming it `accent` means the same colour.
    pub name: String,
    pub color_hex: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectExport {
    pub id: String,
    pub kind: ProjectExportKind,
    pub created_at: f64,
    pub filenames: Vec<String>,
    /// Whether the free-tier watermark was baked into these files (Swift
    /// `ProjectExport.watermarked`; absent on records from before the field).
    #[serde(default, skip_serializing_if = "is_none")]
    pub watermarked: Option<bool>,
    /// Unknown keys, preserved — see `ProjectMetadata::extra`.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Top-level `metadata.json` (Swift `ProjectMetadata`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectMetadata {
    pub id: String,
    pub name: String,
    pub created_at: f64,
    pub subtitles: Vec<SubtitleEntry>,
    pub trim_start: f64,
    pub trim_end: f64,
    #[serde(default, skip_serializing_if = "is_none")]
    pub trim_keyframes: Option<Vec<VideoTrimKeyframe>>,
    pub video_duration: f64,
    pub composition_settings: CompositionSettings,
    #[serde(default, skip_serializing_if = "is_none")]
    pub layers: Option<Vec<ProjectLayer>>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub resources: Option<Vec<ProjectResource>>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub exports: Option<Vec<ProjectExport>>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub updated_at: Option<f64>,
    pub state: String,
    /// Format version the writer produced. Absent means 1 — the era before
    /// the field existed.
    #[serde(default, skip_serializing_if = "is_none")]
    pub format_version: Option<i64>,
    /// Oldest reader allowed to open this file for WRITING. A reader below
    /// this must refuse or open read-only: the Swift twin's Codable drops
    /// fields it does not know, so saving from an old binary silently
    /// destroys newer features — that is the data loss this gates.
    #[serde(default, skip_serializing_if = "is_none")]
    pub min_reader_version: Option<i64>,
    /// Every key this version does not model, preserved through the round
    /// trip. Tolerant-read PLUS lossy-write is how a project loses data to
    /// an older writer; with this bag the Rust side is lossless, and can be
    /// the single writer once the editor migration completes.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl ProjectResource {
    /// A resource with nothing but its kind decided — the defaults a file
    /// gets when it is adopted from `Resources/` without an entry. Fields the
    /// app fills in on demand (duration, natural size) stay `None`: absent
    /// means "not measured yet", which the render paths already handle.
    pub fn placeholder(kind: ProjectResourceKind) -> Self {
        ProjectResource {
            background: None,
            id: String::new(),
            kind,
            media_cuts: Vec::new(),
            speed: None,
            filename: String::new(),
            display_name: String::new(),
            added_at: 0.0,
            duration: None,
            trim_start: None,
            trim_end: None,
            trim_keyframes: None,
            caption_text: None,
            caption_style: None,
            caption_voice_clip: None,
            drawing: None,
            path: None,
            sprite: None,
            sampling: None,
            image_cuts: Vec::new(),
            audio_gain: None,
            volume: None,
            disabled_audio_track_indices: Vec::new(),
            video_natural_width: None,
            pixel_width: None,
            pixel_height: None,
            video_natural_height: None,
            frame: None,
            looped: None,
            extra: Default::default(),
        }
    }
}

impl ProjectMetadata {
    /// The oldest reader that may open this project FOR WRITING.
    ///
    /// Per file, not per binary: a project using none of these features is
    /// still a version-1 document, and stamping every save with the current
    /// version would lock projects out of the previous release over a feature
    /// they do not use. A version is claimed only for a change an older
    /// reader would DESTROY by saving — a field it drops on the way through —
    /// not for one it merely ignores while rendering.
    ///
    /// 9 transitions — a layer's own entry and exit, and the one at a
    /// resource swap (dropped, they all become cuts),
    /// 8 layer fades (an older reader drops `fadeIn`/`fadeOut` on save and
    /// the ramp is simply gone),
    /// 7 placement (a rule an older reader replaces with zoom 1 in a corner),
    /// 6 keyframe viewport (drops the window, un-zooming the edit),
    /// 5 keyframe resource swap (turns a sequence back into one still),
    /// 4 background gradient (falls back to the flat colour, then drops it),
    /// 3 sprite sheets (draws the whole sheet as one still, then drops the
    /// sprite fields), 2 motion paths (`path` is a resource KIND and kinds
    /// decode strictly, so an older binary cannot open the file at all).
    ///
    /// The twin of `ProjectStore.minimumReaderVersion(for:)` in the app, so a
    /// project written from the CLI carries the same stamp as one saved from
    /// the editor; `a_project_is_stamped_by_what_it_uses` walks the same
    /// ladder the Swift tests do.
    pub fn minimum_reader_version(&self) -> i64 {
        let layers = self.layers.as_deref().unwrap_or(&[]);
        let resources = self.resources.as_deref().unwrap_or(&[]);
        let any_keyframe = |pick: fn(&ProjectLayerKeyframe) -> bool| {
            layers.iter().any(|l| l.keyframes.iter().any(pick))
        };

        // 16 is a BACKGROUND resource: kinds decode strictly, so an older
        // binary must refuse the file rather than throw mid-decode.
        if resources
            .iter()
            .any(|r| r.kind == ProjectResourceKind::Background)
        {
            return 16;
        }

        // 10 is a per-layer motion blur. Dropped by an older reader's
        // save, the shot silently goes sharp — the same destruction test
        // every rung on this ladder passed.
        // 15 is a duration RULE, or a peer-reaching anchor. Dropped by an
        // older reader's save, a slide that waits for its narration stops
        // waiting and the chain built through peers collapses onto stale
        // numbers — the same destruction test as every rung.
        let peer = |anchor: &Option<TimingAnchor>| {
            anchor.as_ref().is_some_and(|a| {
                matches!(
                    a.from,
                    TimingReference::PreviousPeerStart
                        | TimingReference::PreviousPeerEnd
                        | TimingReference::NextPeerStart
                        | TimingReference::NextPeerEnd
                )
            })
        };
        if layers.iter().any(|l| {
            l.duration_rule.is_some()
                || l.timing
                    .as_ref()
                    .is_some_and(|t| peer(&t.start) || peer(&t.end))
        }) {
            return 15;
        }
        // 14 is a mask that MOVES. Dropped by an older reader's save, the
        // flying window parks in the rect's centre — the composition stops
        // showing what it showed, the same destruction test as every rung.
        if any_keyframe(|k| {
            k.mask_offset_x.is_some()
                || k.mask_offset_y.is_some()
                || k.mask_zoom.is_some()
                || k.mask_zoom_y.is_some()
                || k.mask_rotation.is_some()
        }) {
            return 14;
        }
        // 13 is a shape mask — dropped by an older reader's save, the
        // porthole vanishes and the full rectangle floods back over
        // whatever it was framing. `maskInverted` rides the same rung: it
        // shipped in the same format version, so no reader has ever
        // understood 13 without it.
        if layers
            .iter()
            .any(|l| l.mask_resource_id.is_some() || l.mask_inverted.is_some())
        {
            return 13;
        }
        // 12 is a blend mode — dropped, a screen-blended glow becomes an
        // opaque black card over the footage, which is far worse than the
        // look merely reverting.
        if layers.iter().any(|l| l.blend_mode.is_some()) {
            return 12;
        }
        // 11 is a per-layer colour grade — dropped by an older reader's
        // save, the look silently reverts.
        if layers.iter().any(|l| {
            l.adjustments.is_some()
                || l.keyframes.iter().any(|k| {
                    k.saturation.is_some()
                        || k.contrast.is_some()
                        || k.brightness.is_some()
                        || k.tint_amount.is_some()
                })
        }) {
            return 11;
        }
        // Keyframed shutters ride the same rung as the constant: both
        // landed in one unreleased batch, so no reader has ever understood
        // 10 without them.
        if layers
            .iter()
            .any(|l| l.motion_blur.is_some() || l.keyframes.iter().any(|k| k.shutter.is_some()))
        {
            return 10;
        }
        // A text reveal rides the same rung: it landed in the same
        // unreleased batch as transitions, so no reader has ever understood
        // 9 without it — and dropped, a typewriter becomes a caption that
        // was simply there.
        let has_reveal =
            |style: &Option<SubtitleStyle>| style.as_ref().is_some_and(|s| s.reveal.is_some());
        let reveals = self.composition_settings.subtitle_reveal.is_some()
            || layers.iter().any(|l| has_reveal(&l.caption_style))
            || resources.iter().any(|r| has_reveal(&r.caption_style));
        if reveals
            || layers.iter().any(|l| {
                l.transition_in.is_some()
                    || l.transition_out.is_some()
                    || l.keyframes.iter().any(|k| k.transition.is_some())
            })
        {
            return 9;
        }
        if layers
            .iter()
            .any(|l| l.fade_in.is_some() || l.fade_out.is_some())
        {
            return 8;
        }
        if any_keyframe(|k| k.placement.is_some()) {
            return 7;
        }
        if any_keyframe(|k| k.viewport.is_some()) {
            return 6;
        }
        if any_keyframe(|k| k.resource_id.is_some()) {
            return 5;
        }
        if self.composition_settings.background_gradient.is_some()
            || any_keyframe(|k| k.gradient.is_some())
        {
            return 4;
        }
        if resources
            .iter()
            .any(|r| r.sprite.is_some() || r.sampling.is_some())
        {
            return 3;
        }
        if resources
            .iter()
            .any(|r| r.kind == ProjectResourceKind::Path)
            || any_keyframe(|k| k.motion_path.is_some() || k.transition_percent.is_some())
        {
            return 2;
        }
        1
    }

    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// The caption resource with this id, if it is one.
    ///
    /// Separate from the layer so a caller that already knows WHICH resource
    /// is showing — a keyframe swap resolves that per time — can ask for it
    /// by name. Reading the layer's own id is only ever right before the
    /// first swap.
    fn caption_resource_named(&self, id: Option<&str>) -> Option<&ProjectResource> {
        let id = id?;
        self.resources
            .as_ref()?
            .iter()
            .find(|resource| resource.id == id && resource.kind == ProjectResourceKind::Caption)
    }

    /// Caption text the way the app resolves it: the layer's caption resource
    /// first, the layer's own field second. Captions authored in the app keep
    /// their text on the RESOURCE; a renderer that reads only the layer shows
    /// nothing for them — which is how the preview once came to composite a
    /// second, host-drawn copy on top of its own.
    pub fn caption_text_for(&self, layer: &ProjectLayer) -> Option<String> {
        self.caption_text_showing(layer, layer.resource_id.as_deref())
    }

    /// The text of the caption resource `showing`, falling back to the
    /// layer's own words — what `caption_text_for` does, for a caller that
    /// resolved the swap itself.
    pub fn caption_text_showing(
        &self,
        layer: &ProjectLayer,
        showing: Option<&str>,
    ) -> Option<String> {
        self.caption_resource_named(showing)
            .and_then(|resource| resource.caption_text.clone())
            .or_else(|| layer.caption_text.clone())
    }

    /// Caption style, same precedence as [`caption_text_for`](Self::caption_text_for).
    pub fn caption_style_for(&self, layer: &ProjectLayer) -> Option<SubtitleStyle> {
        self.caption_style_showing(layer, layer.resource_id.as_deref())
    }

    pub fn caption_style_showing(
        &self,
        layer: &ProjectLayer,
        showing: Option<&str>,
    ) -> Option<SubtitleStyle> {
        self.caption_resource_named(showing)
            .and_then(|resource| resource.caption_style.clone())
            .or_else(|| layer.caption_style.clone())
    }
}

#[cfg(test)]
mod forward_compat_tests {
    use super::*;

    #[test]
    fn unknown_keys_survive_the_round_trip_at_every_level() {
        // Tolerant-read PLUS lossy-write is how a project loses features to
        // an older writer. The Rust model is now lossless: what it does not
        // model, it carries.
        let json = r#"{
            "id": "T", "name": "T", "createdAt": 0, "state": "recorded",
            "subtitles": [], "trimStart": 0, "trimEnd": 0,
            "videoDuration": 0, "compositionSettings": {},
            "someFutureTopLevel": {"nested": true},
            "layers": [{
                "id": "L", "name": "L", "sortIndex": 0, "kind": "video",
                "isEnabled": true, "startTime": 0, "keyframes": [],
                "someFutureLayerField": 42
            }],
            "resources": [{
                "id": "R", "kind": "video", "filename": "v.mp4",
                "displayName": "V", "addedAt": 0,
                "someFutureResourceField": "kept",
                "mediaCuts": [{
                    "id": "C", "name": "Cut",
                    "someFutureCutField": [1, 2, 3]
                }]
            }]
        }"#;
        let meta = ProjectMetadata::from_json(json).expect("decodes");
        let out = meta.to_json().expect("encodes");
        let value: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["someFutureTopLevel"]["nested"], true);
        assert_eq!(value["layers"][0]["someFutureLayerField"], 42);
        assert_eq!(value["resources"][0]["someFutureResourceField"], "kept");
        assert_eq!(
            value["resources"][0]["mediaCuts"][0]["someFutureCutField"],
            serde_json::json!([1, 2, 3])
        );
    }

    #[test]
    fn version_fields_round_trip_and_stay_absent_when_unset() {
        let plain = r#"{
            "id": "T", "name": "T", "createdAt": 0, "state": "recorded",
            "subtitles": [], "trimStart": 0, "trimEnd": 0,
            "videoDuration": 0, "compositionSettings": {}
        }"#;
        let meta = ProjectMetadata::from_json(plain).unwrap();
        assert_eq!(meta.format_version, None);
        assert_eq!(meta.min_reader_version, None);
        let out = meta.to_json().unwrap();
        assert!(!out.contains("formatVersion"), "absent stays absent");

        let versioned = r#"{
            "id": "T", "name": "T", "createdAt": 0, "state": "recorded",
            "subtitles": [], "trimStart": 0, "trimEnd": 0,
            "videoDuration": 0, "compositionSettings": {},
            "formatVersion": 3, "minReaderVersion": 2
        }"#;
        let meta = ProjectMetadata::from_json(versioned).unwrap();
        assert_eq!(meta.format_version, Some(3));
        assert_eq!(meta.min_reader_version, Some(2));
        let out = meta.to_json().unwrap();
        assert!(out.contains("\"formatVersion\":3"));
        assert!(out.contains("\"minReaderVersion\":2"));
    }
}

#[cfg(test)]
mod caption_source_tests {
    use super::*;

    fn project(json: &str) -> ProjectMetadata {
        ProjectMetadata::from_json(json).expect("test project parses")
    }

    #[test]
    fn resource_text_wins_over_the_layer_field() {
        // A caption authored in the app keeps its words on the resource; the
        // layer may carry a stale copy from before the edit.
        let p = project(
            r#"{
                "id": "T", "name": "T", "createdAt": 0, "state": "recorded",
                "subtitles": [], "trimStart": 0, "trimEnd": 0,
                "videoDuration": 0, "compositionSettings": {},
                "resources": [{
                    "id": "R", "kind": "caption", "filename": "",
                    "displayName": "Cap", "addedAt": 0,
                    "captionText": "from the resource",
                    "captionStyle": {"fontSize": 96}
                }],
                "layers": [{
                    "id": "L", "name": "Cap", "sortIndex": 1, "kind": "caption",
                    "isEnabled": true, "startTime": 0, "keyframes": [],
                    "resourceID": "R", "captionText": "stale layer copy"
                }]
            }"#,
        );
        let layer = &p.layers.as_ref().unwrap()[0];
        assert_eq!(
            p.caption_text_for(layer).as_deref(),
            Some("from the resource")
        );
        assert_eq!(p.caption_style_for(layer).unwrap().font_size, Some(96.0));
    }

    #[test]
    fn layer_fields_stand_alone_when_there_is_no_resource() {
        // CLI-generated projects put text straight on the layer.
        let p = project(
            r#"{
                "id": "T", "name": "T", "createdAt": 0, "state": "recorded",
                "subtitles": [], "trimStart": 0, "trimEnd": 0,
                "videoDuration": 0, "compositionSettings": {},
                "layers": [{
                    "id": "L", "name": "Cap", "sortIndex": 1, "kind": "caption",
                    "isEnabled": true, "startTime": 0, "keyframes": [],
                    "captionText": "layer only"
                }]
            }"#,
        );
        let layer = &p.layers.as_ref().unwrap()[0];
        assert_eq!(p.caption_text_for(layer).as_deref(), Some("layer only"));
        assert_eq!(p.caption_style_for(layer), None);
    }

    #[test]
    fn a_non_caption_resource_does_not_lend_its_fields() {
        // A video resource that happens to share the id must not be mistaken
        // for a caption source.
        let p = project(
            r#"{
                "id": "T", "name": "T", "createdAt": 0, "state": "recorded",
                "subtitles": [], "trimStart": 0, "trimEnd": 0,
                "videoDuration": 0, "compositionSettings": {},
                "resources": [{
                    "id": "R", "kind": "video", "filename": "v.mp4",
                    "displayName": "V", "addedAt": 0,
                    "captionText": "not a caption"
                }],
                "layers": [{
                    "id": "L", "name": "Cap", "sortIndex": 1, "kind": "caption",
                    "isEnabled": true, "startTime": 0, "keyframes": [],
                    "resourceID": "R", "captionText": "the layer's own"
                }]
            }"#,
        );
        let layer = &p.layers.as_ref().unwrap()[0];
        assert_eq!(
            p.caption_text_for(layer).as_deref(),
            Some("the layer's own")
        );
    }
}

#[cfg(test)]
mod fps_tests {
    use super::*;

    #[test]
    fn fps_round_trips_and_stays_absent_when_unset() {
        // Absent means "render at 30", which is not the same as 30 written
        // down — a project that never chose a rate should not start claiming
        // one the moment it is saved.
        let plain: CompositionSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(plain.fps, None);
        assert!(!serde_json::to_string(&plain).unwrap().contains("fps"));

        // Fractional rates survive: 60000/1001 is what a screen recording
        // actually is, and rounding it to 60 would resample every frame.
        let ntsc: CompositionSettings =
            serde_json::from_str(r#"{"fps": 59.94005994005994}"#).unwrap();
        assert_eq!(ntsc.fps, Some(59.94005994005994));
        let text = serde_json::to_string(&ntsc).unwrap();
        assert!(text.contains("59.94"), "{text}");
    }
}

#[cfg(test)]
#[cfg(test)]
mod placement_model_tests {
    use super::*;

    #[test]
    fn a_placement_round_trips_and_tolerates_the_future() {
        let json = r#"{"id": "A", "time": 0, "transitionDuration": 0,
            "placement": {"height": 620, "anchor": "topRight", "offset": [-40, 12]}}"#;
        let keyframe: ProjectLayerKeyframe = serde_json::from_str(json).expect("keyframe");
        let rule = keyframe.placement.clone().expect("placement");
        assert_eq!(rule.height, Some(620.0));
        assert_eq!(rule.anchor, Some(Anchor::TopRight));
        assert_eq!(rule.offset, Some([-40.0, 12.0]));
        assert!(rule.sizes());
        let back: ProjectLayerKeyframe =
            serde_json::from_str(&serde_json::to_string(&keyframe).unwrap()).unwrap();
        assert_eq!(back.placement, keyframe.placement);

        // An anchor from a newer build reads as center rather than refusing
        // the file; the box hangs somewhere sensible and the feel is wrong,
        // not the document.
        let future: Placement =
            serde_json::from_str(r#"{"height": 10, "anchor": "goldenSpiral"}"#).unwrap();
        assert_eq!(future.anchor, Some(Anchor::Center));

        // A keyframe without one stays without one on the wire.
        let plain: ProjectLayerKeyframe =
            serde_json::from_str(r#"{"id": "B", "time": 0, "transitionDuration": 0}"#).unwrap();
        let wire = serde_json::to_string(&plain).unwrap();
        assert!(!wire.contains("placement"), "{wire}");
    }

    #[test]
    fn resource_pixel_size_round_trips() {
        let json = r#"{"id": "I", "kind": "image", "filename": "i.png",
            "displayName": "I", "addedAt": 0, "pixelWidth": 1170, "pixelHeight": 2532}"#;
        let resource: ProjectResource = serde_json::from_str(json).expect("resource");
        assert_eq!(resource.pixel_width, Some(1170.0));
        let back: ProjectResource =
            serde_json::from_str(&serde_json::to_string(&resource).unwrap()).unwrap();
        assert_eq!(back.pixel_height, Some(2532.0));
    }

    #[test]
    fn caption_stroke_and_shadow_survive_the_hand_written_wire() {
        // CompositionSettings has hand-written Serialize/Deserialize, so a
        // field added to the struct but not to all four wire sites is
        // accepted, ignored, and dropped on the next save — which is
        // exactly how `backgroundGradient` once vanished.
        let json = r#"{"canvasWidth": 1080, "canvasHeight": 1920,
            "subtitleStrokeColorHex": "0B1020", "subtitleStrokeWidth": 8,
            "subtitleShadowColorHex": "000000", "subtitleShadowOpacity": 0.5,
            "subtitleShadowRadius": 12}"#;
        let settings: CompositionSettings = serde_json::from_str(json).expect("settings");
        assert_eq!(settings.subtitle_stroke_width, 8.0);
        assert_eq!(settings.subtitle_shadow_radius, 12.0);
        let back: CompositionSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(back.subtitle_stroke_color_hex, "0B1020");
        assert_eq!(back.subtitle_stroke_width, 8.0);
        assert_eq!(back.subtitle_shadow_opacity, 0.5);
        assert_eq!(back.subtitle_shadow_radius, 12.0);

        // Per-caption overrides round-trip too, and a caption that names
        // none keeps none on the wire.
        let styled: SubtitleStyle = serde_json::from_str(
            r#"{"strokeColorHex": "@ink", "strokeWidth": 6,
                "shadowOpacity": 0.4, "shadowOffset": [0, 4]}"#,
        )
        .expect("style");
        assert_eq!(styled.stroke_width, Some(6.0));
        assert_eq!(styled.shadow_offset, Some([0.0, 4.0]));
        let plain: SubtitleStyle = serde_json::from_str("{}").unwrap();
        let wire = serde_json::to_string(&plain).unwrap();
        assert!(!wire.contains("stroke"), "{wire}");
    }

    /// The stamp is what the FILE uses, not what the binary can do — one
    /// feature at a time, each claiming its own version and nothing more.
    #[test]
    fn a_project_is_stamped_by_what_it_uses() {
        let meta = |body: &str| -> ProjectMetadata {
            ProjectMetadata::from_json(&format!(
                r#"{{"id":"AAAAAAAA-0000-0000-0000-00000000AAAA","name":"v",
                     "createdAt":0,"state":"recorded","trimStart":0,"trimEnd":0,
                     "videoDuration":0,"subtitles":[],
                     "compositionSettings":{{"canvasWidth":1920,"canvasHeight":1080}},
                     {body}}}"#
            ))
            .expect("fixture")
        };
        let layer = |keyframe: &str| {
            format!(
                r#""layers":[{{"id":"L","name":"l","sortIndex":0,"kind":"video",
                   "isEnabled":true,"startTime":0,"duration":1,
                   "keyframes":[{{"id":"K","time":0,"transitionDuration":0.5{keyframe}}}]}}]"#
            )
        };

        assert_eq!(
            meta(&layer("")).minimum_reader_version(),
            1,
            "plain project"
        );
        assert_eq!(
            meta(&layer(r#","motionPath":{"pathResourceID":"P"}"#)).minimum_reader_version(),
            2
        );
        assert_eq!(
            meta(
                r#""resources":[{"id":"R","kind":"image","filename":"a.png",
                  "displayName":"a","addedAt":0,"sprite":{"columns":2,"rows":2}}]"#
            )
            .minimum_reader_version(),
            3
        );
        assert_eq!(
            meta(&layer(
                r#","gradient":{"kind":"linear",
                    "stops":[{"colorHex":"FF0000","at":0},{"colorHex":"0000FF","at":1}],
                    "start":[0,0],"end":[1,1]}"#
            ))
            .minimum_reader_version(),
            4
        );
        assert_eq!(
            meta(&layer(r#","resourceID":"R2""#)).minimum_reader_version(),
            5
        );
        assert_eq!(
            meta(&layer(r#","viewport":[0,0,1,1]"#)).minimum_reader_version(),
            6
        );
        assert_eq!(
            meta(&layer(r#","placement":{"height":540,"anchor":"center"}"#))
                .minimum_reader_version(),
            7
        );

        // The ladder is highest-wins: a project using both is stamped by the
        // newer feature, or an older reader would open it and destroy that one.
        assert_eq!(
            meta(&layer(
                r#","viewport":[0,0,1,1],"placement":{"height":540,"anchor":"center"}"#
            ))
            .minimum_reader_version(),
            7
        );

        // Motion blur is a LAYER field, not a keyframe one, and claims 10:
        // dropped by an older reader's save, the shot silently goes sharp.
        assert_eq!(
            meta(
                r#""layers":[{"id":"L","name":"Clip","sortIndex":0,"kind":"video",
                     "isEnabled":true,"startTime":0,"duration":4,
                     "motionBlur":{"shutter":0.5},"keyframes":[]}]"#
            )
            .minimum_reader_version(),
            10
        );

        // A grade claims 11 — dropped, the look silently reverts — and it
        // outranks blur on the same project (highest-wins).
        assert_eq!(
            meta(
                r#""layers":[{"id":"L","name":"Clip","sortIndex":0,"kind":"video",
                     "isEnabled":true,"startTime":0,"duration":4,
                     "motionBlur":{"shutter":0.5},
                     "adjustments":{"saturation":0},"keyframes":[]}]"#
            )
            .minimum_reader_version(),
            11
        );
        assert_eq!(
            meta(&layer(r#","saturation":0.5"#)).minimum_reader_version(),
            11,
            "and a keyframed grade field claims it too"
        );

        // A blend mode claims 12: dropped, a screen-blended glow becomes an
        // opaque black card over the footage.
        assert_eq!(
            meta(
                r#""layers":[{"id":"L","name":"Glow","sortIndex":0,"kind":"image",
                     "isEnabled":true,"startTime":0,"duration":4,
                     "blendMode":"screen","keyframes":[]}]"#
            )
            .minimum_reader_version(),
            12
        );

        // A mask claims 13: dropped, the porthole vanishes and the full
        // rectangle floods back. The invert flag alone claims it too — a
        // reader that saves it off flips a hole into a window.
        assert_eq!(
            meta(
                r#""layers":[{"id":"L","name":"Clip","sortIndex":0,"kind":"video",
                     "isEnabled":true,"startTime":0,"duration":4,
                     "maskResourceID":"M","keyframes":[]}]"#
            )
            .minimum_reader_version(),
            13
        );
        assert_eq!(
            meta(
                r#""layers":[{"id":"L","name":"Clip","sortIndex":0,"kind":"video",
                     "isEnabled":true,"startTime":0,"duration":4,
                     "maskInverted":true,"keyframes":[]}]"#
            )
            .minimum_reader_version(),
            13
        );

        // A mask that MOVES claims 14: dropped, the window parks.
        assert_eq!(
            meta(&layer(r#","maskRotation":45"#)).minimum_reader_version(),
            14
        );
        assert_eq!(
            meta(&layer(r#","maskOffsetX":-320"#)).minimum_reader_version(),
            14
        );
    }

    /// A new project must not be born carrying the pre-layer timeline, and
    /// the readers must treat its absence as "no legacy timeline" rather
    /// than as a project with no background and no transform.
    #[test]
    fn a_new_project_carries_no_legacy_timeline() {
        let settings = CompositionSettings::default();
        assert!(settings.video_keyframes.is_empty());
        assert!(settings.background_keyframes.is_empty());
    }

    /// Four hand-written sites decide whether a CompositionSettings field
    /// survives a save: the struct, the default, the wire it decodes from and
    /// the wire it encodes to. A field missing from the last one is dropped
    /// silently — the session that set it looks right and the reopen does not.
    #[test]
    fn the_new_caption_defaults_survive_a_round_trip() {
        let json = r#"{
            "canvasWidth": 1920, "canvasHeight": 1080,
            "subtitleAlignment": "trailing",
            "subtitleShadowOffset": [6, -4]
        }"#;
        let settings: CompositionSettings = serde_json::from_str(json).expect("decode");
        assert_eq!(settings.subtitle_alignment, SubtitleTextAlignment::Trailing);
        assert_eq!(settings.subtitle_shadow_offset, Some([6.0, -4.0]));

        let back: CompositionSettings =
            serde_json::from_str(&serde_json::to_string(&settings).expect("encode"))
                .expect("re-decode");
        assert_eq!(back.subtitle_alignment, SubtitleTextAlignment::Trailing);
        assert_eq!(back.subtitle_shadow_offset, Some([6.0, -4.0]));
    }

    /// An offset that was never set must stay unset, or every project made
    /// before the field existed gains a hard [0,0] drop and loses its shadow
    /// behind the glyphs.
    #[test]
    fn an_unset_shadow_offset_is_not_invented_on_save() {
        let settings = CompositionSettings::default();
        let encoded = serde_json::to_string(&settings).expect("encode");
        assert!(
            !encoded.contains("subtitleShadowOffset"),
            "an absent offset must not be written: {encoded}"
        );
    }

    /// The per-caption half of the same gap.
    #[test]
    fn a_caption_can_override_the_plate() {
        let style: SubtitleStyle =
            serde_json::from_str(r#"{"padding": 30, "cornerRadius": 4}"#).expect("style");
        assert_eq!(style.padding, Some(30.0));
        assert_eq!(style.corner_radius, Some(4.0));
        let back: SubtitleStyle =
            serde_json::from_str(&serde_json::to_string(&style).expect("encode"))
                .expect("re-decode");
        assert_eq!(back.padding, Some(30.0));
        assert_eq!(back.corner_radius, Some(4.0));
    }
}

#[cfg(test)]
mod palette_tests {
    use super::*;

    #[test]
    fn a_palette_round_trips_and_resolves() {
        let json = r#"{"canvasWidth": 64, "canvasHeight": 64,
            "backgroundColorHex": "@ink",
            "palette": [{"name": "accent", "colorHex": "5B8CFF"},
                        {"name": "ink", "colorHex": "0B1020"}]}"#;
        let settings: CompositionSettings = serde_json::from_str(json).expect("settings");
        assert_eq!(settings.resolve_color("@accent"), "5B8CFF");
        // Case-insensitive: naming it `accent` and typing `@Accent` is the
        // same colour to a person, so it had better be to the renderer.
        assert_eq!(settings.resolve_color("@ACCENT"), "5B8CFF");
        // A literal is untouched, with or without the hash.
        assert_eq!(settings.resolve_color("FF0000"), "FF0000");
        assert_eq!(settings.resolve_color("#FF0000"), "#FF0000");
        // An unknown name passes through rather than becoming a colour
        // nobody chose; validation is what should name it.
        assert_eq!(settings.resolve_color("@nope"), "@nope");
        // And it survives the manual Wire round trip, which is the trap:
        // CompositionSettings has hand-written Serialize/Deserialize, so a
        // field added in only three of the four places would vanish on save.
        let back: CompositionSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(back.palette, settings.palette);
        assert_eq!(back.background_color_hex, "@ink");
    }

    #[test]
    fn a_project_without_a_palette_grows_no_key() {
        let settings: CompositionSettings =
            serde_json::from_str(r#"{"canvasWidth": 64, "canvasHeight": 64}"#).unwrap();
        assert!(settings.palette.is_none());
        assert!(!serde_json::to_string(&settings)
            .unwrap()
            .contains("palette"));
    }
}

#[cfg(test)]
mod sprite_tests {
    use super::*;

    fn sheet(columns: u32, rows: u32, frames: Option<u32>, fps: Option<f64>) -> SpriteSheet {
        SpriteSheet {
            columns,
            rows,
            frame_count: frames,
            fps,
            frame_durations: None,
        }
    }

    #[test]
    fn frames_run_left_to_right_then_down() {
        let s = sheet(3, 2, None, None);
        assert_eq!(s.frames(), 6);
        // Frame 0 is the top-left cell, one third wide and one half tall.
        assert_eq!(s.uv_rect(0), [0.0, 0.0, 1.0 / 3.0, 0.5]);
        // Frame 3 has wrapped onto the second row.
        assert_eq!(s.uv_rect(3), [0.0, 0.5, 1.0 / 3.0, 0.5]);
        assert_eq!(s.uv_rect(5), [2.0 / 3.0, 0.5, 1.0 / 3.0, 0.5]);
    }

    /// A sheet's last row is often short — 10 frames in a 4×3 grid. The count
    /// is what cycles, so the animation must not step into the two empty
    /// cells at the end and flash blank.
    #[test]
    fn a_short_last_row_cycles_on_the_count_not_the_grid() {
        let s = sheet(4, 3, Some(10), Some(10.0));
        assert_eq!(s.frames(), 10);
        assert_eq!(s.frame_at(0.9), 9, "last real frame");
        assert_eq!(s.frame_at(1.0), 0, "wraps past the empty cells");
        // A count bigger than the grid is a malformed sheet, not a licence to
        // sample outside the texture.
        assert_eq!(sheet(2, 2, Some(99), None).frames(), 4);
    }

    #[test]
    fn a_degenerate_sheet_still_renders_something() {
        let s = sheet(0, 0, Some(0), Some(0.0));
        assert_eq!(s.frames(), 1);
        assert_eq!(s.frame_at(5.0), 0);
        assert_eq!(s.uv_rect(0), [0.0, 0.0, 1.0, 1.0], "the whole image");
        assert!(s.cycle_duration() > 0.0, "no division by zero");
    }

    /// The reason the modulo is taken on the frame INDEX rather than on the
    /// time: an hour into a 12fps loop the index is 43200, exact in f64,
    /// while `elapsed % cycle` accumulates error in the remainder.
    #[test]
    fn a_long_running_loop_does_not_drift() {
        let s = sheet(4, 1, None, Some(12.0));
        assert_eq!(s.frame_at(3600.0), 0);
        assert_eq!(s.frame_at(3600.0 + 1.0 / 12.0), 1);
        assert_eq!(s.frame_at(3600.0 + 3.0 / 12.0), 3);
    }

    /// A GIF that holds one frame for a beat says so per frame. Averaging it
    /// into a single fps is what loses the hold, so the array wins when it is
    /// usable — and is ignored, not half-applied, when it is not.
    #[test]
    fn per_frame_durations_beat_a_uniform_rate_but_only_when_sound() {
        let mut s = sheet(3, 1, None, Some(10.0));
        s.frame_durations = Some(vec![1.0, 0.1, 0.1]);
        assert_eq!(s.frame_at(0.0), 0);
        assert_eq!(s.frame_at(0.99), 0, "still holding the first frame");
        assert_eq!(s.frame_at(1.05), 1);
        assert_eq!(s.frame_at(1.15), 2);
        assert_eq!(s.frame_at(1.25), 0, "wraps after 1.2s");
        assert!((s.cycle_duration() - 1.2).abs() < 1e-9);

        // Wrong length, or a zero, and the array is discarded whole.
        s.frame_durations = Some(vec![1.0, 0.1]);
        assert_eq!(s.frame_at(0.99), 9 % 3, "back to 10fps");
        s.frame_durations = Some(vec![1.0, 0.0, 0.1]);
        assert_eq!(s.frame_at(0.99), 9 % 3, "a zero-length frame is not a hold");
    }

    #[test]
    fn a_sprite_survives_a_round_trip_and_absent_stays_absent() {
        let json = r#"{"id":"R","kind":"image","filename":"walk.png",
            "displayName":"Walk","addedAt":0,"imageCuts":[],
            "disabledAudioTrackIndices":[],
            "sampling":"nearest",
            "sprite":{"columns":4,"rows":2,"frameCount":7,"fps":15}}"#;
        let resource: ProjectResource = serde_json::from_str(json).unwrap();
        let sprite = resource.sprite.clone().expect("sprite");
        assert_eq!(sprite.frames(), 7);
        assert_eq!(resource.sampling, Some(ResourceSampling::Nearest));
        let text = serde_json::to_string(&resource).unwrap();
        assert!(text.contains(r#""columns":4"#), "{text}");
        assert!(text.contains(r#""sampling":"nearest""#), "{text}");

        // An ordinary image writes neither key, so existing projects do not
        // grow fields the moment they are saved.
        let plain: ProjectResource = serde_json::from_str(
            r#"{"id":"R","kind":"image","filename":"a.png","displayName":"A",
                "addedAt":0,"imageCuts":[],"disabledAudioTrackIndices":[]}"#,
        )
        .unwrap();
        let text = serde_json::to_string(&plain).unwrap();
        assert!(!text.contains("sprite"), "{text}");
        assert!(!text.contains("sampling"), "{text}");
    }
}

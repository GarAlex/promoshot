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
    ProjectSourceType,
    [(Video, "video"), (Slideshow, "slideshow")]
);
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
    ]
);
tolerant_enum!(
    ProjectExportKind,
    Images,
    [(Video, "video"), (Images, "images"), (Gif, "gif")]
);
strict_enum!(
    SlideshowImageOrientation,
    [
        (Original, "original"),
        (RotateLeft, "rotateLeft"),
        (RotateRight, "rotateRight"),
        (UpsideDown, "upsideDown"),
    ]
);
strict_enum!(
    SlideshowTransitionEffect,
    [(None, "none"), (Crossfade, "crossfade")]
);
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
    pub subtitle_background_color_hex: String,
    pub subtitle_background_opacity: f64,
    pub subtitle_background_padding: f64,
    pub subtitle_background_corner_radius: f64,
    pub video_border_color_hex: String,
    pub video_border_width: f64,
    pub video_corner_radius: f64,
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
            subtitle_left_margin: 720.0,
            subtitle_right_margin: 60.0,
            subtitle_vertical_margin: 80.0,
            subtitle_font_size: 72.0,
            subtitle_font_family: SubtitleFontFamily::System,
            subtitle_bold: true,
            subtitle_italic: false,
            subtitle_color_hex: "FFFFFF".into(),
            background_color_hex: "000000".into(),
            subtitle_background_color_hex: "000000".into(),
            subtitle_background_opacity: 0.7,
            subtitle_background_padding: 16.0,
            subtitle_background_corner_radius: 8.0,
            video_border_color_hex: "FFFFFF".into(),
            video_border_width: 0.0,
            video_corner_radius: 0.0,
            video_keyframes: vec![VideoKeyframe {
                id: String::new(),
                time: 0.0,
                zoom: 1.0,
                vertical_shift: 0.0,
                horizontal_shift: 0.0,
                transition_duration: 0.5,
            }],
            background_keyframes: vec![BackgroundKeyframe {
                id: String::new(),
                time: 0.0,
                color_hex: "000000".into(),
                transition_duration: 0.5,
            }],
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
    subtitle_background_corner_radius: Option<f64>,
    video_border_color_hex: Option<String>,
    video_border_width: Option<f64>,
    video_corner_radius: Option<f64>,
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
            subtitle_background_corner_radius: w
                .subtitle_background_corner_radius
                .unwrap_or(dflt.subtitle_background_corner_radius),
            video_border_color_hex: w
                .video_border_color_hex
                .unwrap_or(dflt.video_border_color_hex),
            video_border_width: w.video_border_width.unwrap_or(dflt.video_border_width),
            video_corner_radius: w.video_corner_radius.unwrap_or(dflt.video_corner_radius),
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
            subtitle_background_color_hex: &'a str,
            subtitle_background_opacity: f64,
            subtitle_background_padding: f64,
            subtitle_background_corner_radius: f64,
            video_border_color_hex: &'a str,
            video_border_width: f64,
            video_corner_radius: f64,
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
            subtitle_background_color_hex: &self.subtitle_background_color_hex,
            subtitle_background_opacity: self.subtitle_background_opacity,
            subtitle_background_padding: self.subtitle_background_padding,
            subtitle_background_corner_radius: self.subtitle_background_corner_radius,
            video_border_color_hex: &self.video_border_color_hex,
            video_border_width: self.video_border_width,
            video_corner_radius: self.video_corner_radius,
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
    pub transition_duration: f64,
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
    pub image_orientation: Option<SlideshowImageOrientation>,
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
    /// Timing derived from the layer above instead of stated outright.
    ///
    /// `start_time` and `duration` remain the resolved answer — every renderer
    /// keeps reading plain numbers, and this is only the rule that produced
    /// them. That split is deliberate: two renderers interpreting one spec
    /// differently is exactly how caption placement diverged.
    #[serde(default, skip_serializing_if = "is_none")]
    pub timing: Option<LayerTiming>,
    #[serde(default)]
    pub keyframes: Vec<ProjectLayerKeyframe>,
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
    #[serde(skip_serializing_if = "is_none")]
    pub frame: Option<ResourceFrame>,
    #[serde(skip_serializing_if = "is_none")]
    pub looped: Option<bool>,
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
    caption_voice_clip: Option<SubtitleVoiceClip>,
    #[serde(default)]
    drawing: Option<DrawingDocument>,
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
    video_natural_height: Option<f64>,
    #[serde(default)]
    frame: Option<ResourceFrame>,
    #[serde(default)]
    looped: Option<bool>,
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
            image_cuts: w.image_cuts.unwrap_or_default(),
            audio_gain: w.audio_gain,
            volume,
            disabled_audio_track_indices: indices,
            video_natural_width: w.video_natural_width,
            video_natural_height: w.video_natural_height,
            frame: w.frame,
            looped: w.looped,
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
// Slideshow + exports + top level

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlideshowImage {
    pub id: String,
    pub filename: String,
    pub sort_index: i64,
    pub is_enabled: bool,
    pub duration: f64,
    pub transition_duration: f64,
    pub orientation: SlideshowImageOrientation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlideshowSettings {
    pub images: Vec<SlideshowImage>,
    pub transition_effect: SlideshowTransitionEffect,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectExport {
    pub id: String,
    pub kind: ProjectExportKind,
    pub created_at: f64,
    pub filenames: Vec<String>,
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
    pub source_type: Option<ProjectSourceType>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub slideshow: Option<SlideshowSettings>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub layers: Option<Vec<ProjectLayer>>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub resources: Option<Vec<ProjectResource>>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub exports: Option<Vec<ProjectExport>>,
    #[serde(default, skip_serializing_if = "is_none")]
    pub updated_at: Option<f64>,
    pub state: String,
}

impl ProjectMetadata {
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
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

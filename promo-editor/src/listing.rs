//! The store listing: a show whose slides are screenshots standing in a
//! drawn device, captioned in the band the device leaves (review
//! 2026-09-27, P3-46). Ported rule for rule from the app's
//! `AppStoreDevice`, `AppStoreEmphasis`, `AppStoreArrangement` and
//! `AppStoreListing`, so the wizard people use and the one agents run
//! build one listing; `tests/wizard/` holds this to the app's own output.
//!
//! A listing is THREE layers however many shots it has: one device layer
//! whose picture swaps, one caption layer whose words swap, one
//! background. A frame that travels has to be a single object — with a
//! layer per shot, one picture in one place dissolves into another in
//! another place, and the device never moves.

use promo_model::{ResourceFrame, Size};
use serde_json::{json, Value};

/// What the store says about a device: its canvas, the band the caption
/// takes, the finish, the bezel.
pub(crate) struct Device {
    /// The App Store's default accepted screenshot size, as of 2026-08.
    pub canvas: (f64, f64),
    pub material: &'static str,
    /// The caption's share of the canvas, from the top.
    pub band: f64,
    pub bezel: f64,
}

pub(crate) fn device(name: &str) -> Device {
    match name {
        "iPad" => Device {
            canvas: (2064.0, 2752.0),
            material: "spaceBlack",
            band: 0.30,
            bezel: 0.030,
        },
        "mac" => Device {
            canvas: (2880.0, 1800.0),
            material: "silver",
            band: 0.34,
            bezel: 0.018,
        },
        _ => Device {
            canvas: (1290.0, 2796.0),
            material: "naturalTitanium",
            band: 0.26,
            bezel: 0.035,
        },
    }
}

impl Device {
    /// The headline size for a canvas: a share of its width, so a
    /// 1440-wide Mac listing gets words half the size of a 2880-wide one.
    pub fn caption_font_size(&self, canvas_w: f64) -> f64 {
        canvas_w * 0.062
    }

    /// The room the shot has: everything under the caption's band, inset
    /// a little on every side.
    pub fn shot_area(&self, (w, h): (f64, f64)) -> (f64, f64) {
        (w * 0.84, (h - h * self.band) * 0.90)
    }

    /// The rule that fits a picture of `aspect` into that room, centred in
    /// it — both dimensions fitted, the binding one chosen per picture.
    pub fn shot_placement(&self, aspect: f64, canvas: (f64, f64)) -> Value {
        let (area_w, area_h) = self.shot_area(canvas);
        let drop = canvas.1 * self.band / 2.0;
        if aspect > area_w / area_h.max(1.0) {
            json!({"width": area_w, "anchor": "center", "offset": [0.0, drop]})
        } else {
            json!({"height": area_h, "anchor": "center", "offset": [0.0, drop]})
        }
    }

    /// The slab this device wears at this framing: drawn, with a bezel,
    /// a depth and a finish — no photograph of anyone's hardware.
    pub fn slab(&self, framing: &str, material: Option<&str>) -> Value {
        let (tilt_x, tilt_y) = if framing == "angled" {
            (4.0, -12.0)
        } else {
            (0.0, 0.0)
        };
        json!({
            "kind": "device",
            "material": material.unwrap_or(self.material),
            "bezelFraction": self.bezel,
            "tiltX": tilt_x,
            "tiltY": tilt_y,
        })
    }
}

/// The device's name as the listing writes it.
pub(crate) fn label(name: &str) -> &'static str {
    match name {
        "iPad" => "iPad",
        "mac" => "Mac",
        _ => "iPhone",
    }
}

/// The body a headless host stands, having no library to link: the
/// engine's own device recipe. The app links its shared library's model.
pub(crate) fn recipe_kind(name: &str) -> &'static str {
    match name {
        "iPad" => "tablet",
        "mac" => "laptop",
        _ => "phone",
    }
}

/// The shape of the shipped body's SCREEN, width over height (the
/// millimetres the device models are built with, body minus bezels).
fn screen_aspect(name: &str) -> f64 {
    match name {
        "iPad" => 201.5 / 267.6,
        "mac" => 301.6 / 198.2,
        _ => 66.5 / 144.6,
    }
}

/// A screen reel's canvas: the screen's shape at the listing's height, so a
/// screenshot lands on the slot 1 : 1.
pub(crate) fn screen_canvas(name: &str, height: f64) -> (f64, f64) {
    ((height * screen_aspect(name) / 2.0).round() * 2.0, height)
}

/// A camera preset and the light that goes with it: degrees, distance in
/// bounds radii; the light sits ahead of the camera.
pub(crate) fn angle_camera(angle: &str) -> Value {
    let (yaw, pitch, distance, fov) = match angle {
        "straight" => (0.0, 0.0, 4.2, 30.0),
        "quarterRight" => (25.0, 8.0, 4.2, 30.0),
        "hero" => (-35.0, 18.0, 3.8, 28.0),
        "above" => (-15.0, 55.0, 4.2, 30.0),
        _ => (-25.0, 8.0, 4.2, 30.0),
    };
    json!({"yaw": yaw, "pitch": pitch, "roll": 0.0, "distance": distance, "fov": fov})
}

pub(crate) fn angle_light(camera: &Value) -> Value {
    json!({"yaw": camera["yaw"].as_f64().unwrap_or(0.0) + 40.0, "pitch": 50.0, "intensity": 1.0})
}

/// The box a body makes on screen at a camera preset, width over height —
/// MEASURED: the engine drew each body at each preset.
pub(crate) fn box_aspect(device: &str, angle: &str) -> f64 {
    match (device, angle) {
        ("iPad", "straight") => 0.761,
        ("iPad", "quarterLeft" | "quarterRight") => 0.685,
        ("iPad", "hero") => 0.632,
        ("iPad", "above") => 1.165,
        ("mac", "straight") => 1.515,
        ("mac", "quarterLeft" | "quarterRight") => 1.343,
        ("mac", "hero") => 1.120,
        ("mac", "above") => 1.044,
        (_, "straight") => 0.488,
        (_, "hero") => 0.450,
        (_, "above") => 0.799,
        _ => 0.452,
    }
}

/// What a finish paints a body: its colour, and the word (rung 44) the
/// engine owns the numbers behind — metals anodized, plastics matte.
pub(crate) fn finish(material: &str) -> (&'static str, &'static str) {
    let hex = match material {
        "naturalTitanium" => "9B978F",
        "silver" => "D8DADC",
        "gold" => "E6D2A8",
        "deepBlue" => "3A4A63",
        "plasticWhite" => "F4F4F2",
        "plasticBlack" => "202124",
        "plasticBlue" => "2E6BE6",
        "plasticRed" => "E5453B",
        "plasticGreen" => "33B15B",
        "plasticYellow" => "F5C518",
        "plasticPink" => "F06AA6",
        _ => "2B2B2E",
    };
    let word = if material.starts_with("plastic") {
        "matte"
    } else {
        "anodized"
    };
    (hex, word)
}

/// The slots of a device model that wear the chosen finish; binding one a
/// model does not have is harmless, so the rule stays one rule.
pub(crate) const FINISHED_SLOTS: [&str; 2] = ["Body", "Deck"];

/// Which part of the screen a shot is meant to SHOW. The device vacates a
/// side and the words take it, so the part pointed at is always the part
/// nearest the caption.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Emphasis {
    None,
    Top,
    Bottom,
    Leading,
    Trailing,
}

/// A rectangle: x, y, width, height.
#[derive(Clone, Copy)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl Rect {
    fn max_x(&self) -> f64 {
        self.x + self.w
    }
    fn mid_y(&self) -> f64 {
        self.y + self.h / 2.0
    }
}

impl Emphasis {
    /// The shot at `index` under an arrangement: centred never moves;
    /// sides alternate which half of the screen is nearest the reader;
    /// stacked alternates top and bottom.
    pub fn at(arrangement: &str, index: usize) -> Self {
        match arrangement {
            "sides" if index.is_multiple_of(2) => Emphasis::Trailing,
            "sides" => Emphasis::Leading,
            "stacked" if index.is_multiple_of(2) => Emphasis::Bottom,
            "stacked" => Emphasis::Top,
            _ => Emphasis::None,
        }
    }

    /// How far from the canvas centre the device sits to hide `hiding` of
    /// itself off the far edge. Showing the BOTTOM means pushing up.
    fn offset(self, (bw, bh): (f64, f64), (cw, ch): (f64, f64), hiding: f64) -> (f64, f64) {
        let share = hiding.clamp(0.0, 0.6);
        match self {
            Emphasis::None => (0.0, 0.0),
            Emphasis::Bottom => (0.0, -(ch / 2.0 + bh * (share - 0.5))),
            Emphasis::Top => (0.0, ch / 2.0 + bh * (share - 0.5)),
            Emphasis::Trailing => (-(cw / 2.0 + bw * (share - 0.5)), 0.0),
            Emphasis::Leading => (cw / 2.0 + bw * (share - 0.5), 0.0),
        }
    }

    /// The room the device left behind — what the caption must fit in.
    fn clear_rect(self, framed: (f64, f64), canvas: (f64, f64), hiding: f64) -> Rect {
        let (cw, ch) = canvas;
        let (bw, bh) = framed;
        let whole = Rect {
            x: 0.0,
            y: 0.0,
            w: cw,
            h: ch,
        };
        let (cx, cy) = (cw / 2.0, ch / 2.0);
        let (sx, sy) = self.offset(framed, canvas, hiding);
        match self {
            Emphasis::None => whole,
            Emphasis::Bottom => {
                let edge = cy + sy + bh / 2.0;
                Rect {
                    x: 0.0,
                    y: edge,
                    w: cw,
                    h: (ch - edge).max(0.0),
                }
            }
            Emphasis::Top => {
                let edge = cy + sy - bh / 2.0;
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w: cw,
                    h: edge.max(0.0),
                }
            }
            Emphasis::Trailing => {
                let edge = cx + sx + bw / 2.0;
                Rect {
                    x: edge,
                    y: 0.0,
                    w: (cw - edge).max(0.0),
                    h: ch,
                }
            }
            Emphasis::Leading => {
                let edge = cx + sx - bw / 2.0;
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w: edge.max(0.0),
                    h: ch,
                }
            }
        }
    }
}

/// One screenshot in the listing.
pub(crate) struct Shot {
    pub resource_id: String,
    /// The picture's pixels and the slab it wears: the drawn box is the
    /// framed size, bezel included.
    pub content: Option<(f64, f64)>,
    pub frame: Option<ResourceFrame>,
    pub headline: String,
    pub emphasis: Emphasis,
    /// How much of the device leaves the canvas to show its emphasis.
    pub hiding: f64,
    /// A 3D body's measured box (width over height) at its camera, when the
    /// shot is not the framed picture.
    pub box_aspect: Option<f64>,
}

/// What a caption resource needs, besides its id.
pub(crate) struct Caption {
    pub display_name: String,
    pub text: String,
    pub style: Value,
}

pub(crate) struct Built {
    pub captions: Vec<Caption>,
    /// Background, device, headline — ids and resources filled in by the
    /// caller, which owns the id stream.
    pub device_keys: Vec<Value>,
    /// (time, caption index) per headline swap; index 0's is the layer's
    /// own resource.
    pub caption_keys: Vec<(f64, usize)>,
    pub span: f64,
}

/// A soft cap on how much of the device may leave the canvas.
const MAX_HIDDEN: f64 = 0.5;

/// The drawn box of a framed shot, bezel included.
fn framed_box(shot: &Shot, drawn_height: f64) -> (f64, f64) {
    let source = shot
        .content
        .map(|(w, h)| {
            let framed =
                promo_timeline::frame::framed_pixel_size(Size::new(w, h), shot.frame.as_ref());
            (framed.width(), framed.height())
        })
        .unwrap_or((1.0, 1.0));
    let aspect = if source.1 > 0.0 {
        source.0 / source.1
    } else {
        1.0
    };
    (drawn_height * aspect, drawn_height)
}

/// How tall a headline's box is: an estimate — a bold sans averages a
/// little over half its point size per glyph.
fn caption_box_height(headline: &str, font_size: f64, width: f64, padding: f64) -> f64 {
    if width <= 0.0 || font_size <= 0.0 {
        return 0.0;
    }
    let per_glyph = font_size * 0.52;
    let per_line = ((width / per_glyph).floor() as i64).max(1);
    let glyphs = headline.chars().count() as f64;
    let lines = ((glyphs / per_line as f64).ceil() as i64).max(1);
    lines as f64 * (font_size * 1.25).ceil() + padding * 2.0
}

/// The largest size at which the headline clears the device, down to a
/// floor below which it is not a headline any more.
fn fitted_font_size(headline: &str, room: Rect, start: f64, padding: f64, inset: f64) -> f64 {
    const FLOOR: f64 = 48.0;
    let width = (room.w - inset * 2.0).max(1.0);
    let mut candidate = start;
    while candidate > FLOOR {
        if caption_box_height(headline, candidate, width, padding) <= room.h {
            return candidate;
        }
        candidate -= 4.0;
    }
    FLOOR
}

/// The caption's margins, from the room the device left; the look comes
/// in, only the geometry is decided here.
fn caption_style(
    room: Rect,
    canvas: (f64, f64),
    font_size: f64,
    padding: f64,
    inset: f64,
    headline: &str,
    look: &Value,
) -> Value {
    let mut style = look.clone();
    style["fontSize"] = json!(font_size);
    style["padding"] = json!(padding);
    style["backgroundOpacity"] = json!(0.0);
    let height = caption_box_height(
        headline,
        font_size,
        (room.w - inset * 2.0).max(1.0),
        padding,
    );
    style["leftMargin"] = json!(room.x + inset);
    style["rightMargin"] = json!(canvas.0 - room.max_x() + inset);
    style["verticalMargin"] = json!(inset.max(room.mid_y() - height / 2.0));
    style["alignment"] = json!(if room.max_x() < canvas.0 - 1.0 {
        "leading"
    } else if room.x > 1.0 {
        "trailing"
    } else {
        "center"
    });
    style
}

/// The listing's geometry and schedule: the device's keyframes carry BOTH
/// the swap and the placement, so one keyframe says "this shot, here" and
/// the ramp between two moves the frame while the picture changes. A
/// headline swaps with no blend — two at once is unreadable — landing at
/// the end of the picture's arrival, so the words never describe a shot
/// that has not arrived.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build(
    shots: &[Shot],
    canvas: (f64, f64),
    drawn_height: f64,
    shot_room: (f64, f64),
    base_offset: (f64, f64),
    seconds_per_shot: f64,
    arrival: Option<Value>,
    alternating_tilt: Option<f64>,
    base_font_size: f64,
    inset: f64,
    look: &Value,
) -> Option<Built> {
    const PADDING: f64 = 16.0;
    if shots.is_empty() || seconds_per_shot <= 0.0 || canvas.0 <= 0.0 || canvas.1 <= 0.0 {
        return None;
    }
    let span = seconds_per_shot * shots.len() as f64;
    let blend = arrival
        .as_ref()
        .and_then(|a| a["duration"].as_f64())
        .unwrap_or(0.0);
    let mut captions = Vec::new();
    let mut device_keys = Vec::new();
    let mut caption_keys = Vec::new();
    for (index, shot) in shots.iter().enumerate() {
        let at = seconds_per_shot * index as f64;
        let framed = shot
            .box_aspect
            .map(|a| (drawn_height * a, drawn_height))
            .unwrap_or_else(|| framed_box(shot, drawn_height));
        let hidden = shot.hiding.clamp(0.0, MAX_HIDDEN);
        let offset = shot.emphasis.offset(framed, canvas, hidden);
        let room = if shot.emphasis == Emphasis::None {
            let top = canvas.1 / 2.0 + base_offset.1 - framed.1 / 2.0;
            Rect {
                x: 0.0,
                y: 0.0,
                w: canvas.0,
                h: top.max(0.0),
            }
        } else {
            shot.emphasis.clear_rect(framed, canvas, hidden)
        };

        let offsets = [offset.0 + base_offset.0, offset.1 + base_offset.1];
        let room_aspect = if shot_room.1 > 0.0 {
            shot_room.0 / shot_room.1
        } else {
            1.0
        };
        let shot_aspect = if framed.1 > 0.0 {
            framed.0 / framed.1
        } else {
            1.0
        };
        let placement = if shot_aspect > room_aspect && shot_room.0 > 0.0 {
            json!({"width": shot_room.0, "anchor": "center", "offset": offsets})
        } else {
            json!({"height": drawn_height, "anchor": "center", "offset": offsets})
        };
        let mut key = json!({
            "time": at,
            "transitionDuration": if index == 0 { 0.0 } else { seconds_per_shot * 0.7 },
            "placement": placement,
        });
        if let Some(tilt) = alternating_tilt {
            key["tiltY"] = json!(if index % 2 == 0 { -tilt } else { tilt });
        }
        if index > 0 {
            key["resourceID"] = json!(shot.resource_id);
            if let Some(arrival) = &arrival {
                key["transition"] = arrival.clone();
            }
        }
        device_keys.push(key);

        let size = fitted_font_size(&shot.headline, room, base_font_size, PADDING, inset);
        captions.push(Caption {
            display_name: if shot.headline.is_empty() {
                format!("Caption {}", index + 1)
            } else {
                shot.headline.clone()
            },
            text: shot.headline.clone(),
            style: caption_style(room, canvas, size, PADDING, inset, &shot.headline, look),
        });
        caption_keys.push((if index > 0 { at + blend } else { 0.0 }, index));
    }
    Some(Built {
        captions,
        device_keys,
        caption_keys,
        span,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot(index: usize, headline: &str, emphasis: Emphasis, hiding: f64) -> Shot {
        let frame: ResourceFrame = serde_json::from_value(json!({"kind": "device"})).unwrap();
        Shot {
            resource_id: format!("shot-{index}"),
            content: Some((1179.0, 2556.0)),
            frame: Some(frame),
            headline: headline.into(),
            emphasis,
            hiding,
            box_aspect: None,
        }
    }

    fn fade() -> Option<Value> {
        Some(json!({"kind": "fade", "duration": 0.45}))
    }

    const CANVAS: (f64, f64) = (1290.0, 2796.0);

    /// A listing is three layers however many shots: the device's keyframes
    /// carry four swaps for five shots, each saying where its shot sits;
    /// the words CUT (a blend would draw two headlines) and land after
    /// their picture has arrived — the app's emitter tests, ported with it.
    #[test]
    fn a_listing_swaps_one_device_and_one_headline_whatever_the_count() {
        let shots: Vec<Shot> = (1..=5)
            .map(|i| {
                let emphasis = if i % 2 == 0 {
                    Emphasis::Bottom
                } else {
                    Emphasis::Trailing
                };
                shot(i, &format!("Headline {i}"), emphasis, 1.0 / 3.0)
            })
            .collect();
        let built = build(
            &shots,
            CANVAS,
            1850.0,
            (CANVAS.0, 1850.0),
            (0.0, 0.0),
            3.0,
            fade(),
            None,
            88.0,
            90.0,
            &json!({}),
        )
        .unwrap();
        assert_eq!(built.span, 15.0);
        assert_eq!(built.captions.len(), 5, "a headline per shot");
        let swaps = built
            .device_keys
            .iter()
            .filter(|k| k.get("resourceID").is_some())
            .count();
        assert_eq!(swaps, 4, "the layer's own resource is the first");
        assert!(built
            .device_keys
            .iter()
            .all(|k| k.get("placement").is_some()));
        assert_eq!(
            built.caption_keys[1].0, 3.45,
            "after its picture has arrived"
        );
    }

    /// A headline shrinks to clear the device rather than overlapping it,
    /// down to a floor below which it is not a headline any more.
    #[test]
    fn a_headline_shrinks_to_clear_the_device() {
        let roomy = build(
            &[shot(1, "Short", Emphasis::Bottom, 1.0 / 3.0)],
            CANVAS,
            1850.0,
            (CANVAS.0, 1850.0),
            (0.0, 0.0),
            3.0,
            fade(),
            None,
            88.0,
            90.0,
            &json!({}),
        )
        .unwrap();
        assert_eq!(
            roomy.captions[0].style["fontSize"], 88.0,
            "a short headline needs no help"
        );
        let long = "a headline that will not fit ".repeat(4);
        let tight = build(
            &[shot(1, &long, Emphasis::Trailing, 0.05)],
            CANVAS,
            2400.0,
            (CANVAS.0, 2400.0),
            (0.0, 0.0),
            3.0,
            fade(),
            None,
            88.0,
            90.0,
            &json!({}),
        )
        .unwrap();
        let size = tight.captions[0].style["fontSize"].as_f64().unwrap();
        assert!(size < 88.0 && size >= 48.0, "{size}");
    }

    /// Turning is an option: on, the angle alternates; off, none is written.
    #[test]
    fn alternating_tilt_is_an_option_not_a_bake() {
        let shots: Vec<Shot> = (1..=4)
            .map(|i| shot(i, "H", Emphasis::None, 1.0 / 3.0))
            .collect();
        let plain = build(
            &shots,
            CANVAS,
            1850.0,
            (CANVAS.0, 1850.0),
            (0.0, 0.0),
            3.0,
            fade(),
            None,
            88.0,
            90.0,
            &json!({}),
        )
        .unwrap();
        assert!(plain.device_keys.iter().all(|k| k.get("tiltY").is_none()));
        let turned = build(
            &shots,
            CANVAS,
            1850.0,
            (CANVAS.0, 1850.0),
            (0.0, 0.0),
            3.0,
            fade(),
            Some(14.0),
            88.0,
            90.0,
            &json!({}),
        )
        .unwrap();
        let tilts: Vec<f64> = turned
            .device_keys
            .iter()
            .map(|k| k["tiltY"].as_f64().unwrap())
            .collect();
        assert_eq!(tilts, vec![-14.0, 14.0, -14.0, 14.0]);
    }

    /// The arrangement is a PATTERN across the set, and it alternates: one
    /// decision instead of one per shot.
    #[test]
    fn an_arrangement_alternates_across_the_set() {
        let pattern = |name: &str| (0..4).map(|i| Emphasis::at(name, i)).collect::<Vec<_>>();
        assert!(pattern("centred").iter().all(|e| *e == Emphasis::None));
        assert!(
            pattern("sides")
                == vec![
                    Emphasis::Trailing,
                    Emphasis::Leading,
                    Emphasis::Trailing,
                    Emphasis::Leading
                ]
        );
        assert!(
            pattern("stacked")
                == vec![
                    Emphasis::Bottom,
                    Emphasis::Top,
                    Emphasis::Bottom,
                    Emphasis::Top
                ]
        );
    }

    /// "Show the bottom" pushes the device UP and leaves the room below it;
    /// showing the right pushes it left and leaves the right-hand column;
    /// hiding more leaves more room — the trade being made.
    #[test]
    fn emphasis_pushes_the_opposite_way_and_says_what_is_left() {
        let (canvas, framed) = (CANVAS, (901.0, 1954.0));
        let near = |a: f64, b: f64| (a - b).abs() <= 1.0;
        let up = Emphasis::Bottom.offset(framed, canvas, 1.0 / 3.0);
        assert!(near(up.1, -1072.0) && up.0 == 0.0, "{up:?}");
        let room = Emphasis::Bottom.clear_rect(framed, canvas, 1.0 / 3.0);
        assert!(near(room.y, 1398.0 - 1072.0 + 977.0), "{}", room.y);
        assert!(near(room.h, canvas.1 - room.y) && room.h > 1400.0);
        let left = Emphasis::Trailing.offset(framed, canvas, 1.0 / 3.0);
        assert!(near(left.0, -495.0), "{left:?}");
        let column = Emphasis::Trailing.clear_rect(framed, canvas, 1.0 / 3.0);
        assert!(near(column.x, 645.0 - 495.0 + 450.5) && column.w > 500.0);
        let half = Emphasis::Trailing.clear_rect(framed, canvas, 0.5);
        assert!(half.w > column.w);
        assert_eq!(Emphasis::None.offset(framed, canvas, 0.5), (0.0, 0.0));
        let whole = Emphasis::None.clear_rect(framed, canvas, 0.5);
        assert_eq!(
            (whole.x, whole.y, whole.w, whole.h),
            (0.0, 0.0, canvas.0, canvas.1)
        );
    }

    /// A reel's canvas is the device screen's shape, with an even width.
    #[test]
    fn a_reel_is_the_screen_shape_at_an_even_width() {
        for device_name in ["iPhone", "iPad", "mac"] {
            let height = device(device_name).canvas.1;
            let (w, h) = screen_canvas(device_name, height);
            assert!(
                (w / h - screen_aspect(device_name)).abs() < 0.002,
                "{device_name}"
            );
            assert_eq!(w as i64 % 2, 0, "an even width encodes");
        }
    }
}

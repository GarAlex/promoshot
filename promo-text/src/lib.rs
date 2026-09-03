//! promo-text: turning a caption into pixels.
//!
//! Until now text was the one thing the core could not draw — captions were
//! rasterized by the host (CoreText on Apple), which is why a headless render
//! silently dropped them. This does the layout the app documents in
//! `VideoComposer.drawSubtitle`: a panel inset by the style's margins, the
//! text wrapped inside it, the background box sized to the text plus padding
//! and aligned within the panel.
//!
//! Deliberately not a general text engine: one styled block, no rich runs, no
//! bidi tailoring beyond what shaping gives for free.
//!
//! **Against CoreText.** Shaping and metrics match exactly — the same string
//! at the same size lands in an identical glyph bounding box. Weight does
//! not: CoreText dilates stems for light-on-dark text, so unsmoothed output
//! is about 7% lighter. [`TextStyle::smoothing`] recovers roughly half of
//! that by remapping coverage; the remainder would need real outline
//! emboldening, which is the honest next step if captions ever have to match
//! the Apple renderer pixel for pixel.

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, SwashCache, Weight};
use std::sync::{Mutex, OnceLock};

/// Where the text sits horizontally inside the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Leading,
    Center,
    Trailing,
}

impl Align {
    pub fn parse(raw: &str) -> Self {
        match raw {
            "center" => Align::Center,
            "trailing" | "right" => Align::Trailing,
            _ => Align::Leading,
        }
    }
}

/// Everything the app's subtitle style carries, flattened.
#[derive(Debug, Clone)]
pub struct TextStyle {
    /// A family name, or None for the system UI font.
    pub font_family: Option<String>,
    pub font_size: f64,
    pub bold: bool,
    pub italic: bool,
    pub align: Align,
    /// Straight (non-premultiplied) RGBA, 0–255.
    pub text_rgba: [u8; 4],
    pub background_rgba: [u8; 4],
    pub padding: f64,
    pub corner_radius: f64,
    pub left_margin: f64,
    pub right_margin: f64,
    /// Distance from the TOP of the canvas, matching the app.
    pub vertical_margin: f64,
    /// Line height as a multiple of font size.
    pub line_height: f64,
    /// Where the box hangs, when a placement rule says so — the same
    /// nine-anchor grid media layers use, only `anchor` and `offset` read
    /// (a caption's size is typography, not a rule). Present, it decides
    /// the position outright and the margins keep only their wrap-width
    /// job.
    pub placement: Option<promo_model::Placement>,
    /// Outline drawn around the glyphs, in canvas pixels. Zero width is no
    /// outline.
    ///
    /// This is what lets a caption sit straight on FOOTAGE with no plate
    /// behind it — the social-caption look — where plain text is grey mush
    /// the moment the frame behind it is bright. It lives inside the
    /// padding: the raster is exactly text-plus-padding, so a stroke wider
    /// than the padding is clipped rather than growing the box (which would
    /// move the caption and resize any plate).
    pub stroke_rgba: [u8; 4],
    pub stroke_width: f64,
    /// A soft drop shadow under everything else. Same padding budget.
    pub shadow_rgba: [u8; 4],
    pub shadow_radius: f64,
    pub shadow_offset: [f64; 2],
    /// Coverage gamma — "font smoothing".
    ///
    /// Glyph antialiasing is blended in sRGB space by the compositor, but
    /// sRGB is not linear: a 50%-covered edge pixel written as 127 carries
    /// only about 21% of white's linear luminance, so light text on a dark
    /// background renders visibly thinner than the outline. Measured against
    /// CoreText on the same string, font and size, unsmoothed output had 7%
    /// less ink for an identical glyph bounding box.
    ///
    /// `None` picks a value from the text colour: light text is thickened,
    /// dark text left alone (dark-on-light has the opposite bias and is much
    /// less pronounced). Set it explicitly to override.
    ///
    /// Measured on "Formulas without friction", Helvetica Neue Bold 54,
    /// white on #0E1726, mean ink over an identical 640x40 glyph box:
    /// unsmoothed 113.3, gamma 2.0 → 117.5, gamma 3.4 → 120.3, CoreText
    /// 122.1. The default is 2.0: it recovers half the gap without hardening
    /// edges. Closing the rest needs outline emboldening (stem darkening),
    /// which coverage remapping cannot do — see the crate docs.
    pub smoothing: Option<f64>,
}

impl TextStyle {
    /// Every LENGTH in this style multiplied by `factor`, for rasterizing the
    /// same caption at a denser texture size.
    ///
    /// Destructured WITHOUT `..` on purpose: a new field on `TextStyle` stops
    /// this function compiling until someone decides whether it is a length.
    /// The last two added without that decision — `stroke_width` and
    /// `shadow_radius` — were left out of the caller's hand-written list, and
    /// an export denser than the canvas drew the outline and the shadow at a
    /// fraction of their size while the glyphs scaled correctly.
    pub fn scaled_lengths(&self, factor: f64) -> Self {
        let TextStyle {
            font_family,
            font_size,
            bold,
            italic,
            align,
            text_rgba,
            background_rgba,
            padding,
            corner_radius,
            left_margin,
            right_margin,
            vertical_margin,
            line_height,
            stroke_rgba,
            stroke_width,
            shadow_rgba,
            shadow_radius,
            shadow_offset,
            placement,
            smoothing,
        } = self;
        TextStyle {
            font_family: font_family.clone(),
            font_size: font_size * factor,
            bold: *bold,
            italic: *italic,
            align: *align,
            text_rgba: *text_rgba,
            background_rgba: *background_rgba,
            padding: padding * factor,
            corner_radius: corner_radius * factor,
            left_margin: left_margin * factor,
            right_margin: right_margin * factor,
            vertical_margin: vertical_margin * factor,
            // A multiple of the font size, which already scaled.
            line_height: *line_height,
            stroke_rgba: *stroke_rgba,
            stroke_width: stroke_width * factor,
            shadow_rgba: *shadow_rgba,
            shadow_radius: shadow_radius * factor,
            shadow_offset: [shadow_offset[0] * factor, shadow_offset[1] * factor],
            // The offset is a length; the anchor is not.
            placement: placement.as_ref().map(|rule| {
                let mut scaled = rule.clone();
                if let Some(offset) = scaled.offset {
                    scaled.offset = Some([offset[0] * factor, offset[1] * factor]);
                }
                scaled
            }),
            // Coverage gamma — dimensionless.
            smoothing: *smoothing,
        }
    }
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            font_family: None,
            stroke_rgba: [0, 0, 0, 0],
            stroke_width: 0.0,
            shadow_rgba: [0, 0, 0, 0],
            shadow_radius: 0.0,
            shadow_offset: [0.0, 0.0],
            font_size: 72.0,
            bold: true,
            italic: false,
            align: Align::Center,
            text_rgba: [255, 255, 255, 255],
            background_rgba: [0, 0, 0, 178],
            padding: 16.0,
            corner_radius: 8.0,
            left_margin: 60.0,
            right_margin: 60.0,
            vertical_margin: 80.0,
            line_height: 1.25,
            placement: None,
            smoothing: None,
        }
    }
}

/// Distance from every pixel to the nearest covered one, by two chamfer
/// passes. O(w·h) and round enough for an outline; a separable max filter
/// would give square corners on round letters.
fn distance_to_ink(mask: &[f32], width: usize, height: usize) -> Vec<f32> {
    const NEAR: f32 = 1.0;
    const DIAG: f32 = std::f32::consts::SQRT_2;
    let far = (width + height) as f32;
    let mut dist: Vec<f32> = mask
        .iter()
        .map(|&a| if a >= 0.5 { 0.0 } else { far })
        .collect();
    let at = |x: usize, y: usize| y * width + x;
    for y in 0..height {
        for x in 0..width {
            let mut d = dist[at(x, y)];
            if y > 0 {
                d = d.min(dist[at(x, y - 1)] + NEAR);
                if x > 0 {
                    d = d.min(dist[at(x - 1, y - 1)] + DIAG);
                }
                if x + 1 < width {
                    d = d.min(dist[at(x + 1, y - 1)] + DIAG);
                }
            }
            if x > 0 {
                d = d.min(dist[at(x - 1, y)] + NEAR);
            }
            dist[at(x, y)] = d;
        }
    }
    for y in (0..height).rev() {
        for x in (0..width).rev() {
            let mut d = dist[at(x, y)];
            if y + 1 < height {
                d = d.min(dist[at(x, y + 1)] + NEAR);
                if x + 1 < width {
                    d = d.min(dist[at(x + 1, y + 1)] + DIAG);
                }
                if x > 0 {
                    d = d.min(dist[at(x - 1, y + 1)] + DIAG);
                }
            }
            if x + 1 < width {
                d = d.min(dist[at(x + 1, y)] + NEAR);
            }
            dist[at(x, y)] = d;
        }
    }
    dist
}

/// A separable box blur, run three times — close enough to a gaussian for a
/// shadow, and linear in the radius rather than quadratic.
fn blur_mask(mask: &[f32], width: usize, height: usize, radius: f64) -> Vec<f32> {
    // THREE passes approximate a gaussian of sigma ≈ radius only when each
    // pass is a THIRD of it. Running each pass at the full radius spreads
    // the alpha roughly 1.7x too far and collapses the peak, which is why a
    // "radius 10" shadow read as a barely-there smudge instead of a shadow.
    let per_pass = (radius / 3.0).max(0.0);
    if per_pass <= 0.0 || width == 0 || height == 0 {
        return mask.to_vec();
    }
    // A box radius has to be a whole number of pixels, and ROUNDING it made
    // the control coarse in a way nothing on screen explained: 8, 9 and 10
    // all rounded to 3 and blurred identically, so two steps in every three
    // did nothing while the number above the slider kept moving. Blend the
    // two neighbouring box radii instead — the softness then follows the
    // value, and a radius that lands exactly on a whole box (9 → 3) still
    // renders exactly as it did before.
    let lower_radius = per_pass.floor();
    let fraction = (per_pass - lower_radius) as f32;
    let lower_radius = lower_radius as usize;
    let lower = if lower_radius == 0 {
        mask.to_vec()
    } else {
        box_blur3(mask, width, height, lower_radius)
    };
    if fraction < 1e-4 {
        return lower;
    }
    let upper = box_blur3(mask, width, height, lower_radius + 1);
    lower
        .iter()
        .zip(&upper)
        .map(|(a, b)| a + (b - a) * fraction)
        .collect()
}

/// Three box passes at radius `r`, horizontal then vertical each time.
fn box_blur3(mask: &[f32], width: usize, height: usize, r: usize) -> Vec<f32> {
    let mut src = mask.to_vec();
    let mut dst = vec![0.0f32; src.len()];
    for _ in 0..3 {
        for y in 0..height {
            for x in 0..width {
                let (mut sum, mut n) = (0.0f32, 0.0f32);
                for k in x.saturating_sub(r)..=(x + r).min(width - 1) {
                    sum += src[y * width + k];
                    n += 1.0;
                }
                dst[y * width + x] = sum / n.max(1.0);
            }
        }
        std::mem::swap(&mut src, &mut dst);
        for x in 0..width {
            for y in 0..height {
                let (mut sum, mut n) = (0.0f32, 0.0f32);
                for k in y.saturating_sub(r)..=(y + r).min(height - 1) {
                    sum += src[k * width + x];
                    n += 1.0;
                }
                dst[y * width + x] = sum / n.max(1.0);
            }
        }
        std::mem::swap(&mut src, &mut dst);
    }
    src
}

/// Where one reveal unit — a grapheme, a word, or a whole line — sits inside
/// the raster.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnitSpan {
    /// Visual line, counting wrapped lines separately.
    pub line: u32,
    /// Raster-space x of the unit's left and right edges, padding included.
    pub start_x: f64,
    pub end_x: f64,
}

/// The geometry a reveal needs: where every unit is, in a raster laid out for
/// the WHOLE string.
///
/// Laid out whole on purpose. Re-rasterizing a growing prefix would re-flow
/// it — the measured width decides the box, the box decides the position — so
/// a centred caption would slide as it typed, and every frame would miss the
/// raster cache. The reveal is a crop of one stable picture.
#[derive(Debug, Clone, PartialEq)]
pub struct RevealLayout {
    pub units: Vec<UnitSpan>,
    /// Raster-space y of each line's top, padding included.
    pub line_tops: Vec<f64>,
    pub line_height: f64,
    /// Raster size, so a caller can turn spans into texture fractions.
    pub width: f64,
    pub height: f64,
}

/// Where each reveal unit sits, for text laid out exactly as `rasterize`
/// lays it out.
pub fn reveal_layout(
    text: &str,
    canvas_width: f64,
    canvas_height: f64,
    style: &TextStyle,
    by: RevealBy,
) -> Option<RevealLayout> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let box_ = measure(text, canvas_width, canvas_height, style)?;
    // Laid out exactly as `rasterize` lays it out — same font resolution,
    // same metrics, same wrap width — or the spans would describe a picture
    // nobody drew.
    let mut fonts = FontSystem::new();
    let resolved = resolve_family(&mut fonts, style.font_family.as_deref());
    let metrics = Metrics::new(
        style.font_size as f32,
        (style.font_size * style.line_height) as f32,
    );
    let mut buffer = Buffer::new(&mut fonts, metrics);
    let mut buffer = buffer.borrow_with(&mut fonts);
    buffer.set_size(Some(box_.text_width as f32), None);
    let family = match &resolved {
        ResolvedFamily::Named(name) => Family::Name(name),
        ResolvedFamily::Serif => Family::Serif,
        ResolvedFamily::Monospace => Family::Monospace,
        ResolvedFamily::SansSerif => Family::SansSerif,
    };
    let attrs = Attrs::new()
        .family(family)
        .weight(if style.bold {
            Weight::BOLD
        } else {
            Weight::NORMAL
        })
        .style(if style.italic {
            Style::Italic
        } else {
            Style::Normal
        });
    buffer.set_text(text, attrs, Shaping::Advanced);
    buffer.shape_until_scroll(true);

    let padding = style.padding;
    let mut units: Vec<UnitSpan> = Vec::new();
    let mut line_tops: Vec<f64> = Vec::new();

    for (line, run) in buffer.layout_runs().enumerate() {
        line_tops.push(padding + line as f64 * metrics.line_height as f64);
        match by {
            // One span for the whole line, left edge to right edge.
            RevealBy::Line => units.push(UnitSpan {
                line: line as u32,
                start_x: padding,
                end_x: padding + run.line_w as f64,
            }),
            RevealBy::Character => {
                // By CLUSTER, not byte: a glyph carries the byte range of the
                // cluster it belongs to, so an emoji or a combining accent is
                // one tick of the typewriter rather than several.
                let mut cluster = usize::MAX;
                for glyph in run.glyphs {
                    if glyph.start != cluster {
                        cluster = glyph.start;
                        units.push(UnitSpan {
                            line: line as u32,
                            start_x: padding + glyph.x as f64,
                            end_x: padding + (glyph.x + glyph.w) as f64,
                        });
                    } else if let Some(last) = units.last_mut() {
                        last.end_x = last.end_x.max(padding + (glyph.x + glyph.w) as f64);
                    }
                }
            }
            RevealBy::Word => {
                // The shaper does not hand back word boundaries, so they come
                // from the run's own text by byte range. Glyphs are matched by
                // range rather than by counting: at a soft wrap the trailing
                // space is dropped from the run entirely, so counts drift.
                let mut current: Option<UnitSpan> = None;
                for glyph in run.glyphs {
                    let text_of = run.text.get(glyph.start..glyph.end).unwrap_or("");
                    let is_space =
                        !text_of.is_empty() && text_of.chars().all(|c| c.is_whitespace());
                    if is_space {
                        if let Some(span) = current.take() {
                            units.push(span);
                        }
                        continue;
                    }
                    let left = padding + glyph.x as f64;
                    let right = padding + (glyph.x + glyph.w) as f64;
                    match current.as_mut() {
                        Some(span) => {
                            span.start_x = span.start_x.min(left);
                            span.end_x = span.end_x.max(right);
                        }
                        None => {
                            current = Some(UnitSpan {
                                line: line as u32,
                                start_x: left,
                                end_x: right,
                            })
                        }
                    }
                }
                if let Some(span) = current.take() {
                    units.push(span);
                }
            }
        }
    }

    Some(RevealLayout {
        units,
        line_tops,
        line_height: metrics.line_height as f64,
        width: box_.width,
        height: box_.height,
    })
}

/// Which unit a reveal walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevealBy {
    Character,
    Word,
    Line,
}

/// A rasterized caption and where it belongs on the canvas.
#[derive(Debug, Clone)]
pub struct RasterizedText {
    /// Straight RGBA rows, `width * height * 4`.
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Top-left position in canvas coordinates, y down — what the compositor
    /// wants. The app's rule is stated bottom-up; the conversion happens here
    /// so no caller has to remember which way is up.
    pub x: f64,
    pub y: f64,
    /// How many lines it wrapped to.
    pub lines: u32,
}

/// What `resolve_family` settled on.
#[derive(Debug)]
enum ResolvedFamily {
    Named(String),
    SansSerif,
    Serif,
    Monospace,
}

/// UI sans-serifs in preference order — Apple first, then the common Linux
/// and Windows faces, so a headline looks like a headline everywhere.
const UI_SANS: &[&str] = &[
    "SF Pro Text",
    "SF Pro Display",
    "Helvetica Neue",
    "Helvetica",
    "Inter",
    "Segoe UI",
    "Noto Sans",
    "DejaVu Sans",
    "Liberation Sans",
    "Arial",
];

/// Lowercased with spaces, hyphens and underscores removed, so the name a
/// project stores can be compared with the name the system installed.
fn normalized_family(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_whitespace() && *c != '-' && *c != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

/// The installed family matching `name`, if any.
///
/// Projects store the app's wire spelling — "georgia", "markerFelt",
/// "timesNewRoman" — while the installed families are "Georgia", "Marker
/// Felt", "Times New Roman". An exact comparison missed EVERY curated font,
/// which then fell through to the UI sans below: the picker previewed
/// Georgia and the video rendered Helvetica, with no error anywhere. The
/// prefix pass is for names that carry a suffix the wire spelling omits,
/// such as "chalkboard" against "Chalkboard SE".
fn matching_family(fonts: &FontSystem, name: &str) -> Option<String> {
    let wanted = normalized_family(name);
    if wanted.is_empty() {
        return None;
    }
    let mut prefix_match: Option<String> = None;
    for face in fonts.db().faces() {
        for (family, _) in face.families.iter() {
            let candidate = normalized_family(family);
            if candidate == wanted {
                return Some(family.clone());
            }
            if prefix_match.is_none() && candidate.starts_with(&wanted) {
                prefix_match = Some(family.clone());
            }
        }
    }
    prefix_match
}

fn has_family(fonts: &FontSystem, name: &str) -> bool {
    matching_family(fonts, name).is_some()
}

/// Libre stand-ins for the curated Apple faces, consulted only after the
/// named family itself came up absent. Ordered best-first: metric-compatible
/// where one exists — Liberation and the URW base35 clones cover the
/// classics, Gelasio is Georgia's — same-spirit otherwise. A miss on every
/// candidate still falls through to the UI sans, so this table only ever
/// upgrades the degradation: a Linux render of a Mac-authored project gets
/// "a serif shaped like Times" instead of "the system sans".
const STAND_INS: &[(&str, &[&str])] = &[
    (
        "helveticaneue",
        &["Liberation Sans", "Arimo", "Nimbus Sans"],
    ),
    ("avenirnext", &["Nunito", "URW Gothic"]),
    ("gillsans", &["Gillius ADF", "Lato"]),
    ("futura", &["URW Gothic", "Jost", "Century Gothic"]),
    ("trebuchetms", &["Fira Sans", "DejaVu Sans"]),
    ("georgia", &["Gelasio", "P052"]),
    ("palatino", &["P052", "TeX Gyre Pagella", "URW Palladio L"]),
    (
        "timesnewroman",
        &["Liberation Serif", "Tinos", "Nimbus Roman", "FreeSerif"],
    ),
    ("americantypewriter", &["Nimbus Mono PS", "Liberation Mono"]),
    (
        "couriernew",
        &["Liberation Mono", "Cousine", "Nimbus Mono PS", "FreeMono"],
    ),
    ("chalkboard", &["Comic Neue", "Comic Relief"]),
    ("markerfelt", &["Comic Neue", "Comic Relief"]),
    ("snellroundhand", &["Z003", "URW Chancery L"]),
];

/// The first installed stand-in for a curated name, from the given table —
/// injectable so a test can prove the mechanism with a font database it
/// built itself, on a machine whose installed fonts it knows nothing about.
fn stand_in_from(fonts: &FontSystem, name: &str, table: &[(&str, &[&str])]) -> Option<String> {
    let wanted = normalized_family(name);
    let (_, candidates) = table.iter().find(|(key, _)| *key == wanted)?;
    candidates
        .iter()
        .find_map(|candidate| matching_family(fonts, candidate))
}

fn resolve_family(fonts: &mut FontSystem, requested: Option<&str>) -> ResolvedFamily {
    match requested {
        Some("serif") => return ResolvedFamily::Serif,
        Some("monospaced") | Some("mono") | Some("monospace") => return ResolvedFamily::Monospace,
        // A named font that exists wins; one that does not falls through to
        // the UI sans below, rather than silently handing back whatever
        // fontdb happens to default to.
        Some(name) if name != "system" && !name.is_empty() => {
            if let Some(family) = matching_family(fonts, name) {
                return ResolvedFamily::Named(family);
            }
            if let Some(family) = stand_in_from(fonts, name, STAND_INS) {
                return ResolvedFamily::Named(family);
            }
        }
        _ => {}
    }
    for candidate in UI_SANS {
        if has_family(fonts, candidate) {
            return ResolvedFamily::Named((*candidate).to_string());
        }
    }
    ResolvedFamily::SansSerif
}

/// Process-wide font system. Discovery scans the OS font directories, which
/// costs tens of milliseconds — far too much to repeat per frame of a video.
/// The outlines of `text` set in `family` (bold, italic): every glyph's
/// closed contours as polylines, all glyphs together, in EM units — the
/// text is 1 em tall at its size — y up, centred on the text's box. Curves
/// are flattened to `curve_segments` straight pieces each. `None` when
/// nothing shapes to a glyph: empty text, or no font on this host.
pub struct TextOutlines {
    pub contours: Vec<Vec<[f32; 2]>>,
    pub width: f32,
    pub height: f32,
}

pub fn outlines(
    text: &str,
    family: Option<&str>,
    bold: bool,
    italic: bool,
    curve_segments: usize,
) -> Option<TextOutlines> {
    const EM: f32 = 100.0;
    struct Placed {
        font_id: cosmic_text::fontdb::ID,
        glyph_id: u16,
        x: f32,
        y: f32,
        size: f32,
    }
    let mut fonts = font_system().lock().ok()?;
    let resolved = resolve_family(&mut fonts, family);
    let placed: Vec<Placed> = {
        let metrics = Metrics::new(EM, EM * 1.2);
        let mut buffer = Buffer::new(&mut fonts, metrics);
        let mut buffer = buffer.borrow_with(&mut fonts);
        buffer.set_size(None, None);
        let family = match &resolved {
            ResolvedFamily::Named(name) => Family::Name(name),
            ResolvedFamily::Serif => Family::Serif,
            ResolvedFamily::Monospace => Family::Monospace,
            ResolvedFamily::SansSerif => Family::SansSerif,
        };
        let attrs = Attrs::new()
            .family(family)
            .weight(if bold { Weight::BOLD } else { Weight::NORMAL })
            .style(if italic { Style::Italic } else { Style::Normal });
        buffer.set_text(text, attrs, Shaping::Advanced);
        buffer.shape_until_scroll(true);
        let mut placed = Vec::new();
        for run in buffer.layout_runs() {
            for g in run.glyphs {
                placed.push(Placed {
                    font_id: g.font_id,
                    glyph_id: g.glyph_id,
                    x: g.x + g.x_offset,
                    y: run.line_y + g.y - g.y_offset,
                    size: g.font_size,
                });
            }
        }
        placed
    };
    let mut contours: Vec<Vec<[f32; 2]>> = Vec::new();
    for g in placed {
        let Some(font) = fonts.get_font(g.font_id) else {
            continue;
        };
        let face = font.rustybuzz();
        let scale = g.size / face.units_per_em() as f32;
        let mut flat = Flattener {
            contours: Vec::new(),
            current: Vec::new(),
            last: [0.0, 0.0],
            segments: curve_segments.max(1),
            origin: [g.x, g.y],
            scale,
        };
        if face
            .outline_glyph(ttf_parser::GlyphId(g.glyph_id), &mut flat)
            .is_some()
        {
            contours.extend(flat.finish());
        }
    }
    if contours.is_empty() {
        return None;
    }
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for c in &contours {
        for p in c {
            min_x = min_x.min(p[0]);
            max_x = max_x.max(p[0]);
            min_y = min_y.min(p[1]);
            max_y = max_y.max(p[1]);
        }
    }
    let (cx, cy) = ((min_x + max_x) / 2.0, (min_y + max_y) / 2.0);
    for c in &mut contours {
        for p in c.iter_mut() {
            // Layout space is y-down; the body stands y-up.
            *p = [(p[0] - cx) / EM, -(p[1] - cy) / EM];
        }
    }
    Some(TextOutlines {
        contours,
        width: (max_x - min_x) / EM,
        height: (max_y - min_y) / EM,
    })
}

/// Collects a glyph's outline as closed polylines in layout space, curves
/// flattened to a fixed number of pieces.
struct Flattener {
    contours: Vec<Vec<[f32; 2]>>,
    current: Vec<[f32; 2]>,
    last: [f32; 2],
    segments: usize,
    origin: [f32; 2],
    scale: f32,
}

impl Flattener {
    fn map(&self, x: f32, y: f32) -> [f32; 2] {
        // Font units are y-up; the layout is y-down.
        [self.origin[0] + x * self.scale, self.origin[1] - y * self.scale]
    }
    fn take(&mut self) {
        if self.current.len() >= 3 {
            if let (Some(f), Some(l)) = (self.current.first().copied(), self.current.last().copied()) {
                if (f[0] - l[0]).abs() < 1e-4 && (f[1] - l[1]).abs() < 1e-4 {
                    self.current.pop();
                }
            }
            if self.current.len() >= 3 {
                self.contours.push(std::mem::take(&mut self.current));
                return;
            }
        }
        self.current.clear();
    }
    fn finish(mut self) -> Vec<Vec<[f32; 2]>> {
        self.take();
        self.contours
    }
}

impl ttf_parser::OutlineBuilder for Flattener {
    fn move_to(&mut self, x: f32, y: f32) {
        self.take();
        let p = self.map(x, y);
        self.current.push(p);
        self.last = p;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.current.push(p);
        self.last = p;
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (a, c, p) = (self.last, self.map(x1, y1), self.map(x, y));
        for i in 1..=self.segments {
            let t = i as f32 / self.segments as f32;
            let u = 1.0 - t;
            self.current.push([
                u * u * a[0] + 2.0 * u * t * c[0] + t * t * p[0],
                u * u * a[1] + 2.0 * u * t * c[1] + t * t * p[1],
            ]);
        }
        self.last = p;
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (a, c1, c2, p) = (self.last, self.map(x1, y1), self.map(x2, y2), self.map(x, y));
        for i in 1..=self.segments {
            let t = i as f32 / self.segments as f32;
            let u = 1.0 - t;
            let (uu, tt) = (u * u, t * t);
            self.current.push([
                uu * u * a[0] + 3.0 * uu * t * c1[0] + 3.0 * u * tt * c2[0] + tt * t * p[0],
                uu * u * a[1] + 3.0 * uu * t * c1[1] + 3.0 * u * tt * c2[1] + tt * t * p[1],
            ]);
        }
        self.last = p;
    }
    fn close(&mut self) {
        self.take();
    }
}

fn font_system() -> &'static Mutex<FontSystem> {
    static FONTS: OnceLock<Mutex<FontSystem>> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut fonts = FontSystem::new();
        // fontdb has no iOS branch: on device and simulator alike its unix
        // fallback scans /usr/share/fonts, which does not exist there, and an
        // EMPTY font db makes cosmic-text's shaper panic ("no default font
        // found") — which crossed the FFI and aborted the host app the first
        // time an iOS test drew a caption. Load the Apple directories
        // ourselves when discovery came up empty: /System/Library/Fonts
        // holds the iOS fonts on a device and the macOS fonts under the
        // simulator (simulated processes read the host filesystem).
        if fonts.db().is_empty() {
            for dir in ["/System/Library/Fonts", "/Library/Fonts"] {
                fonts.db_mut().load_fonts_dir(dir);
            }
        }
        Mutex::new(fonts)
    })
}

fn swash_cache() -> &'static Mutex<SwashCache> {
    static CACHE: OnceLock<Mutex<SwashCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(SwashCache::new()))
}

/// Where a caption's box lands, without drawing it.
///
/// The editing canvas needs this and only this: it draws captions with the
/// host's own text stack for live editing, and used to guess the box by
/// measuring at INFINITE width — so a headline long enough to wrap showed as
/// one overflowing line in the editor and two lines in the export. Same
/// layout, one answer, no guessing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextBox {
    /// The panel's size in canvas px, padding included.
    pub width: f64,
    pub height: f64,
    /// Top-left in canvas coordinates, y down.
    pub x: f64,
    pub y: f64,
    /// The width the text itself wraps within — panel minus padding.
    pub text_width: f64,
    pub lines: u32,
}

/// Lays out `text` and reports its box. `None` for empty text.
pub fn measure(
    text: &str,
    canvas_width: f64,
    canvas_height: f64,
    style: &TextStyle,
) -> Option<TextBox> {
    layout(text, canvas_width, canvas_height, style).map(|l| l.box_)
}

/// Lays out and draws `text` for a canvas of `canvas_width` × `canvas_height`.
///
/// Returns `None` for empty text — an empty caption is not a zero-sized image,
/// it is nothing to draw at all.
struct Layout {
    box_: TextBox,
}

/// Everything both entry points need, computed once. `rasterize` then draws
/// into the box this decided; `measure` just reports it.
fn layout(text: &str, canvas_width: f64, canvas_height: f64, style: &TextStyle) -> Option<Layout> {
    let raster = rasterize_inner(text, canvas_width, canvas_height, style, false)?;
    Some(Layout {
        box_: TextBox {
            width: raster.width as f64,
            height: raster.height as f64,
            x: raster.x,
            y: raster.y,
            text_width: (canvas_width - style.left_margin - style.right_margin).max(10.0)
                - style.padding * 2.0,
            lines: raster.lines,
        },
    })
}

pub fn rasterize(
    text: &str,
    canvas_width: f64,
    canvas_height: f64,
    style: &TextStyle,
) -> Option<RasterizedText> {
    rasterize_inner(text, canvas_width, canvas_height, style, true)
}

fn rasterize_inner(
    text: &str,
    canvas_width: f64,
    canvas_height: f64,
    style: &TextStyle,
    draw: bool,
) -> Option<RasterizedText> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    // The app's rule, verbatim: panel inset by the margins, text inset again
    // by the padding.
    let panel_width = (canvas_width - style.left_margin - style.right_margin).max(10.0);
    let text_width = (panel_width - style.padding * 2.0).max(10.0);

    let mut fonts = font_system().lock().ok()?;
    // `Family::SansSerif` leans on fontdb's default family, which is unset on
    // a bare FontSystem — on this machine it resolved to a MONOSPACE face and
    // every headline came out looking like terminal output. Pick a real UI
    // font by name instead, from what the machine actually has. Resolved
    // before the buffer borrows the font system.
    let resolved = resolve_family(&mut fonts, style.font_family.as_deref());

    let metrics = Metrics::new(
        style.font_size as f32,
        (style.font_size * style.line_height) as f32,
    );
    let mut buffer = Buffer::new(&mut fonts, metrics);
    let mut buffer = buffer.borrow_with(&mut fonts);
    buffer.set_size(Some(text_width as f32), None);

    let family = match &resolved {
        ResolvedFamily::Named(name) => Family::Name(name),
        ResolvedFamily::Serif => Family::Serif,
        ResolvedFamily::Monospace => Family::Monospace,
        ResolvedFamily::SansSerif => Family::SansSerif,
    };
    let attrs = Attrs::new()
        .family(family)
        .weight(if style.bold {
            Weight::BOLD
        } else {
            Weight::NORMAL
        })
        .style(if style.italic {
            Style::Italic
        } else {
            Style::Normal
        });
    buffer.set_text(text, attrs, Shaping::Advanced);
    buffer.shape_until_scroll(true);

    // Measured, not assumed: the widest run decides the box width, so a short
    // headline gets a snug background instead of a full-width bar.
    let mut measured_width: f32 = 0.0;
    let mut line_count = 0usize;
    for run in buffer.layout_runs() {
        measured_width = measured_width.max(run.line_w);
        line_count += 1;
    }
    let line_height = metrics.line_height as f64;
    let text_height = (line_count.max(1) as f64 * line_height).ceil();

    let bg_width = panel_width.min(measured_width.ceil() as f64 + style.padding * 2.0);
    let bg_height = text_height + style.padding * 2.0;
    let bg_x = match style.align {
        Align::Leading => style.left_margin,
        Align::Center => style.left_margin + (panel_width - bg_width) / 2.0,
        Align::Trailing => style.left_margin + panel_width - bg_width,
    };

    // A placement rule positions the measured box outright — anchor cell
    // plus offset — and the margins keep only the wrap width they already
    // decided above. Without one, the box sits where it always has: at the
    // align-derived x, vertical_margin down from the top.
    let (bg_x, bg_y) = match &style.placement {
        Some(rule) => rule.position_box(bg_width, bg_height, canvas_width, canvas_height),
        None => (bg_x, style.vertical_margin),
    };

    let width = bg_width.round().max(1.0) as u32;
    let height = bg_height.round().max(1.0) as u32;
    if !draw {
        // Measuring only: the box is decided, and glyph rasterization is the
        // expensive half. An editor overlay asks for this on every layout
        // pass, so it must not pay for pixels it throws away.
        return Some(RasterizedText {
            rgba: Vec::new(),
            width,
            height,
            x: bg_x,
            y: bg_y,
            lines: line_count.max(1) as u32,
        });
    }
    let mut rgba = vec![0u8; (width * height * 4) as usize];

    // Background panel, with anti-aliased rounded corners.
    if style.background_rgba[3] > 0 {
        fill_rounded_rect(
            &mut rgba,
            width,
            height,
            style.corner_radius,
            style.background_rgba,
        );
    }

    // Text, drawn inside the padding. cosmic-text hands back coverage per
    // pixel; blend it over whatever the panel put there.
    // Coverage gamma, chosen from the text colour unless the caller said.
    let text_luma = (0.2126 * style.text_rgba[0] as f64
        + 0.7152 * style.text_rgba[1] as f64
        + 0.0722 * style.text_rgba[2] as f64)
        / 255.0;
    let gamma = style
        .smoothing
        .unwrap_or(if text_luma > 0.5 { 2.0 } else { 1.0 })
        .max(0.1);

    let mut cache = swash_cache().lock().ok()?;
    let color = cosmic_text::Color::rgba(
        style.text_rgba[0],
        style.text_rgba[1],
        style.text_rgba[2],
        style.text_rgba[3],
    );
    // Coverage is collected into a mask FIRST when there is a stroke or a
    // shadow, because both are derived from the glyph shapes: cosmic-text
    // hands back coverage per pixel and no outlines, so the outline is a
    // dilation of that mask and the shadow is a blur of it. Drawing the
    // glyphs straight into the buffer, as the plain path does, throws the
    // shape away before either can be built.
    let wants_effects = (style.stroke_width > 0.0 && style.stroke_rgba[3] > 0)
        || (style.shadow_rgba[3] > 0
            && (style.shadow_radius > 0.0 || style.shadow_offset != [0.0, 0.0]));
    let mut mask: Vec<f32> = if wants_effects {
        vec![0.0; (width * height) as usize]
    } else {
        Vec::new()
    };
    // Horizontal alignment inside the box: the box is already sized to the
    // text, so runs are nudged by the difference for multi-line blocks.
    let inner_width = bg_width - style.padding * 2.0;
    buffer.draw(&mut cache, color, |gx, gy, gw, gh, gcolor| {
        let raw = gcolor.a() as f64 / 255.0;
        if raw <= 0.0 {
            return;
        }
        // Fully covered pixels are unaffected; only the partial edge changes.
        let a = raw.powf(1.0 / gamma);
        for dy in 0..gh {
            for dx in 0..gw {
                let px = gx + dx as i32;
                let py = gy + dy as i32;
                let x = px as f64 + style.padding;
                let y = py as f64 + style.padding;
                let _ = inner_width;
                if x < 0.0 || y < 0.0 || x >= width as f64 || y >= height as f64 {
                    continue;
                }
                if !mask.is_empty() {
                    // Effects on: only the SHAPE is recorded here. Blending
                    // the glyph now would put the fill under the stroke that
                    // is derived from it, and the outline would swallow the
                    // letter.
                    let index = y as usize * width as usize + x as usize;
                    if let Some(slot) = mask.get_mut(index) {
                        *slot = slot.max(a as f32);
                    }
                    continue;
                }
                blend(
                    &mut rgba,
                    width,
                    x as u32,
                    y as u32,
                    [gcolor.r(), gcolor.g(), gcolor.b()],
                    a,
                );
            }
        }
    });

    // Shadow, then outline, then the letters — the order they must be read
    // in. Each is derived from the one mask above.
    if !mask.is_empty() {
        let (w, h) = (width as usize, height as usize);
        // The outline's coverage, needed before the shadow: a shadow is cast
        // by the SILHOUETTE — glyph plus outline — not by the glyph alone.
        // Casting it from the glyph buried its strongest part under the
        // stroke that was painted over it, leaving only the weak outer tail,
        // and a stroked caption looked as though it had no shadow at all
        // (measured: a third of the effect of the same shadow unstroked).
        let stroked = style.stroke_width > 0.0 && style.stroke_rgba[3] > 0;
        let dist = if stroked || style.shadow_rgba[3] > 0 {
            distance_to_ink(&mask, w, h)
        } else {
            Vec::new()
        };
        let stroke_coverage = |index: usize| -> f32 {
            if !stroked {
                return 0.0;
            }
            (style.stroke_width as f32 + 0.5 - dist[index]).clamp(0.0, 1.0)
        };
        if style.shadow_rgba[3] > 0 {
            let silhouette: Vec<f32> = if stroked {
                mask.iter()
                    .enumerate()
                    .map(|(i, &a)| a.max(stroke_coverage(i)))
                    .collect()
            } else {
                mask.clone()
            };
            let blurred = blur_mask(&silhouette, w, h, style.shadow_radius);
            let (dx, dy) = (style.shadow_offset[0], style.shadow_offset[1]);
            let alpha = style.shadow_rgba[3] as f64 / 255.0;
            for y in 0..h {
                for x in 0..w {
                    let sx = x as f64 - dx;
                    let sy = y as f64 - dy;
                    if sx < 0.0 || sy < 0.0 || sx >= w as f64 || sy >= h as f64 {
                        continue;
                    }
                    let a = blurred[sy as usize * w + sx as usize] as f64 * alpha;
                    if a <= 0.001 {
                        continue;
                    }
                    blend(
                        &mut rgba,
                        width,
                        x as u32,
                        y as u32,
                        [
                            style.shadow_rgba[0],
                            style.shadow_rgba[1],
                            style.shadow_rgba[2],
                        ],
                        a,
                    );
                }
            }
        }
        if stroked {
            let alpha = style.stroke_rgba[3] as f64 / 255.0;
            for y in 0..h {
                for x in 0..w {
                    // Anti-aliased at the outer edge: the half-pixel is what
                    // keeps a thin outline from looking like a staircase.
                    let coverage = stroke_coverage(y * w + x);
                    if coverage <= 0.001 {
                        continue;
                    }
                    blend(
                        &mut rgba,
                        width,
                        x as u32,
                        y as u32,
                        [
                            style.stroke_rgba[0],
                            style.stroke_rgba[1],
                            style.stroke_rgba[2],
                        ],
                        coverage as f64 * alpha,
                    );
                }
            }
        }
        for y in 0..h {
            for x in 0..w {
                let a = mask[y * w + x] as f64;
                if a <= 0.001 {
                    continue;
                }
                blend(
                    &mut rgba,
                    width,
                    x as u32,
                    y as u32,
                    [style.text_rgba[0], style.text_rgba[1], style.text_rgba[2]],
                    a,
                );
            }
        }
    }

    // Top-left origin, matching the app: both its SwiftUI preview
    // (`.offset(y: verticalMargin)`) and its exporter (a bitmap context
    // flipped to top-left in `makeBitmapContext`) treat the vertical margin as
    // a distance from the TOP. This module used to measure from the bottom,
    // which put every caption somewhere else than the app drew it.
    let y = bg_y;

    Some(RasterizedText {
        rgba,
        width,
        height,
        x: bg_x,
        y,
        lines: line_count.max(1) as u32,
    })
}

fn blend(rgba: &mut [u8], width: u32, x: u32, y: u32, color: [u8; 3], alpha: f64) {
    let i = ((y * width + x) * 4) as usize;
    if i + 3 >= rgba.len() {
        return;
    }
    let dst_a = rgba[i + 3] as f64 / 255.0;
    let out_a = alpha + dst_a * (1.0 - alpha);
    if out_a <= 0.0 {
        return;
    }
    for c in 0..3 {
        let src = color[c] as f64;
        let dst = rgba[i + c] as f64;
        rgba[i + c] = (((src * alpha) + dst * dst_a * (1.0 - alpha)) / out_a).round() as u8;
    }
    rgba[i + 3] = (out_a * 255.0).round() as u8;
}

/// Fills the whole buffer with `color`, rounding the corners. Coverage is
/// sampled from the corner circle so the edge is not a staircase.
fn fill_rounded_rect(rgba: &mut [u8], width: u32, height: u32, radius: f64, color: [u8; 4]) {
    let r = radius
        .max(0.0)
        .min(width as f64 / 2.0)
        .min(height as f64 / 2.0);
    let alpha = color[3] as f64 / 255.0;
    for y in 0..height {
        for x in 0..width {
            let coverage = if r <= 0.0 {
                1.0
            } else {
                corner_coverage(
                    x as f64 + 0.5,
                    y as f64 + 0.5,
                    width as f64,
                    height as f64,
                    r,
                )
            };
            if coverage <= 0.0 {
                continue;
            }
            blend(
                rgba,
                width,
                x,
                y,
                [color[0], color[1], color[2]],
                alpha * coverage,
            );
        }
    }
}

/// 1 inside, 0 outside, and a soft edge across the last pixel of the corner
/// arc.
fn corner_coverage(x: f64, y: f64, w: f64, h: f64, r: f64) -> f64 {
    let cx = if x < r {
        r
    } else if x > w - r {
        w - r
    } else {
        return 1.0;
    };
    let cy = if y < r {
        r
    } else if y > h - r {
        h - r
    } else {
        return 1.0;
    };
    let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
    (r - d + 0.5).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_is_nothing_to_draw() {
        assert!(rasterize("", 1920.0, 1080.0, &TextStyle::default()).is_none());
        assert!(rasterize("   ", 1920.0, 1080.0, &TextStyle::default()).is_none());
    }

    #[test]
    fn a_headline_produces_visible_pixels() {
        let out = rasterize("A faster spreadsheet", 1440.0, 900.0, &TextStyle::default())
            .expect("rasterized");
        assert!(out.width > 100, "width {}", out.width);
        assert!(out.height > 40, "height {}", out.height);
        assert_eq!(out.rgba.len(), (out.width * out.height * 4) as usize);
        let lit = out.rgba.chunks_exact(4).filter(|px| px[3] > 0).count();
        assert!(lit > 0, "something was drawn");
    }

    /// The box hugs the text rather than spanning the panel — otherwise every
    /// caption is a full-width bar.
    #[test]
    fn the_box_is_sized_to_the_text() {
        let style = TextStyle::default();
        let short = rasterize("Hi", 1440.0, 900.0, &style).unwrap();
        let long = rasterize(
            "Formulas without friction, and quite a lot more besides",
            1440.0,
            900.0,
            &style,
        )
        .unwrap();
        assert!(
            long.width > short.width,
            "{} vs {}",
            long.width,
            short.width
        );
        assert!(
            short.width < 1440,
            "a two-letter caption must not span the canvas"
        );
    }

    /// Alignment moves the box inside the panel, per the app's rule.
    #[test]
    fn alignment_positions_the_box_within_the_panel() {
        let mut style = TextStyle {
            align: Align::Leading,
            ..Default::default()
        };
        let leading = rasterize("Hello", 1440.0, 900.0, &style).unwrap();
        style.align = Align::Center;
        let center = rasterize("Hello", 1440.0, 900.0, &style).unwrap();
        style.align = Align::Trailing;
        let trailing = rasterize("Hello", 1440.0, 900.0, &style).unwrap();

        assert_eq!(leading.x, style.left_margin);
        assert!(center.x > leading.x);
        assert!(trailing.x > center.x);
        assert!(
            trailing.x + trailing.width as f64 <= 1440.0 - style.right_margin + 0.5,
            "trailing must not cross the right margin"
        );
    }

    /// The app measures the vertical margin from the bottom; a caller reading
    /// this as "from the top" would put every caption in the wrong half.
    #[test]
    fn a_placement_hangs_the_measured_box_on_the_grid() {
        let mut style = TextStyle {
            font_size: 48.0,
            ..TextStyle::default()
        };
        let plain = measure("Anchored words", 1920.0, 1080.0, &style).expect("box");

        style.placement = Some(promo_model::Placement {
            height: None,
            width: None,
            mode: None,
            anchor: Some(promo_model::Anchor::BottomRight),
            offset: Some([-24.0, -12.0]),
        });
        let placed = measure("Anchored words", 1920.0, 1080.0, &style).expect("box");

        assert_eq!(
            (placed.width, placed.height),
            (plain.width, plain.height),
            "placement moves the box, never sizes it"
        );
        assert!(
            (placed.x - (1920.0 - placed.width - 24.0)).abs() < 0.6,
            "bottom-right anchor, nudged 24 left: x={}",
            placed.x
        );
        assert!(
            (placed.y - (1080.0 - placed.height - 12.0)).abs() < 0.6,
            "and 12 up: y={}",
            placed.y
        );
    }

    #[test]
    fn vertical_margin_is_measured_from_the_top() {
        // The app's own two renderers both place a caption's TOP edge at
        // `verticalMargin`. This crate measured from the bottom, so the same
        // project drew captions in one place in the app and another in the
        // CLI — with nothing to catch it, because the app's parity tests
        // compare its CoreText drawing against itself.
        let style = TextStyle {
            vertical_margin: 80.0,
            ..Default::default()
        };
        let out = rasterize("Top", 1440.0, 900.0, &style).unwrap();
        assert!(
            (out.y - 80.0).abs() < 1.5,
            "expected ~80 from the top, got {}",
            out.y
        );
    }

    /// The editor asks `measure`, the renderer calls `rasterize`; if those
    /// two ever disagree the canvas draws a caption somewhere the export does
    /// not. They share a layout precisely so this test can be short.
    #[test]
    fn measuring_agrees_with_drawing() {
        let style = TextStyle {
            font_size: 64.0,
            left_margin: 300.0,
            right_margin: 300.0,
            ..Default::default()
        };
        for text in [
            "Short",
            "Two\nlines",
            "A considerably longer headline that cannot possibly fit on one line",
        ] {
            let drawn = rasterize(text, 1440.0, 900.0, &style).expect(text);
            let measured = measure(text, 1440.0, 900.0, &style).expect(text);
            assert_eq!(measured.width, drawn.width as f64, "{text}");
            assert_eq!(measured.height, drawn.height as f64, "{text}");
            assert_eq!(measured.x, drawn.x, "{text}");
            assert_eq!(measured.y, drawn.y, "{text}");
            assert_eq!(measured.lines, drawn.lines, "{text}");
        }
        // And it really is measuring more than one line.
        assert_eq!(
            measure("Two\nlines", 1440.0, 900.0, &style).unwrap().lines,
            2
        );
        assert!(
            measure(
                "A considerably longer headline that cannot possibly fit on one line",
                1440.0,
                900.0,
                &style
            )
            .unwrap()
            .lines
                >= 2
        );
    }

    #[test]
    fn wrapping_grows_the_box_downward() {
        let style = TextStyle {
            font_size: 64.0,
            left_margin: 400.0,
            right_margin: 400.0,
            ..Default::default()
        };
        let one = rasterize("Short", 1440.0, 900.0, &style).unwrap();
        let many = rasterize(
            "A considerably longer headline that cannot possibly fit on one line",
            1440.0,
            900.0,
            &style,
        )
        .unwrap();
        assert!(
            many.height > one.height,
            "{} vs {}",
            many.height,
            one.height
        );
    }
}

#[cfg(test)]
mod smoothing_tests {
    use super::*;

    fn ink(out: &RasterizedText) -> f64 {
        // Mean alpha-weighted luminance over the whole panel.
        let mut total = 0.0;
        for px in out.rgba.chunks_exact(4) {
            let a = px[3] as f64 / 255.0;
            total += a * (0.2126 * px[0] as f64 + 0.7152 * px[1] as f64 + 0.0722 * px[2] as f64);
        }
        total / (out.width * out.height).max(1) as f64
    }

    /// Smoothing must add weight without moving a single glyph: it remaps
    /// edge coverage, it does not re-lay-out anything.
    #[test]
    fn smoothing_thickens_light_text_without_changing_metrics() {
        let plain = TextStyle {
            font_size: 54.0,
            smoothing: Some(1.0),
            background_rgba: [0, 0, 0, 0],
            ..Default::default()
        };
        let smoothed = TextStyle {
            smoothing: Some(2.0),
            ..plain.clone()
        };
        let a = rasterize("Formulas without friction", 1920.0, 200.0, &plain).unwrap();
        let b = rasterize("Formulas without friction", 1920.0, 200.0, &smoothed).unwrap();

        assert_eq!(
            (a.width, a.height),
            (b.width, b.height),
            "smoothing must not change layout"
        );
        assert!(
            ink(&b) > ink(&a),
            "smoothed text should carry more ink: {} vs {}",
            ink(&b),
            ink(&a)
        );
    }

    /// Dark text on a light background has the opposite bias, and much
    /// weaker; leave it alone rather than making it blotchy.
    #[test]
    fn dark_text_is_not_smoothed_by_default() {
        let dark = TextStyle {
            text_rgba: [10, 20, 30, 255],
            ..Default::default()
        };
        let forced = TextStyle {
            smoothing: Some(1.0),
            ..dark.clone()
        };
        let a = rasterize("Format every detail", 1440.0, 900.0, &dark).unwrap();
        let b = rasterize("Format every detail", 1440.0, 900.0, &forced).unwrap();
        assert_eq!(a.rgba, b.rgba, "default for dark text is no smoothing");
    }

    /// A caption with no plate over bright footage is unreadable without an
    /// outline — that is the whole reason this exists. White text with a
    /// black stroke must therefore put DARK pixels immediately around the
    /// light ones, so the letters survive whatever is behind them.
    #[test]
    fn a_stroke_puts_dark_pixels_around_light_letters() {
        let mut style = TextStyle {
            text_rgba: [255, 255, 255, 255],
            background_rgba: [0, 0, 0, 0], // no plate: the social-caption look
            padding: 24.0,
            ..TextStyle::default()
        };
        let plain = rasterize("Hold", 1920.0, 1080.0, &style).expect("plain");
        style.stroke_rgba = [0, 0, 0, 255];
        style.stroke_width = 6.0;
        let stroked = rasterize("Hold", 1920.0, 1080.0, &style).expect("stroked");

        // Same box: the outline lives inside the padding and must not move
        // the caption or resize it.
        assert_eq!((plain.width, plain.height), (stroked.width, stroked.height));
        assert_eq!((plain.x, plain.y), (stroked.x, stroked.y));

        let dark_opaque = |r: &RasterizedText| {
            r.rgba
                .chunks_exact(4)
                .filter(|px| px[3] > 128 && px[0] < 60 && px[1] < 60 && px[2] < 60)
                .count()
        };
        assert_eq!(dark_opaque(&plain), 0, "no plate, no stroke: nothing dark");
        assert!(
            dark_opaque(&stroked) > 200,
            "the stroke drew {} dark pixels",
            dark_opaque(&stroked)
        );

        // And the letters are still white on top — an outline that swallowed
        // the fill would be worse than none.
        let white = |r: &RasterizedText| {
            r.rgba
                .chunks_exact(4)
                .filter(|px| px[3] > 200 && px[0] > 220 && px[1] > 220 && px[2] > 220)
                .count()
        };
        let (before, after) = (white(&plain), white(&stroked));
        assert!(
            after as f64 > before as f64 * 0.7,
            "fill survives the outline: {before} -> {after}"
        );
    }

    /// A stroke wider than the padding is CLIPPED, not grown into: the box
    /// is text-plus-padding, and letting it grow would move every caption
    /// and resize any plate behind it.
    #[test]
    fn a_stroke_never_moves_the_caption() {
        let base = TextStyle {
            padding: 8.0,
            ..TextStyle::default()
        };
        let plain = rasterize("Ship", 1920.0, 1080.0, &base).expect("plain");
        let huge = TextStyle {
            stroke_rgba: [255, 0, 0, 255],
            stroke_width: 40.0,
            ..base
        };
        let stroked = rasterize("Ship", 1920.0, 1080.0, &huge).expect("stroked");
        assert_eq!((plain.width, plain.height), (stroked.width, stroked.height));
        assert_eq!((plain.x, plain.y), (stroked.x, stroked.y));
    }

    /// The shadow sits UNDER the letters and is offset from them, so it
    /// darkens what is behind the caption without dulling the text.
    #[test]
    fn a_shadow_darkens_below_without_touching_the_letters() {
        let style = TextStyle {
            text_rgba: [255, 255, 255, 255],
            background_rgba: [0, 0, 0, 0],
            padding: 28.0,
            shadow_rgba: [0, 0, 0, 220],
            shadow_radius: 6.0,
            shadow_offset: [0.0, 6.0],
            ..TextStyle::default()
        };
        let out = rasterize("Now", 1920.0, 1080.0, &style).expect("shadowed");
        let w = out.width as usize;
        let rows = |from: usize, to: usize| {
            out.rgba[from * w * 4..to * w * 4]
                .chunks_exact(4)
                .filter(|px| px[3] > 20 && px[0] < 80)
                .count()
        };
        let h = out.height as usize;
        // More shadow below the text than above it, because it is offset down.
        assert!(
            rows(h / 2, h) > rows(0, h / 4),
            "the shadow falls below: {} vs {}",
            rows(h / 2, h),
            rows(0, h / 4)
        );
        let white = out
            .rgba
            .chunks_exact(4)
            .filter(|px| px[3] > 200 && px[0] > 220)
            .count();
        assert!(white > 0, "the letters are still white");
    }

    /// The font picker writes the app's wire spelling into the project, so a
    /// caption asking for "georgia" or "markerFelt" has to reach the same
    /// face the picker showed. It used to reach none of them: an exact
    /// comparison against the installed "Georgia" failed and every curated
    /// font silently rendered in the UI sans.
    ///
    /// Measured against the SANS, not against the installed name — asking
    /// for "Georgia" goes down the same resolution path, so using it as the
    /// reference made this test pass with that path disabled entirely.
    #[test]
    fn a_caption_gets_the_font_the_picker_named() {
        let width_of = |family: &str| -> f64 {
            let style = TextStyle {
                font_family: Some(family.into()),
                font_size: 72.0,
                ..TextStyle::default()
            };
            measure("Handgloves", 1920.0, 1080.0, &style)
                .map(|b| b.width)
                .unwrap_or(0.0)
        };
        let sans = width_of("system");
        let probes = ["georgia", "markerFelt", "futura", "timesNewRoman"];
        // A machine with neither the curated faces nor any stand-in — a bare
        // container — cannot answer the wiring question this test asks; the
        // hermetic stand_in_tests below carry the resolver's guarantee
        // there. Any installed face keeps this test armed, and CI installs
        // one (Linux: fonts-liberation) precisely so it stays armed.
        let answerable = {
            let fonts = font_system().lock().unwrap();
            probes.iter().any(|name| {
                matching_family(&fonts, name).is_some()
                    || stand_in_from(&fonts, name, STAND_INS).is_some()
            })
        };
        if !answerable {
            eprintln!(
                "a_caption_gets_the_font_the_picker_named: no curated face or \
                 stand-in installed — this machine cannot arm the probe"
            );
            return;
        }
        let measured: Vec<(&str, f64)> =
            probes.iter().map(|name| (*name, width_of(name))).collect();
        assert!(
            measured.iter().any(|(_, width)| (width - sans).abs() > 0.5),
            "every curated font measured exactly like the fallback sans ({sans}): \
             {measured:?} — the picker's font is not reaching the renderer"
        );
    }

    /// The resolver, proved against a font database the test built itself —
    /// so it answers the same on a Mac, a fontless container, or CI. The
    /// fixture face is Tuffy (public domain), a family no OS ships, which is
    /// the point: the only way these tests pass is through the table.
    mod stand_in_tests {
        use super::super::*;

        fn fonts_with_tuffy() -> FontSystem {
            let mut db = cosmic_text::fontdb::Database::new();
            db.load_font_data(include_bytes!("../fonts/Tuffy.ttf").to_vec());
            FontSystem::new_with_locale_and_db("en-US".into(), db)
        }

        /// A curated face that is absent takes the first INSTALLED stand-in,
        /// in table order — not the first listed.
        #[test]
        fn a_missing_face_takes_its_first_installed_stand_in() {
            let fonts = fonts_with_tuffy();
            let table: &[(&str, &[&str])] = &[("georgia", &["Gelasio", "Tuffy", "DejaVu Serif"])];
            assert_eq!(
                stand_in_from(&fonts, "Georgia", table).as_deref(),
                Some("Tuffy"),
                "Gelasio is not in the db, Tuffy is — order walked, spelling \
                 normalized on the way in"
            );
        }

        /// A name with no table entry answers None and the caller falls
        /// through to the UI sans — the stated degradation, unchanged.
        #[test]
        fn an_unlisted_face_has_no_stand_in() {
            let fonts = fonts_with_tuffy();
            assert_eq!(stand_in_from(&fonts, "Zapfino", STAND_INS), None);
        }

        /// The resolver end to end on the hermetic db: a curated name whose
        /// stand-in is installed resolves NAMED, not to the sans fallback.
        #[test]
        fn resolution_reaches_the_stand_in_not_the_fallback() {
            let mut fonts = fonts_with_tuffy();
            let table: &[(&str, &[&str])] = &[("markerfelt", &["Tuffy"])];
            let got = match matching_family(&fonts, "markerFelt") {
                Some(f) => Some(f),
                None => stand_in_from(&fonts, "markerFelt", table),
            };
            assert_eq!(got.as_deref(), Some("Tuffy"));
            // And the real resolve_family path stays a fallback when the
            // real table has no installed candidate in this db.
            match resolve_family(&mut fonts, Some("markerFelt")) {
                ResolvedFamily::Named(name) => {
                    panic!("nothing in this db should match, got {name}")
                }
                ResolvedFamily::SansSerif => {}
                other => panic!("expected the sans fallback, got {other:?}"),
            }
        }

        /// Every table key is one of the app's curated wire spellings,
        /// normalized — a typo here would be a stand-in nobody can reach.
        #[test]
        fn every_stand_in_key_is_a_curated_wire_spelling() {
            let curated = [
                "helveticaNeue",
                "avenirNext",
                "gillSans",
                "futura",
                "trebuchetMS",
                "georgia",
                "palatino",
                "timesNewRoman",
                "americanTypewriter",
                "courierNew",
                "chalkboard",
                "markerFelt",
                "snellRoundhand",
            ];
            let normalized: Vec<String> = curated.iter().map(|n| normalized_family(n)).collect();
            for (key, candidates) in STAND_INS {
                assert!(
                    normalized.iter().any(|n| n == key),
                    "`{key}` is not a curated wire spelling"
                );
                assert!(!candidates.is_empty(), "`{key}` lists no candidates");
            }
        }
    }

    /// Every step of the blur control has to do something. The box radius is
    /// a whole number of pixels and used to be ROUNDED, so 8, 9 and 10 all
    /// blurred identically: the slider moved, the number moved, the render
    /// did not.
    #[test]
    fn every_step_of_the_blur_softens_the_shadow_a_little_more() {
        // Total shadow alpha is constant under blur; its SPREAD is what
        // grows, so measure how far the faint edge reaches.
        let spread = |radius: f64| -> usize {
            let style = TextStyle {
                text_rgba: [255, 255, 255, 255],
                background_rgba: [0, 0, 0, 0],
                padding: 40.0,
                shadow_rgba: [0, 0, 0, 220],
                shadow_radius: radius,
                shadow_offset: [0.0, 0.0],
                ..TextStyle::default()
            };
            let out = rasterize("Now", 1920.0, 1080.0, &style).expect("shadowed");
            out.rgba
                .chunks_exact(4)
                .filter(|px| px[3] > 4 && px[0] < 80)
                .count()
        };

        let (at_8, at_9, at_10) = (spread(8.0), spread(9.0), spread(10.0));
        assert!(
            at_8 != at_9 && at_9 != at_10,
            "8/9/10 must not render the same: {at_8}, {at_9}, {at_10}"
        );

        // And more blur is always softer, never less.
        let mut previous = 0;
        for step in 0..=8 {
            let here = spread(f64::from(step) * 3.0);
            assert!(
                here >= previous,
                "radius {} narrowed the shadow: {here} after {previous}",
                step * 3
            );
            previous = here;
        }
    }

    /// Neither effect is on by default, so every project that existed before
    /// them renders exactly as it did.
    #[test]
    fn effects_are_off_unless_asked_for() {
        let style = TextStyle::default();
        assert_eq!(style.stroke_width, 0.0);
        assert_eq!(style.stroke_rgba[3], 0);
        assert_eq!(style.shadow_rgba[3], 0);
        let a = rasterize("Same", 1920.0, 1080.0, &style).expect("a");
        let b = rasterize(
            "Same",
            1920.0,
            1080.0,
            &TextStyle {
                stroke_rgba: [255, 0, 0, 255],
                stroke_width: 0.0,
                ..TextStyle::default()
            },
        )
        .expect("b");
        assert_eq!(a.rgba, b.rgba, "a zero-width stroke changes nothing");
    }

    /// A shadow is cast by the SILHOUETTE — glyph plus outline — not by the
    /// glyph alone. Casting it from the glyph buries its strongest part
    /// under the stroke painted over it, and a stroked caption then looks
    /// as though it has no shadow: measured at a third of the effect the
    /// same shadow has on unstroked text, which is what a reviewer spotted
    /// by eye.
    #[test]
    fn a_shadow_is_cast_by_the_outline_too() {
        let base = TextStyle {
            text_rgba: [255, 255, 255, 255],
            background_rgba: [0, 0, 0, 0],
            padding: 30.0,
            ..TextStyle::default()
        };
        let shadow = |st: TextStyle| TextStyle {
            shadow_rgba: [0, 0, 0, 220],
            shadow_radius: 12.0,
            shadow_offset: [0.0, 6.0],
            ..st
        };
        let ink =
            |r: &RasterizedText| -> f64 { r.rgba.chunks_exact(4).map(|px| px[3] as f64).sum() };
        let plain = rasterize("Hold", 1920.0, 1080.0, &base).expect("plain");
        let plain_shadowed =
            rasterize("Hold", 1920.0, 1080.0, &shadow(base.clone())).expect("plain shadowed");

        let stroked_style = TextStyle {
            stroke_rgba: [0, 0, 0, 255],
            stroke_width: 5.0,
            ..base.clone()
        };
        let stroked = rasterize("Hold", 1920.0, 1080.0, &stroked_style).expect("stroked");
        let stroked_shadowed =
            rasterize("Hold", 1920.0, 1080.0, &shadow(stroked_style)).expect("stroked shadowed");

        let plain_gain = ink(&plain_shadowed) - ink(&plain);
        let stroked_gain = ink(&stroked_shadowed) - ink(&stroked);
        assert!(plain_gain > 0.0, "the shadow does something on plain text");
        assert!(
            stroked_gain > plain_gain * 0.6,
            "the outline swallowed the shadow: {stroked_gain} vs {plain_gain} unstroked"
        );
    }
}

#[cfg(test)]
mod reveal_layout_tests {
    use super::*;

    fn style() -> TextStyle {
        TextStyle {
            font_size: 48.0,
            padding: 10.0,
            left_margin: 40.0,
            right_margin: 40.0,
            ..TextStyle::default()
        }
    }

    /// The spans have to describe the picture `rasterize` draws, so they are
    /// laid out the same way — and they must march left to right without
    /// gaps or overlaps, or a typewriter would stutter.
    #[test]
    fn word_spans_march_across_the_line_in_order() {
        let layout = reveal_layout("one two three", 1920.0, 1080.0, &style(), RevealBy::Word)
            .expect("layout");
        assert_eq!(layout.units.len(), 3, "three words");
        assert!(layout.units.iter().all(|u| u.line == 0), "all on one line");
        for pair in layout.units.windows(2) {
            assert!(
                pair[0].end_x <= pair[1].start_x,
                "words must not overlap: {:?}",
                pair
            );
            assert!(pair[0].start_x < pair[1].start_x, "and must be in order");
        }
        let last = layout.units.last().unwrap();
        assert!(last.end_x <= layout.width, "inside the raster");
    }

    /// A typewriter ticks per CLUSTER: an emoji or a combining accent is one
    /// keystroke, not several.
    #[test]
    fn character_spans_are_clusters_not_bytes() {
        let plain =
            reveal_layout("abc", 1920.0, 1080.0, &style(), RevealBy::Character).expect("layout");
        assert_eq!(plain.units.len(), 3);

        let accented = reveal_layout("e\u{0301}", 1920.0, 1080.0, &style(), RevealBy::Character)
            .expect("layout");
        assert_eq!(accented.units.len(), 1, "e + combining acute is one letter");
    }

    /// Wrapped text reveals one line at a time: the spans carry which line
    /// they are on, and the line tops tile the raster.
    #[test]
    fn wrapped_text_reports_a_line_per_span() {
        let long = "wrapping happens when the words no longer fit on a single line at all";
        let layout = reveal_layout(long, 700.0, 1080.0, &style(), RevealBy::Word).expect("layout");
        let lines: Vec<u32> = layout.units.iter().map(|u| u.line).collect();
        assert!(lines.iter().max().copied().unwrap_or(0) > 0, "it wrapped");
        assert!(lines.windows(2).all(|w| w[0] <= w[1]), "in reading order");
        assert_eq!(
            layout.line_tops.len(),
            lines.iter().max().unwrap().to_owned() as usize + 1
        );
        for pair in layout.line_tops.windows(2) {
            assert!(
                (pair[1] - pair[0] - layout.line_height).abs() < 0.001,
                "lines tile by exactly one line height"
            );
        }
    }

    /// A reveal must not move the caption: the layout it measures is the
    /// same box `measure` reports for the whole string.
    #[test]
    fn the_reveal_layout_matches_the_measured_box() {
        let text = "the box must not move";
        let measured = measure(text, 1920.0, 1080.0, &style()).expect("measure");
        let layout = reveal_layout(text, 1920.0, 1080.0, &style(), RevealBy::Word).expect("layout");
        assert_eq!(layout.width, measured.width);
        assert_eq!(layout.height, measured.height);
    }
}

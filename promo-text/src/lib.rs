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
            smoothing: None,
        }
    }
}

/// Distance from every pixel to the nearest covered one, by two chamfer
/// passes. O(w·h) and round enough for an outline; a separable max filter
/// would give square corners on round letters.
fn distance_to_ink(mask: &[f32], width: usize, height: usize) -> Vec<f32> {
    const NEAR: f32 = 1.0;
    const DIAG: f32 = 1.4142136;
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
                if x > 0 { d = d.min(dist[at(x - 1, y - 1)] + DIAG); }
                if x + 1 < width { d = d.min(dist[at(x + 1, y - 1)] + DIAG); }
            }
            if x > 0 { d = d.min(dist[at(x - 1, y)] + NEAR); }
            dist[at(x, y)] = d;
        }
    }
    for y in (0..height).rev() {
        for x in (0..width).rev() {
            let mut d = dist[at(x, y)];
            if y + 1 < height {
                d = d.min(dist[at(x, y + 1)] + NEAR);
                if x + 1 < width { d = d.min(dist[at(x + 1, y + 1)] + DIAG); }
                if x > 0 { d = d.min(dist[at(x - 1, y + 1)] + DIAG); }
            }
            if x + 1 < width { d = d.min(dist[at(x + 1, y)] + NEAR); }
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
    let r = (radius / 3.0).round().max(0.0) as usize;
    if r == 0 || width == 0 || height == 0 {
        return mask.to_vec();
    }
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

fn has_family(fonts: &FontSystem, name: &str) -> bool {
    fonts
        .db()
        .faces()
        .any(|face| face.families.iter().any(|(family, _)| family == name))
}

fn resolve_family(fonts: &mut FontSystem, requested: Option<&str>) -> ResolvedFamily {
    match requested {
        Some("serif") => return ResolvedFamily::Serif,
        Some("monospaced") | Some("mono") | Some("monospace") => return ResolvedFamily::Monospace,
        // A named font that exists wins; one that does not falls through to
        // the UI sans below, rather than silently handing back whatever
        // fontdb happens to default to.
        Some(name) if name != "system" && !name.is_empty() && has_family(fonts, name) => {
            return ResolvedFamily::Named(name.to_string())
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
fn layout(
    text: &str,
    canvas_width: f64,
    canvas_height: f64,
    style: &TextStyle,
) -> Option<Layout> {
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
            y: style.vertical_margin,
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
        || (style.shadow_rgba[3] > 0 && (style.shadow_radius > 0.0
            || style.shadow_offset != [0.0, 0.0]));
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
            if !stroked { return 0.0; }
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
                    if a <= 0.001 { continue; }
                    blend(&mut rgba, width, x as u32, y as u32,
                          [style.shadow_rgba[0], style.shadow_rgba[1], style.shadow_rgba[2]], a);
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
                    if coverage <= 0.001 { continue; }
                    blend(&mut rgba, width, x as u32, y as u32,
                          [style.stroke_rgba[0], style.stroke_rgba[1], style.stroke_rgba[2]],
                          coverage as f64 * alpha);
                }
            }
        }
        for y in 0..h {
            for x in 0..w {
                let a = mask[y * w + x] as f64;
                if a <= 0.001 { continue; }
                blend(&mut rgba, width, x as u32, y as u32,
                      [style.text_rgba[0], style.text_rgba[1], style.text_rgba[2]], a);
            }
        }
    }

    // Top-left origin, matching the app: both its SwiftUI preview
    // (`.offset(y: verticalMargin)`) and its exporter (a bitmap context
    // flipped to top-left in `makeBitmapContext`) treat the vertical margin as
    // a distance from the TOP. This module used to measure from the bottom,
    // which put every caption somewhere else than the app drew it.
    let _ = canvas_height;
    let y = style.vertical_margin;

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

    #[test]
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
        assert_eq!(measure("Two\nlines", 1440.0, 900.0, &style).unwrap().lines, 2);
        assert!(measure(
            "A considerably longer headline that cannot possibly fit on one line",
            1440.0, 900.0, &style).unwrap().lines >= 2);
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
            r.rgba.chunks_exact(4)
                .filter(|px| px[3] > 128 && px[0] < 60 && px[1] < 60 && px[2] < 60)
                .count()
        };
        assert_eq!(dark_opaque(&plain), 0, "no plate, no stroke: nothing dark");
        assert!(dark_opaque(&stroked) > 200,
                "the stroke drew {} dark pixels", dark_opaque(&stroked));

        // And the letters are still white on top — an outline that swallowed
        // the fill would be worse than none.
        let white = |r: &RasterizedText| {
            r.rgba.chunks_exact(4)
                .filter(|px| px[3] > 200 && px[0] > 220 && px[1] > 220 && px[2] > 220)
                .count()
        };
        let (before, after) = (white(&plain), white(&stroked));
        assert!(after as f64 > before as f64 * 0.7,
                "fill survives the outline: {before} -> {after}");
    }

    /// A stroke wider than the padding is CLIPPED, not grown into: the box
    /// is text-plus-padding, and letting it grow would move every caption
    /// and resize any plate behind it.
    #[test]
    fn a_stroke_never_moves_the_caption() {
        let base = TextStyle { padding: 8.0, ..TextStyle::default() };
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
        assert!(rows(h / 2, h) > rows(0, h / 4),
                "the shadow falls below: {} vs {}", rows(h / 2, h), rows(0, h / 4));
        let white = out.rgba.chunks_exact(4)
            .filter(|px| px[3] > 200 && px[0] > 220).count();
        assert!(white > 0, "the letters are still white");
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
        let b = rasterize("Same", 1920.0, 1080.0, &TextStyle {
            stroke_rgba: [255, 0, 0, 255], stroke_width: 0.0, ..TextStyle::default()
        }).expect("b");
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
        let ink = |r: &RasterizedText| -> f64 {
            r.rgba.chunks_exact(4).map(|px| px[3] as f64).sum()
        };
        let plain = rasterize("Hold", 1920.0, 1080.0, &base).expect("plain");
        let plain_shadowed = rasterize("Hold", 1920.0, 1080.0, &shadow(base.clone()))
            .expect("plain shadowed");

        let stroked_style = TextStyle {
            stroke_rgba: [0, 0, 0, 255],
            stroke_width: 5.0,
            ..base.clone()
        };
        let stroked = rasterize("Hold", 1920.0, 1080.0, &stroked_style).expect("stroked");
        let stroked_shadowed = rasterize("Hold", 1920.0, 1080.0, &shadow(stroked_style))
            .expect("stroked shadowed");

        let plain_gain = ink(&plain_shadowed) - ink(&plain);
        let stroked_gain = ink(&stroked_shadowed) - ink(&stroked);
        assert!(plain_gain > 0.0, "the shadow does something on plain text");
        assert!(
            stroked_gain > plain_gain * 0.6,
            "the outline swallowed the shadow: {stroked_gain} vs {plain_gain} unstroked"
        );
    }
}

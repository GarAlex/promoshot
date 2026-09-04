//! A caption's text style, resolved from its own `captionStyle` over the
//! composition's subtitle defaults — the one place a caption's look is
//! decided, shared by the renderer (which rasterizes it) and the layout
//! checks (which only measure it).
use promo_model::{CompositionSettings, SubtitleStyle};

/// The style a caption is drawn and measured with.
pub fn caption_style(style: Option<&SubtitleStyle>, settings: &CompositionSettings) -> promo_text::TextStyle {
    let get = |pick: fn(&SubtitleStyle) -> Option<f64>, fallback: f64| -> f64 {
        style.and_then(pick).unwrap_or(fallback)
    };
    let text_rgba = rgba_bytes(
        settings.resolve_color(
            &style
                .and_then(|s| s.text_color_hex.clone())
                .unwrap_or_else(|| settings.subtitle_color_hex.clone()),
        ),
        1.0,
    );
    let bg_opacity = style
        .and_then(|s| s.background_opacity)
        .unwrap_or(settings.subtitle_background_opacity);
    let background_rgba = rgba_bytes(
        settings.resolve_color(
            &style
                .and_then(|s| s.background_color_hex.clone())
                .unwrap_or_else(|| settings.subtitle_background_color_hex.clone()),
        ),
        bg_opacity,
    );
    promo_text::TextStyle {
        font_family: style
            .and_then(|s| s.font_family.as_ref())
            .map(|f| f.as_str().to_string())
            .or_else(|| Some(settings.subtitle_font_family.as_str().to_string())),
        font_size: get(|s| s.font_size, settings.subtitle_font_size),
        bold: style
            .and_then(|s| s.is_bold)
            .unwrap_or(settings.subtitle_bold),
        italic: style
            .and_then(|s| s.is_italic)
            .unwrap_or(settings.subtitle_italic),
        // Per caption, then the composition's default. Nothing falls back to
        // a constant here any more: the constant was "center" while the app
        // assumed "leading", so an unaligned caption edited one way rendered
        // another.
        align: promo_text::Align::parse(
            style
                .and_then(|s| s.alignment.as_ref())
                .unwrap_or(&settings.subtitle_alignment)
                .as_str(),
        ),
        text_rgba,
        background_rgba,
        stroke_rgba: rgba_bytes(
            settings.resolve_color(
                &style
                    .and_then(|s| s.stroke_color_hex.clone())
                    .unwrap_or_else(|| settings.subtitle_stroke_color_hex.clone()),
            ),
            1.0,
        ),
        stroke_width: get(|s| s.stroke_width, settings.subtitle_stroke_width),
        shadow_rgba: rgba_bytes(
            settings.resolve_color(
                &style
                    .and_then(|s| s.shadow_color_hex.clone())
                    .unwrap_or_else(|| settings.subtitle_shadow_color_hex.clone()),
            ),
            style
                .and_then(|s| s.shadow_opacity)
                .unwrap_or(settings.subtitle_shadow_opacity),
        ),
        shadow_radius: get(|s| s.shadow_radius, settings.subtitle_shadow_radius),
        // Default the drop from the EFFECTIVE radius, not the composition's.
        // Deriving it from the settings value gave a caption that set its own
        // radius an offset of zero, so its shadow sat directly under the
        // glyphs where they hid it.
        shadow_offset: style
            .and_then(|s| s.shadow_offset)
            .or(settings.subtitle_shadow_offset)
            .unwrap_or_else(|| {
                [
                    0.0,
                    get(|s| s.shadow_radius, settings.subtitle_shadow_radius) / 2.0,
                ]
            }),
        padding: get(|s| s.padding, settings.subtitle_background_padding),
        corner_radius: get(
            |s| s.corner_radius,
            settings.subtitle_background_corner_radius,
        ),
        left_margin: get(|s| s.left_margin, settings.subtitle_left_margin),
        right_margin: get(|s| s.right_margin, settings.subtitle_right_margin),
        vertical_margin: get(|s| s.vertical_margin, settings.subtitle_vertical_margin),
        // Per caption only — there is no composition-wide caption placement,
        // and that is a choice: the settings margins are the composition's
        // statement of where captions live, and a placement is one caption
        // saying otherwise.
        placement: style.and_then(|s| s.placement.clone()),
        line_height: 1.25,
        // Let promo-text choose from the text colour.
        smoothing: None,
    }
}

/// Hex + alpha as straight RGBA bytes.
pub fn rgba_bytes(hex: &str, alpha: f64) -> [u8; 4] {
    let c = rgba_from_hex(hex);
    [
        (c[0] * 255.0).round() as u8,
        (c[1] * 255.0).round() as u8,
        (c[2] * 255.0).round() as u8,
        (alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

/// `RRGGBB` or `RRGGBBAA`, a leading `#` allowed; black when it is not one.
pub fn rgba_from_hex(hex: &str) -> [f32; 4] {
    let mut value = hex.trim().to_uppercase();
    if let Some(stripped) = value.strip_prefix('#') {
        value = stripped.to_string();
    }
    let Ok(parsed) = u64::from_str_radix(&value, 16) else {
        return [0.0, 0.0, 0.0, 1.0];
    };
    match value.len() {
        6 => [
            ((parsed >> 16) & 0xFF) as f32 / 255.0,
            ((parsed >> 8) & 0xFF) as f32 / 255.0,
            (parsed & 0xFF) as f32 / 255.0,
            1.0,
        ],
        8 => [
            ((parsed >> 24) & 0xFF) as f32 / 255.0,
            ((parsed >> 16) & 0xFF) as f32 / 255.0,
            ((parsed >> 8) & 0xFF) as f32 / 255.0,
            (parsed & 0xFF) as f32 / 255.0,
        ],
        _ => [0.0, 0.0, 0.0, 1.0],
    }
}

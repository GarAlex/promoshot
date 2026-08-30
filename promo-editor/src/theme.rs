//! Selecting and following a THEME — a palette resource.
//!
//! Ported rule-for-rule from the Mac app's `syncSelectedTheme` /
//! `followRoles` / `adoptCaptionLook` (ProjectStore.swift), because the
//! folding is exactly the kind of behaviour two front ends would slowly
//! disagree about if each carried its own copy:
//!
//! - `settings.palette` is the selected resource's MATERIALIZED copy —
//!   every resolver reads the settings field, and the resource is the
//!   authority it is refreshed from.
//! - The settings colours a role drives are pointed AT the role
//!   (`subtitleColorHex = "@text"`) rather than filled with its value, so
//!   a later theme re-skins the project with no further writing. Adopted
//!   only where nothing was authored: a field still holding its factory
//!   value, or already following the same role.
//! - When the incoming theme does NOT state a role a field follows, the
//!   reference is inlined back to the value it last resolved to — an
//!   undefined name renders BLACK while an editing canvas draws it white,
//!   so leaving it would ship invisible captions that looked fine.
//! - Typography (`captionStyle`) lands only at SELECTION, over fields
//!   nobody moved off the factory default. Numbers cannot be references;
//!   re-applying on every refresh would silently revert anyone's change.
//! - `canvas` maps to no settings colour (the ground arrives as a
//!   background plate on the Mac; `themePlateID` adoption needs the shared
//!   library and stays host work).

use promo_model::{CompositionSettings, PaletteColor, ProjectResource, SubtitleStyle};

/// The role names and the settings colour each drives. `canvas` and
/// `highlight` are handled apart: one has no settings colour, the other
/// lives inside `subtitleReveal`.
const ROLE_FIELDS: [&str; 6] = [
    "text",
    "text-bg",
    "edge",
    "caption-outline",
    "caption-shadow",
    "media-shadow",
];

/// Refreshes `settings` from the selected palette resource. `adopting_look`
/// is true at the SELECTION moment only — the one time typography is taken.
pub(crate) fn sync_selected_palette(
    settings: &mut CompositionSettings,
    resource: &ProjectResource,
    adopting_look: bool,
) {
    let outgoing = settings.palette.clone().unwrap_or_default();
    let entries = resource.palette.clone().unwrap_or_default();
    settings.palette = if entries.is_empty() {
        None
    } else {
        Some(entries.clone())
    };
    follow_roles(settings, &entries, &outgoing);
    if adopting_look {
        if let Some(look) = &resource.caption_style {
            adopt_caption_look(settings, look);
        }
    }
}

fn stated(entries: &[PaletteColor], role: &str) -> bool {
    entries.iter().any(|c| c.name.eq_ignore_ascii_case(role))
}

fn outgoing_value(outgoing: &[PaletteColor], role: &str) -> Option<String> {
    outgoing
        .iter()
        .find(|c| c.name.eq_ignore_ascii_case(role))
        .map(|c| c.color_hex.clone())
}

fn follow_roles(
    settings: &mut CompositionSettings,
    entries: &[PaletteColor],
    outgoing: &[PaletteColor],
) {
    let factory = CompositionSettings::default();
    for role in ROLE_FIELDS {
        let (field, factory_value): (&mut String, &String) = match role {
            "text" => (
                &mut settings.subtitle_color_hex,
                &factory.subtitle_color_hex,
            ),
            "text-bg" => (
                &mut settings.subtitle_background_color_hex,
                &factory.subtitle_background_color_hex,
            ),
            "edge" => (
                &mut settings.video_border_color_hex,
                &factory.video_border_color_hex,
            ),
            "caption-outline" => (
                &mut settings.subtitle_stroke_color_hex,
                &factory.subtitle_stroke_color_hex,
            ),
            "caption-shadow" => (
                &mut settings.subtitle_shadow_color_hex,
                &factory.subtitle_shadow_color_hex,
            ),
            "media-shadow" => (
                &mut settings.video_shadow_color_hex,
                &factory.video_shadow_color_hex,
            ),
            _ => unreachable!(),
        };
        let reference = format!("@{role}");
        let follows = field.eq_ignore_ascii_case(&reference);
        if stated(entries, role) {
            if !follows && field == factory_value {
                *field = reference;
            }
        } else if follows {
            *field = outgoing_value(outgoing, role).unwrap_or_else(|| factory_value.clone());
        }
    }
    follow_highlight(settings, entries, outgoing);
}

/// The reveal's highlight has no settings colour of its own — it lives
/// inside `subtitleReveal`. Same rule as the rest: adopt only where
/// nothing was authored, inline back when the incoming theme drops it.
fn follow_highlight(
    settings: &mut CompositionSettings,
    entries: &[PaletteColor],
    outgoing: &[PaletteColor],
) {
    let Some(reveal) = settings.subtitle_reveal.as_mut() else {
        return;
    };
    let follows = reveal
        .highlight_color_hex
        .as_deref()
        .is_some_and(|c| c.eq_ignore_ascii_case("@highlight"));
    if stated(entries, "highlight") {
        if !follows && reveal.highlight_color_hex.is_none() {
            reveal.highlight_color_hex = Some("@highlight".into());
        }
    } else if follows {
        reveal.highlight_color_hex = outgoing_value(outgoing, "highlight");
    }
}

/// The theme's typography, over fields nobody has moved off the factory
/// default. Colours are deliberately NOT taken from here even though
/// `SubtitleStyle` can carry them: roles own colour, and one thing with
/// two sources is how they start disagreeing.
fn adopt_caption_look(settings: &mut CompositionSettings, look: &SubtitleStyle) {
    let factory = CompositionSettings::default();
    macro_rules! adopt {
        ($value:expr, $field:ident) => {
            if let Some(v) = $value.clone() {
                if settings.$field == factory.$field {
                    settings.$field = v;
                }
            }
        };
    }
    adopt!(look.font_family, subtitle_font_family);
    adopt!(look.font_size, subtitle_font_size);
    adopt!(look.is_bold, subtitle_bold);
    adopt!(look.is_italic, subtitle_italic);
    adopt!(look.alignment, subtitle_alignment);
    adopt!(look.padding, subtitle_background_padding);
    adopt!(look.corner_radius, subtitle_background_corner_radius);
    adopt!(look.background_opacity, subtitle_background_opacity);
    adopt!(look.stroke_width, subtitle_stroke_width);
    adopt!(look.shadow_opacity, subtitle_shadow_opacity);
    adopt!(look.shadow_radius, subtitle_shadow_radius);
    adopt!(look.left_margin, subtitle_left_margin);
    adopt!(look.right_margin, subtitle_right_margin);
    adopt!(look.vertical_margin, subtitle_vertical_margin);
    if let Some(offset) = look.shadow_offset {
        if settings.subtitle_shadow_offset.is_none() {
            settings.subtitle_shadow_offset = Some(offset);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette_resource(entries: &[(&str, &str)]) -> ProjectResource {
        serde_json::from_value(serde_json::json!({
            "id": "T", "kind": "palette", "filename": "",
            "displayName": "Studio Dark", "addedAt": 0,
            "palette": entries.iter().map(|(n, c)| serde_json::json!(
                {"name": n, "colorHex": c})).collect::<Vec<_>>(),
        }))
        .unwrap()
    }

    /// Selection points factory-default colours at the roles the theme
    /// states, materializes the entries, and takes the typography — but a
    /// colour or size someone authored stays theirs.
    #[test]
    fn selection_follows_roles_and_adopts_look_over_defaults_only() {
        let mut settings = CompositionSettings {
            subtitle_font_size: 200.0,               // authored — must survive
            video_border_color_hex: "123456".into(), // authored
            ..Default::default()
        };
        let mut resource =
            palette_resource(&[("canvas", "101014"), ("text", "F2F2F7"), ("edge", "3A3A3C")]);
        resource.caption_style = Some(
            serde_json::from_value(serde_json::json!(
                {"fontSize": 96.0, "isBold": true}))
            .unwrap(),
        );

        sync_selected_palette(&mut settings, &resource, true);

        assert_eq!(settings.palette.as_ref().unwrap().len(), 3);
        assert_eq!(settings.subtitle_color_hex, "@text");
        assert_eq!(
            settings.video_border_color_hex, "123456",
            "an authored colour is not the theme's to take"
        );
        assert_eq!(
            settings.subtitle_font_size, 200.0,
            "an authored size is not the theme's to take"
        );
        assert!(settings.subtitle_bold, "the look lands over defaults");
    }

    /// Switching to a theme that drops a role must inline the reference
    /// back to what it resolved to — a name nobody defines renders black.
    #[test]
    fn a_dropped_role_is_inlined_back_not_left_dangling() {
        let mut settings = CompositionSettings::default();
        sync_selected_palette(
            &mut settings,
            &palette_resource(&[("text", "F2F2F7")]),
            true,
        );
        assert_eq!(settings.subtitle_color_hex, "@text");

        sync_selected_palette(
            &mut settings,
            &palette_resource(&[("edge", "3A3A3C")]),
            true,
        );
        assert_eq!(
            settings.subtitle_color_hex, "F2F2F7",
            "the outgoing theme's value is inlined, not the factory's"
        );
        assert_eq!(settings.video_border_color_hex, "@edge");
    }
}

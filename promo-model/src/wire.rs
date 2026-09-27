//! The format's vocabulary of named values, for hosts that keep their own
//! copy (review 2026-09-27, P3-46): the apps' Swift declares these enums,
//! generated from this list by core-private's `tests/wire.rs`, so a value
//! added here reaches the app's decoder the same day — and one the app
//! decodes strictly where the core carries it (an effect kind, a marker
//! kind) cannot make the app call a readable file damaged.

/// One named-value enum: its wire spellings in order, the value an unknown
/// one acts as (`None`: strict — an unknown value refuses the file), and
/// retired spellings that still read as a live value.
#[derive(Debug, Clone, PartialEq)]
pub struct WireEnum {
    pub name: &'static str,
    pub values: Vec<&'static str>,
    pub fallback: Option<&'static str>,
    pub legacy: Vec<(&'static str, &'static str)>,
}

/// Every named-value enum in the format.
pub fn enums() -> Vec<WireEnum> {
    use crate::project::*;
    vec![
        ProjectLayerKind::wire(),
        ProjectResourceKind::wire(),
        AudioEffectKind::wire(),
        MarkerKind::wire(),
        ProjectExportKind::wire(),
        Easing::wire(),
        Anchor::wire(),
        PlacementMode::wire(),
        ImageOrientation::wire(),
        TransitionKind::wire(),
        TransitionEdge::wire(),
        BlendMode::wire(),
        RevealUnit::wire(),
        RevealMode::wire(),
        SubtitleVoiceKind::wire(),
        SubtitleTextAlignment::wire(),
        SubtitleFontWeight::wire(),
        SubtitleFontFamily::wire(),
        DrawingShapeKind::wire(),
        ResourceFrameKind::wire(),
        FrameMaterial::wire(),
        BackgroundFill::wire(),
        ReleaseMoment::wire(),
    ]
}

#[cfg(test)]
mod tests {
    /// Every enum the model declares is listed: a new one joins the
    /// generated copies or this fails.
    #[test]
    fn every_enum_is_listed() {
        let source = include_str!("project.rs");
        let declared =
            source.matches("\ntolerant_enum!(").count() + source.matches("\nstrict_enum!(").count();
        assert_eq!(super::enums().len(), declared);
        let wire: Vec<_> = super::enums().iter().map(|e| e.name).collect();
        assert!(wire.contains(&"TransitionKind") && wire.contains(&"ProjectLayerKind"));
    }
}

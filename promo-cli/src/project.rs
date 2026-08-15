//! Opening a project folder, and answering "what can actually be rendered?"

use promo_media::Registry;
use promo_model::{ProjectLayerKind, ProjectMetadata, ProjectResource};
use std::path::{Path, PathBuf};

/// A project folder: `metadata.json` plus `Resources/` and `Images/`.
pub struct Project {
    pub dir: PathBuf,
    pub meta: ProjectMetadata,
}

/// Why a layer cannot be rendered by this tool.
#[derive(Debug, Clone, PartialEq)]
pub enum Unsupported {
    /// The asset could not be opened by any registered backend.
    Undecodable(String),
    /// Audio has no bearing on a rendered frame; it is skipped silently for
    /// images and noted for video.
    Audio,
    MissingFile(PathBuf),
}

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unsupported::Undecodable(why) => write!(f, "{why}"),
            Unsupported::Audio => write!(f, "audio does not appear in a rendered frame"),
            Unsupported::MissingFile(p) => write!(f, "file missing: {}", p.display()),
        }
    }
}

impl Project {
    pub fn open(dir: &Path) -> Result<Self, String> {
        let meta_path = dir.join("metadata.json");
        let text = std::fs::read_to_string(&meta_path)
            .map_err(|e| format!("{}: {e}", meta_path.display()))?;
        let mut meta = ProjectMetadata::from_json(&text)
            .map_err(|e| format!("{}: {e}", meta_path.display()))?;
        // Attached layers become plain numbers before anything reads them, so
        // the renderer never has to know the difference.
        for problem in promo_timeline::resolve_attachments(&mut meta) {
            eprintln!("warning: {problem}");
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            meta,
        })
    }

    pub fn resources(&self) -> &[ProjectResource] {
        self.meta.resources.as_deref().unwrap_or(&[])
    }

    pub fn resource(&self, id: &str) -> Option<&ProjectResource> {
        self.resources().iter().find(|r| r.id == id)
    }

    /// Where a resource's file lives. The app writes media to `Resources/` and
    /// slideshow stills to `Images/`; try both rather than encoding a rule
    /// that only holds for one of them.
    pub fn resource_path(&self, resource: &ProjectResource) -> Option<PathBuf> {
        for sub in ["Resources", "Images", ""] {
            let candidate = if sub.is_empty() {
                self.dir.join(&resource.filename)
            } else {
                self.dir.join(sub).join(&resource.filename)
            };
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }

    /// Composition length: the furthest any layer runs, falling back to the
    /// recorded video duration.
    pub fn duration(&self) -> f64 {
        let from_layers = self
            .meta
            .layers
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .filter_map(|l| l.duration.map(|d| l.start_time.max(0.0) + d.max(0.0)))
            .fold(0.0f64, f64::max);
        let recorded = self.meta.video_duration.max(0.0);
        from_layers.max(recorded)
    }

    /// Is this layer's media present and openable? `None` = fine.
    fn media_problem(&self, layer: &promo_model::ProjectLayer) -> Option<Unsupported> {
        let resource = layer
            .resource_id
            .as_ref()
            .and_then(|id| self.resource(id))?;
        let Some(path) = self.resource_path(resource) else {
            return Some(Unsupported::MissingFile(
                self.dir.join("Resources").join(&resource.filename),
            ));
        };
        // Actually open a decoder rather than just checking the file exists,
        // so `inspect` can say "ffmpeg not found" or "no video stream"
        // instead of letting the render discover it later.
        match Registry::with_defaults().open_decoder(&path) {
            Ok(_) => None,
            Err(e) => Some(Unsupported::Undecodable(e.to_string())),
        }
    }

    /// Per-layer verdict: `None` = renderable.
    pub fn unsupported(&self, layer: &promo_model::ProjectLayer) -> Option<Unsupported> {
        match layer.kind {
            // Captions render in the core now (promo-text).
            ProjectLayerKind::Background | ProjectLayerKind::Drawing => None,
            ProjectLayerKind::Caption => None,
            ProjectLayerKind::Audio => Some(Unsupported::Audio),
            // Video is decoded through promo-media now; the only question is
            // whether the file is there and a backend will take it.
            ProjectLayerKind::Video => self.media_problem(layer),
            ProjectLayerKind::Image => {
                let resource = layer.resource_id.as_ref().and_then(|id| self.resource(id));
                match resource {
                    None => Some(Unsupported::MissingFile(self.dir.join("Resources"))),
                    Some(r) => match self.resource_path(r) {
                        Some(_) => None,
                        None => Some(Unsupported::MissingFile(
                            self.dir.join("Resources").join(&r.filename),
                        )),
                    },
                }
            }
        }
    }
}

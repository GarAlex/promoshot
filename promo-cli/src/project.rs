//! Opening a project folder, and answering "what can actually be rendered?"

use promo_media::Registry;
use promo_model::{ProjectLayerKind, ProjectMetadata, ProjectResource};
use std::path::{Path, PathBuf};

/// A project folder: `metadata.json` plus `Resources/` and `Images/`.
pub struct Project {
    pub dir: PathBuf,
    pub meta: ProjectMetadata,
    /// Resources as RESOLVED against the folder: declared entries plus any
    /// media sitting in `Resources/` that nothing declared, minus nothing —
    /// an entry whose file is gone stays, marked missing, so a layer using it
    /// can say so instead of rendering as an empty hole.
    resolved: Vec<promo_model::ResolvedResource>,
    /// Attachments that could not be resolved. `open` prints them for the
    /// render commands; `validate` reports them with everything else instead
    /// of letting them scroll past in stderr.
    pub attachment_problems: Vec<String>,
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
    /// The layer points at a resource the project no longer has.
    MissingResource(String),
}

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unsupported::Undecodable(why) => write!(f, "{why}"),
            Unsupported::Audio => write!(f, "audio does not appear in a rendered frame"),
            Unsupported::MissingFile(p) => write!(f, "file missing: {}", p.display()),
            Unsupported::MissingResource(name) => {
                write!(f, "media missing: nothing in Resources/ for \"{name}\"")
            }
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
        let attachment_problems: Vec<String> = promo_timeline::resolve_attachments(&mut meta)
            .into_iter()
            .map(|problem| problem.to_string())
            .collect();
        // Nested compositions: a composition that contains itself, or nests
        // deeper than the cap, cannot be rendered by recursion — refuse the
        // file, as a decode failure would. Lesser problems (a nested layer
        // naming an unknown resource) are warnings `validate` lists.
        let mut attachment_problems = attachment_problems;
        for problem in promo_model::nesting::problems(&meta) {
            if problem.contains("contains itself") || problem.contains("nests deeper than") {
                return Err(format!("{}: {problem}", meta_path.display()));
            }
            attachment_problems.push(problem);
        }
        let resolved = promo_model::effective_resources(&meta, &Self::listing(dir));
        Ok(Self {
            dir: dir.to_path_buf(),
            meta,
            resolved,
            attachment_problems,
        })
    }

    /// Filenames in `Resources/` (and `Images/`, which slideshow stills use).
    fn listing(dir: &Path) -> Vec<String> {
        let mut names = Vec::new();
        for sub in ["Resources", "Images"] {
            let Ok(entries) = std::fs::read_dir(dir.join(sub)) else {
                continue;
            };
            for entry in entries.flatten() {
                if entry.path().is_file() {
                    names.push(entry.file_name().to_string_lossy().to_string());
                }
            }
        }
        names
    }

    /// Every resource the project effectively has — declared and derived.
    /// This is what the renderer reads, so a file dropped into `Resources/`
    /// is usable without being declared first.
    pub fn resources(&self) -> Vec<ProjectResource> {
        self.resolved.iter().map(|r| r.resource.clone()).collect()
    }

    /// True when this resource is declared but its file is gone.
    pub fn is_missing(&self, id: &str) -> bool {
        self.resolved
            .iter()
            .any(|r| r.resource.id == id && r.is_missing())
    }

    pub fn resource(&self, id: &str) -> Option<&ProjectResource> {
        self.resolved
            .iter()
            .map(|r| &r.resource)
            .find(|r| r.id == id)
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
        promo_timeline::composition_duration(&self.meta)
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
        // A layer pointing at media the project no longer has renders as
        // nothing, whatever its kind — say so rather than counting it
        // renderable. (Drawing layers were reported renderable for weeks
        // while nothing of them reached a frame; a report that only checks
        // "is this kind supported" is how that hid.)
        if let Some(id) = layer.resource_id.as_deref() {
            if self.is_missing(id) || self.resource(id).is_none() {
                return Some(Unsupported::MissingResource(layer.name.clone()));
            }
        }
        match layer.kind {
            ProjectLayerKind::Background => None,
            // Vector content is drawn by the engine from the resource; a
            // drawing layer with no document draws nothing.
            ProjectLayerKind::Drawing => {
                let has_shapes = layer
                    .resource_id
                    .as_deref()
                    .and_then(|id| self.resource(id))
                    .and_then(|r| r.drawing.as_ref())
                    .is_some_and(|doc| !doc.shapes.is_empty());
                if has_shapes {
                    None
                } else {
                    Some(Unsupported::MissingResource(layer.name.clone()))
                }
            }
            // Captions render in the core now (promo-text).
            ProjectLayerKind::Caption => None,
            ProjectLayerKind::Audio => Some(Unsupported::Audio),
            // A model draws through the engine's model pass from its `.glb`;
            // the only question is whether the file is there.
            ProjectLayerKind::Model => {
                let resource = layer
                    .resource_id
                    .as_deref()
                    .and_then(|id| self.resource(id));
                match resource {
                    None => Some(Unsupported::MissingResource(layer.name.clone())),
                    Some(r) => match self.resource_path(r) {
                        Some(path) if path.exists() => None,
                        _ => Some(Unsupported::MissingFile(
                            self.dir.join("Resources").join(&r.filename),
                        )),
                    },
                }
            }
            // Video is decoded through promo-media now; the only question is
            // whether the file is there and a backend will take it.
            // A composition draws itself from the document — no file to
            // open; its own layers answer for themselves.
            ProjectLayerKind::Video
                if layer
                    .resource_id
                    .as_deref()
                    .and_then(|id| self.resource(id))
                    .is_some_and(|r| r.kind == promo_model::ProjectResourceKind::Composition) =>
            {
                None
            }
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

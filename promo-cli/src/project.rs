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
    /// Nothing to draw from: the resource the layer names is not in the
    /// project, its file is not in `Resources/`, or a drawing has no ink —
    /// said in words that name which.
    MissingResource(String),
}

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unsupported::Undecodable(why) => write!(f, "{why}"),
            Unsupported::Audio => write!(f, "audio does not appear in a rendered frame"),
            Unsupported::MissingFile(p) => write!(f, "file missing: {}", p.display()),
            Unsupported::MissingResource(why) => write!(f, "{why}"),
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
    /// slideshow stills to `Images/` — the two folders the inventory lists
    /// and the only two the app looks in. A third guess, the project folder
    /// itself, used to find `Resources/tone.wav` written WITH its folder
    /// while the inventory, and so every "is this layer's file there",
    /// called it missing: two resolvers disagreeing about one file, and the
    /// picture and the sound vanished with nothing saying why. Now there is
    /// one answer, and validate names the folder in the filename.
    pub fn resource_path(&self, resource: &ProjectResource) -> Option<PathBuf> {
        if resource.filename.is_empty() {
            return None;
        }
        ["Resources", "Images"]
            .into_iter()
            .map(|sub| self.dir.join(sub).join(&resource.filename))
            .find(|candidate| candidate.is_file())
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
            match self.resource(id) {
                None => {
                    return Some(Unsupported::MissingResource(format!(
                        "its resourceID \"{id}\" names no resource in the project"
                    )))
                }
                Some(resource) if self.is_missing(id) => {
                    return Some(Unsupported::MissingResource(format!(
                        "its file \"{}\" is not in Resources/",
                        resource.filename
                    )))
                }
                Some(_) => {}
            }
        }
        match layer.kind {
            ProjectLayerKind::Background => None,
            // Vector content is drawn by the engine from the resource; a
            // drawing layer with no document draws nothing.
            ProjectLayerKind::Drawing => {
                let resource = layer
                    .resource_id
                    .as_deref()
                    .and_then(|id| self.resource(id));
                let has_shapes = resource
                    .and_then(|r| r.drawing.as_ref())
                    .is_some_and(|doc| !doc.shapes.is_empty())
                    // A particle system (rung 36) draws itself from its recipe.
                    || resource.is_some_and(|r| r.particles.is_some());
                if has_shapes {
                    None
                } else {
                    Some(Unsupported::MissingResource(
                        "its drawing has no shapes, so it draws nothing".into(),
                    ))
                }
            }
            // Captions render in the core now (promo-text).
            ProjectLayerKind::Caption => None,
            ProjectLayerKind::Audio => Some(Unsupported::Audio),
            // A stage layer (rung 33) draws its members; each member answers
            // for its own file when the walk reaches it.
            ProjectLayerKind::Stage => None,
            // A model draws through the engine's model pass from its `.glb`;
            // the only question is whether the file is there.
            ProjectLayerKind::Model => {
                let resource = layer
                    .resource_id
                    .as_deref()
                    .and_then(|id| self.resource(id));
                match resource {
                    None => Some(Unsupported::MissingResource(
                        "it names no model resource".into(),
                    )),
                    // A body the document describes (a recipe) needs no file.
                    Some(r) if r.recipe.is_some() => None,
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

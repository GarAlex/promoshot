//! SVG pictures for PromoShot (core-private docs/SVG-PLAN.md, slice 1).
//!
//! An SVG is a `vector` resource: its file stays in `Resources/` exactly as
//! it arrived, and this crate is the one parser (usvg) and the one renderer
//! (resvg on tiny-skia) every host uses — the Mac, iOS and headless Linux
//! draw the same pixels, and the apps never get a Swift twin of it.
//!
//! - [`import`] reads a file once: the guards ([`Limits`]), its size, and —
//!   when it has `<text>` — the OUTLINES: the file as usvg parsed it with the
//!   fonts this machine has, text turned into paths and nothing else
//!   changed, written by usvg's own writer. That file is derived, kept in
//!   `Resources/.svg/`, and is what the render reads in place of the
//!   original while the block's `sourceHash` still names the original's
//!   bytes. So drawing never needs a font.
//! - [`Picture`] is a parsed tree ready to draw, built with NO fonts at all
//!   ([`Picture::parse`]); [`Picture::render`] draws it, or a crop of it, at
//!   any pixel size.
//! - [`load`] picks the render source for a resource; [`findings`] is what
//!   validate says about a project's vectors.
//!
//! Hrefs: an `<image>` (or `feImage`) may name an embedded `data:` URI or a
//! file inside the project. Anything else — a URL, a path outside the
//! project — is refused with a reason and draws nothing.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use promo_model::vector::{VectorBlock, VectorOverride, KEY, OUTLINES_DIR};
use promo_model::{ProjectMetadata, ProjectResource, ProjectResourceKind};
use serde_json::{json, Value};
use usvg::fontdb;

pub use promo_model::vector;

mod edit;
mod model;

pub use model::{Model, Node, PaintInfo, Tier, MARKER, SYNTHETIC};

/// The caps that keep a hostile file from hanging an import or a render.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// The file's bytes.
    pub max_bytes: usize,
    /// Elements after every `<use>` is expanded — the count a render walks.
    pub max_nodes: u64,
    /// A filter's region, as a multiple of the picture's own area.
    pub max_filter_area: f64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_bytes: 16 << 20,
            max_nodes: 200_000,
            max_filter_area: 16.0,
        }
    }
}

/// The largest raster side the renderer draws; a deep zoom asks for less.
pub const MAX_RASTER_SIDE: u32 = 8192;
/// The most pixels one raster holds (8K UHD).
pub const MAX_RASTER_PIXELS: u64 = 7680 * 4320;

/// Why a file was refused outright.
#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    /// More bytes than [`Limits::max_bytes`].
    TooManyBytes { bytes: usize, cap: usize },
    /// More elements, `<use>` expanded, than [`Limits::max_nodes`].
    TooManyNodes { nodes: u64, cap: u64 },
    /// A filter region larger than [`Limits::max_filter_area`] pictures.
    FilterRegion { times: f64, cap: f64 },
    /// Not an SVG usvg reads (with its reason).
    Unreadable(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Refusal::TooManyBytes { bytes, cap } => write!(
                f,
                "too large: {:.1} MB, over the {:.0} MB an SVG may be",
                *bytes as f64 / 1_048_576.0,
                *cap as f64 / 1_048_576.0
            ),
            Refusal::TooManyNodes { nodes, cap } => write!(
                f,
                "too large: {nodes} elements with every <use> expanded, over the {cap} an SVG \
                 may draw"
            ),
            Refusal::FilterRegion { times, cap } => write!(
                f,
                "too large: a filter region {times:.0}× the picture, over the {cap:.0}× a \
                 filter may cover"
            ),
            Refusal::Unreadable(why) => write!(f, "not a readable SVG: {why}"),
        }
    }
}

/// Where hrefs may point: files inside `project_root`, relative to
/// `file_dir` (the SVG's own folder).
#[derive(Debug, Clone)]
pub struct Context {
    pub file_dir: PathBuf,
    pub project_root: PathBuf,
}

impl Context {
    /// For a file in a project's `Resources/`.
    pub fn for_resources(resources_dir: &Path) -> Context {
        Context {
            file_dir: resources_dir.to_path_buf(),
            project_root: resources_dir
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| resources_dir.to_path_buf()),
        }
    }
}

/// What the XML says before usvg reads it.
#[derive(Debug, Clone, Default)]
struct Survey {
    nodes: u64,
    text_elements: usize,
    /// SMIL elements or CSS animation in the file — never played.
    animated: bool,
}

/// The cheap pass before the real parse: the byte cap, the XML's own
/// node cap, and the element count with every `<use>` expanded — so a
/// file that fans out exponentially is refused before usvg builds it.
fn survey(bytes: &[u8], limits: &Limits) -> Result<Survey, Refusal> {
    if bytes.len() > limits.max_bytes {
        return Err(Refusal::TooManyBytes {
            bytes: bytes.len(),
            cap: limits.max_bytes,
        });
    }
    let text = if bytes.starts_with(&[0x1f, 0x8b]) {
        // svgz: usvg inflates it; the expanded size is capped the same.
        return Ok(Survey::default());
    } else {
        std::str::from_utf8(bytes).map_err(|_| Refusal::Unreadable("not UTF-8 text".into()))?
    };
    let options = usvg::roxmltree::ParsingOptions {
        allow_dtd: true,
        nodes_limit: u32::try_from(limits.max_nodes.saturating_mul(4)).unwrap_or(u32::MAX),
        ..Default::default()
    };
    let doc =
        usvg::roxmltree::Document::parse_with_options(text, options).map_err(|e| match e {
            usvg::roxmltree::Error::NodesLimitReached => Refusal::TooManyNodes {
                nodes: limits.max_nodes.saturating_mul(4),
                cap: limits.max_nodes,
            },
            other => Refusal::Unreadable(other.to_string()),
        })?;
    let mut ids = std::collections::HashMap::new();
    let mut text_elements = 0;
    let mut animated = false;
    for node in doc.descendants().filter(|n| n.is_element()) {
        if let Some(id) = node.attribute("id") {
            ids.entry(id.to_string()).or_insert(node.id());
        }
        match node.tag_name().name() {
            "text" => text_elements += 1,
            "animate" | "animateTransform" | "animateMotion" | "animateColor" | "set" => {
                animated = true
            }
            "style" => {
                let css: String = node.descendants().filter_map(|n| n.text()).collect();
                if css.contains("@keyframes") || css.contains("animation") {
                    animated = true;
                }
            }
            _ => {}
        }
    }
    let mut memo = std::collections::HashMap::new();
    let mut visiting = std::collections::HashSet::new();
    let nodes = expanded(doc.root_element(), &ids, &doc, &mut memo, &mut visiting);
    if nodes > limits.max_nodes {
        return Err(Refusal::TooManyNodes {
            nodes,
            cap: limits.max_nodes,
        });
    }
    Ok(Survey {
        nodes,
        text_elements,
        animated,
    })
}

/// Elements under `node`, each `<use>` counting its target again.
fn expanded(
    node: usvg::roxmltree::Node,
    ids: &std::collections::HashMap<String, usvg::roxmltree::NodeId>,
    doc: &usvg::roxmltree::Document,
    memo: &mut std::collections::HashMap<usvg::roxmltree::NodeId, u64>,
    visiting: &mut std::collections::HashSet<usvg::roxmltree::NodeId>,
) -> u64 {
    if let Some(&known) = memo.get(&node.id()) {
        return known;
    }
    if !visiting.insert(node.id()) {
        // A reference cycle: usvg skips it, so it adds nothing.
        return 0;
    }
    let mut count: u64 = 1;
    for child in node.children().filter(|n| n.is_element()) {
        count = count.saturating_add(expanded(child, ids, doc, memo, visiting));
    }
    if node.tag_name().name() == "use" {
        let href = node
            .attribute(("http://www.w3.org/1999/xlink", "href"))
            .or_else(|| node.attribute("href"));
        if let Some(target) = href
            .and_then(|h| h.strip_prefix('#'))
            .and_then(|id| ids.get(id))
            .and_then(|&id| doc.get_node(id))
        {
            count = count.saturating_add(expanded(target, ids, doc, memo, visiting));
        }
    }
    visiting.remove(&node.id());
    memo.insert(node.id(), count);
    count
}

/// What a parse noticed: hrefs refused, fonts the import could not find.
#[derive(Debug, Clone, Default)]
pub struct Notes {
    pub refused: Vec<String>,
    pub missing_fonts: Vec<String>,
}

fn sniff(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(b"\x89PNG") {
        Some("png")
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpeg")
    } else if data.starts_with(b"GIF8") {
        Some("gif")
    } else if data.len() > 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

/// The usvg options every parse uses: hrefs guarded, `fonts` the import's
/// (the machine's) or none at all (a render).
fn options<'a>(
    context: &'a Context,
    limits: &'a Limits,
    notes: &'a Mutex<Notes>,
    fonts: Option<Arc<fontdb::Database>>,
) -> usvg::Options<'a> {
    let mut opt = usvg::Options {
        resources_dir: Some(context.file_dir.clone()),
        ..Default::default()
    };
    let root = std::fs::canonicalize(&context.project_root).ok();
    let refuse = move |href: &str, why: &str| {
        let shown: String = href.chars().take(80).collect();
        let mut notes = notes.lock().unwrap();
        let line = format!("{shown} ({why})");
        if !notes.refused.contains(&line) {
            notes.refused.push(line);
        }
    };
    let nested = move |data: &[u8], opts: &usvg::Options| -> Option<usvg::ImageKind> {
        // A picture inside the picture passes the same guards.
        survey(data, limits).ok()?;
        usvg::Tree::from_data_nested(data, opts)
            .ok()
            .map(usvg::ImageKind::SVG)
    };
    opt.image_href_resolver = usvg::ImageHrefResolver {
        resolve_data: Box::new(move |mime, data, opts| match mime {
            "image/svg+xml" => nested(&data, opts),
            "text/plain" if sniff(&data).is_none() => nested(&data, opts),
            _ => (usvg::ImageHrefResolver::default_data_resolver())(mime, data, opts),
        }),
        resolve_string: Box::new(move |href, opts| {
            let scheme = href
                .split_once(':')
                .is_some_and(|(s, _)| !s.is_empty() && !s.contains('/') && s.len() > 1);
            if scheme || href.starts_with("//") {
                refuse(
                    href,
                    "a link — only data: URIs and files inside the project load",
                );
                return None;
            }
            let path = context.file_dir.join(href);
            let Ok(real) = std::fs::canonicalize(&path) else {
                refuse(href, "no such file in the project");
                return None;
            };
            if !root.as_ref().is_some_and(|r| real.starts_with(r)) {
                refuse(href, "outside the project — only files inside it load");
                return None;
            }
            let data = std::fs::read(&real).ok()?;
            let ext = real
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let data = Arc::new(data);
            match sniff(&data) {
                Some("png") => Some(usvg::ImageKind::PNG(data)),
                Some("jpeg") => Some(usvg::ImageKind::JPEG(data)),
                Some("gif") => Some(usvg::ImageKind::GIF(data)),
                Some("webp") => Some(usvg::ImageKind::WEBP(data)),
                _ if ext == "svg" || ext == "svgz" => nested(&data, opts),
                _ => {
                    refuse(href, "not a PNG, JPEG, GIF, WebP or SVG");
                    None
                }
            }
        }),
    };
    match fonts {
        Some(db) => {
            opt.fontdb = db.clone();
            opt.font_resolver = usvg::FontResolver {
                select_font: Box::new(move |font, db| {
                    let named: Vec<&str> = font
                        .families()
                        .iter()
                        .filter_map(|f| match f {
                            usvg::FontFamily::Named(name) => Some(name.as_str()),
                            _ => None,
                        })
                        .collect();
                    let has = |name: &str| {
                        db.faces().any(|face| {
                            face.families
                                .iter()
                                .any(|(family, _)| family.eq_ignore_ascii_case(name))
                        })
                    };
                    let absent = !named.is_empty() && !named.iter().any(|n| has(n));
                    let chosen = (usvg::FontResolver::default_font_selector())(font, db);
                    let wanted = font
                        .families()
                        .first()
                        .map(|f| f.to_string())
                        .unwrap_or_default();
                    if absent || chosen.is_none() {
                        let mut notes = notes.lock().unwrap();
                        let name = named
                            .first()
                            .map(|s| s.to_string())
                            .unwrap_or(wanted)
                            .trim_matches(|c| c == '"' || c == '\'')
                            .to_string();
                        if !notes.missing_fonts.contains(&name) {
                            notes.missing_fonts.push(name);
                        }
                    }
                    chosen
                }),
                select_fallback: usvg::FontResolver::default_fallback_selector(),
            };
        }
        // A render: no font is ever consulted.
        None => opt.fontdb = Arc::new(fontdb::Database::new()),
    }
    opt
}

/// Every filter's region, as a multiple of the picture's area — the
/// largest one.
fn widest_filter(tree: &usvg::Tree) -> f64 {
    fn walk(group: &usvg::Group, area: f64, widest: &mut f64) {
        for filter in group.filters() {
            if let Some(rect) = filter.rect().transform(group.abs_transform()) {
                let times = (rect.width() as f64 * rect.height() as f64) / area;
                if times > *widest {
                    *widest = times;
                }
            }
        }
        for child in group.children() {
            if let usvg::Node::Group(g) = child {
                walk(g, area, widest);
            }
            child.subroots(|sub| walk(sub, area, widest));
        }
    }
    let size = tree.size();
    let area = (size.width() as f64 * size.height() as f64).max(1.0);
    let mut widest = 0.0;
    walk(tree.root(), area, &mut widest);
    widest
}

fn parse_tree(
    bytes: &[u8],
    context: &Context,
    limits: &Limits,
    fonts: Option<Arc<fontdb::Database>>,
) -> Result<(usvg::Tree, Survey, Notes), Refusal> {
    let survey = survey(bytes, limits)?;
    let notes = Mutex::new(Notes::default());
    let tree = {
        let opt = options(context, limits, &notes, fonts);
        usvg::Tree::from_data(bytes, &opt).map_err(|e| match e {
            usvg::Error::ElementsLimitReached => Refusal::TooManyNodes {
                nodes: 1_000_000,
                cap: limits.max_nodes,
            },
            other => Refusal::Unreadable(other.to_string()),
        })?
    };
    let widest = widest_filter(&tree);
    if widest > limits.max_filter_area {
        return Err(Refusal::FilterRegion {
            times: widest,
            cap: limits.max_filter_area,
        });
    }
    Ok((tree, survey, notes.into_inner().unwrap()))
}

/// The fonts an import turns text into outlines with: this machine's,
/// read once per process.
pub fn system_fonts() -> Arc<fontdb::Database> {
    static FONTS: OnceLock<Arc<fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();
            // fontdb names Microsoft's faces for the generic families; a
            // machine without them (Linux has DejaVu or Liberation) would
            // draw `sans-serif` text in nothing at all. Each generic takes
            // the first of the usual faces this machine has.
            let first = |db: &fontdb::Database, names: &[&str]| {
                names
                    .iter()
                    .find(|name| {
                        db.faces().any(|f| {
                            f.families
                                .iter()
                                .any(|(family, _)| family.eq_ignore_ascii_case(name))
                        })
                    })
                    .map(|name| name.to_string())
            };
            if let Some(name) = first(
                &db,
                &[
                    "Helvetica",
                    "Arial",
                    "Liberation Sans",
                    "DejaVu Sans",
                    "Noto Sans",
                ],
            ) {
                db.set_sans_serif_family(name);
            }
            if let Some(name) = first(
                &db,
                &[
                    "Times New Roman",
                    "Times",
                    "Liberation Serif",
                    "DejaVu Serif",
                    "Noto Serif",
                ],
            ) {
                db.set_serif_family(name);
            }
            if let Some(name) = first(
                &db,
                &[
                    "Courier New",
                    "Menlo",
                    "Liberation Mono",
                    "DejaVu Sans Mono",
                    "Noto Sans Mono",
                ],
            ) {
                db.set_monospace_family(name);
            }
            Arc::new(db)
        })
        .clone()
}

/// SHA-256 of `bytes`, lowercase hex — the outlines' receipt.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What an import learned about a file.
#[derive(Debug, Clone)]
pub struct Imported {
    /// The file's own size in CSS pixels.
    pub size: (f64, f64),
    /// The text-as-paths SVG, when the file has `<text>`.
    pub outlines: Option<String>,
    /// SHA-256 of the file.
    pub source_hash: String,
    pub missing_fonts: Vec<String>,
    pub refused: Vec<String>,
    pub text_elements: usize,
    /// The author's names for unnamed elements: address → name.
    pub handles: std::collections::BTreeMap<String, String>,
}

/// Reads an SVG once, as the import does: the guards, the size, and the
/// outlines with `fonts` (normally [`system_fonts`]).
pub fn import_bytes(
    bytes: &[u8],
    context: &Context,
    fonts: Arc<fontdb::Database>,
) -> Result<Imported, Refusal> {
    import_with_texts(bytes, context, fonts, &[])
}

/// [`import_bytes`] with text elements' words replaced first — `texts`,
/// address → words: the outlines a `text` override draws, made on the
/// machine that edits it. Every unnamed element is named by its address
/// before the parse, so the outlines' nodes can be found by edits.
pub fn import_with_texts(
    bytes: &[u8],
    context: &Context,
    fonts: Arc<fontdb::Database>,
    texts: &[(String, String)],
) -> Result<Imported, Refusal> {
    let limits = Limits::default();
    let elements = std::str::from_utf8(bytes)
        .ok()
        .and_then(|t| model::Survey::read(t).ok().map(|s| (t, s)));
    let named = elements
        .as_ref()
        .map(|(text, survey)| survey.named_source(text, texts));
    let parsed = named.as_deref().map_or(bytes, str::as_bytes);
    let (tree, survey, notes) = parse_tree(parsed, context, &limits, Some(fonts))?;
    let size = (tree.size().width() as f64, tree.size().height() as f64);
    let outlines = (survey.text_elements > 0 || tree.has_text_nodes()).then(|| {
        let written = tree.to_string(&usvg::WriteOptions {
            preserve_text: false,
            indent: usvg::Indent::None,
            ..Default::default()
        });
        // The file itself with each text swapped for its glyphs: every
        // element where it was, named by its address — and marked so.
        // Where a group cannot stand in for a text (one inside a clip, a
        // mask, a definition), usvg's writing of the whole picture.
        named
            .as_deref()
            .and_then(|n| edit::splice_outlines(n, &written))
            .and_then(|spliced| {
                let mut dom = edit::Dom::parse(&spliced).ok()?;
                dom.mark(MARKER, "1");
                Some(dom.write())
            })
            .unwrap_or(written)
    });
    Ok(Imported {
        size,
        outlines,
        source_hash: sha256_hex(bytes),
        missing_fonts: notes.missing_fonts,
        refused: notes.refused,
        text_elements: survey.text_elements,
        handles: elements.map(|(_, s)| s.handles()).unwrap_or_default(),
    })
}

/// The derived outlines file for `filename`, inside `Resources/.svg/`.
pub fn outlines_name(filename: &str) -> String {
    filename.to_string()
}

/// The outlines file for `filename` drawn with `texts` (address → words)
/// in place of the file's words: the plain name when there are none, else
/// one named by them — so an undo that brings the old words back finds
/// their outlines still there.
pub fn outlines_name_for(filename: &str, texts: &[(String, String)]) -> String {
    if texts.is_empty() {
        return outlines_name(filename);
    }
    let mut key = String::new();
    for (address, words) in texts {
        key.push_str(address);
        key.push('\u{1f}');
        key.push_str(words);
        key.push('\u{1e}');
    }
    format!("{}.{filename}", &sha256_hex(key.as_bytes())[..12])
}

/// The `text` overrides of a block, address → words, in their order.
pub fn text_overrides(block: &VectorBlock) -> Vec<(String, String)> {
    block
        .overrides
        .iter()
        .filter_map(|o| o.text.clone().map(|t| (o.node.clone(), t)))
        .collect()
}

/// Imports `Resources/<filename>`: writes its outlines (when it has text)
/// to `Resources/.svg/<filename>` and answers the resource's `svg` block
/// plus the natural size (`pixelWidth`, `pixelHeight`) and what the import
/// noticed: `{"svg": {...}, "pixelWidth", "pixelHeight", "refused": [..],
/// "missingFonts": [..], "textElements": n}`.
pub fn install(resources_dir: &Path, filename: &str) -> Result<Value, String> {
    outline(resources_dir, filename, &[])
}

/// [`install`] with text elements' words replaced (`texts`, address →
/// words): what a `text` override runs on the machine that edits it. The
/// outlines land under [`outlines_name_for`]; outlines for other words
/// stay where they are, for an undo.
pub fn outline(
    resources_dir: &Path,
    filename: &str,
    texts: &[(String, String)],
) -> Result<Value, String> {
    let path = resources_dir.join(filename);
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let imported = import_with_texts(
        &bytes,
        &Context::for_resources(resources_dir),
        system_fonts(),
        texts,
    )
    .map_err(|e| format!("{filename}: {e}"))?;
    let mut block = json!({
        "size": [round(imported.size.0), round(imported.size.1)],
    });
    if !imported.handles.is_empty() {
        block["handles"] = json!(imported.handles);
    }
    let folder = resources_dir.join(OUTLINES_DIR);
    let name = outlines_name_for(filename, texts);
    match &imported.outlines {
        Some(text) => {
            std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
            let target = folder.join(&name);
            let staging = folder.join(format!(".{name}.partial"));
            std::fs::write(&staging, text).map_err(|e| format!("{}: {e}", staging.display()))?;
            std::fs::rename(&staging, &target).map_err(|e| format!("{}: {e}", target.display()))?;
            block["outlines"] = json!(name);
            block["sourceHash"] = json!(imported.source_hash);
        }
        None if texts.is_empty() => {
            // A re-import of a file that lost its text: the old outlines go.
            let _ = std::fs::remove_file(folder.join(&name));
        }
        None => {}
    }
    if !imported.missing_fonts.is_empty() {
        block["missingFonts"] = json!(imported.missing_fonts);
    }
    Ok(json!({
        "svg": block,
        "pixelWidth": round(imported.size.0),
        "pixelHeight": round(imported.size.1),
        "refused": imported.refused,
        "missingFonts": imported.missing_fonts,
        "textElements": imported.text_elements,
    }))
}

fn round(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// A parsed SVG, ready to draw at any size. Built with no fonts.
#[derive(Clone)]
pub struct Picture {
    tree: Arc<usvg::Tree>,
    size: (f64, f64),
    stamp: u64,
    refused: Vec<String>,
    /// `<text>` elements in the bytes this was parsed from.
    text_elements: usize,
    /// Elements with every `<use>` expanded.
    elements: u64,
    /// Whether the file animates itself (SMIL or CSS).
    animated: bool,
    /// The bytes parsed, and — when those are its outlines — the file.
    source: Arc<Vec<u8>>,
    original: Option<Arc<Vec<u8>>>,
    context: Context,
    /// The element tree and the drawn tree its edits apply to, built on
    /// first use.
    edits: Arc<OnceLock<Result<EditBase, String>>>,
}

#[derive(Clone)]
struct EditBase {
    model: Arc<Model>,
    /// The XML edits apply to: the file with every unnamed element named,
    /// or its marked outlines.
    source: Arc<String>,
}

/// The bytes a vector resource draws from: its outlines while they are
/// current, else the file; and the file itself.
#[derive(Debug, Clone)]
pub struct Sources {
    pub render: Vec<u8>,
    pub from_outlines: bool,
    pub original: Vec<u8>,
}

impl std::fmt::Debug for Picture {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("Picture")
            .field("size", &self.size)
            .field("stamp", &self.stamp)
            .finish()
    }
}

/// FNV-1a over the bytes: a stable content stamp for cache keys.
fn fnv(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

impl Picture {
    /// Parses `bytes` for drawing: the guards, hrefs inside `context`'s
    /// project only, and no fonts — text in a file without outlines draws
    /// nothing.
    pub fn parse(bytes: &[u8], context: &Context) -> Result<Picture, Refusal> {
        Self::parse_inner(bytes, None, context)
    }

    /// Parses what [`sources_at`] read: the outlines (or the file), with
    /// the file beside them for the element tree.
    pub fn from_sources(sources: Sources, context: &Context) -> Result<Picture, Refusal> {
        let original = sources.from_outlines.then_some(sources.original);
        Self::parse_inner(&sources.render, original, context)
    }

    fn parse_inner(
        bytes: &[u8],
        original: Option<Vec<u8>>,
        context: &Context,
    ) -> Result<Picture, Refusal> {
        let (tree, survey, notes) = parse_tree(bytes, context, &Limits::default(), None)?;
        let size = (tree.size().width() as f64, tree.size().height() as f64);
        Ok(Picture {
            tree: Arc::new(tree),
            size,
            stamp: fnv(bytes),
            refused: notes.refused,
            text_elements: original
                .as_deref()
                .and_then(survey_text_elements)
                .unwrap_or(survey.text_elements),
            elements: survey.nodes,
            animated: survey.animated,
            source: Arc::new(bytes.to_vec()),
            original: original.map(Arc::new),
            context: context.clone(),
            edits: Arc::new(OnceLock::new()),
        })
    }

    fn edit_base(&self) -> Result<&EditBase, String> {
        self.edits
            .get_or_init(|| {
                let file = self.original.as_deref().unwrap_or(&self.source);
                let text = std::str::from_utf8(file)
                    .map_err(|_| "a compressed SVG's elements are not listed".to_string())?;
                let survey = model::Survey::read(text)?;
                let marked = contains(&self.source, MARKER.as_bytes());
                // What edits change: the marked outlines, or the file
                // named. Outlines without the mark (made before element
                // edits, or usvg's whole writing) list the file's elements
                // against the file named — its text drawing nothing — and
                // take no edit.
                let (source, editable) = match (&self.original, marked) {
                    (Some(_), true) => (String::from_utf8_lossy(&self.source).into_owned(), true),
                    (Some(_), false) => (survey.named_source(text, &[]), false),
                    (None, _) => (survey.named_source(text, &[]), true),
                };
                let (tree, probes) = match edit::probed(&source) {
                    Ok((probed, probes)) => (
                        parse_tree(probed.as_bytes(), &self.context, &Limits::default(), None)
                            .ok()
                            .map(|(t, _, _)| t),
                        probes,
                    ),
                    Err(_) => (None, Default::default()),
                };
                let editable = editable && tree.is_some();
                let model = Model::build(survey, tree.as_ref(), &probes, editable);
                Ok(EditBase {
                    model: Arc::new(model),
                    source: Arc::new(source),
                })
            })
            .as_ref()
            .map_err(|e| e.clone())
    }

    /// The file's elements, classified (SVG-PLAN.md slice 2).
    pub fn model(&self) -> Result<Arc<Model>, String> {
        self.edit_base().map(|b| b.model.clone())
    }

    /// This picture with `overrides` applied to its tree — itself when
    /// none changes the tree. An override the element's tier refuses, or
    /// one naming no element, is skipped (validate names it). The stamp
    /// names the edits, so a raster of the edited picture never answers
    /// for the plain one.
    pub fn with_overrides(&self, overrides: &[VectorOverride]) -> Result<Picture, String> {
        let edits: Vec<&VectorOverride> = overrides.iter().filter(|o| o.edits_tree()).collect();
        if edits.is_empty() {
            return Ok(self.clone());
        }
        let base = self.edit_base()?;
        if !base.model.editable() {
            return Err(
                "this SVG's text outlines were made before its elements could be \
                        edited — import it again"
                    .into(),
            );
        }
        let text = edit::apply(&base.source, &base.model, overrides)?;
        let (tree, _, _) = parse_tree(text.as_bytes(), &self.context, &Limits::default(), None)
            .map_err(|e| e.to_string())?;
        let mut stamped = self.stamp.to_le_bytes().to_vec();
        stamped.extend(serde_json::to_vec(&edits).unwrap_or_default());
        Ok(Picture {
            tree: Arc::new(tree),
            stamp: fnv(&stamped),
            ..self.clone()
        })
    }

    /// The natural size in CSS pixels.
    pub fn size(&self) -> (f64, f64) {
        self.size
    }

    /// A stamp of the bytes this was parsed from.
    pub fn stamp(&self) -> u64 {
        self.stamp
    }

    /// Hrefs the parse refused.
    pub fn refused(&self) -> &[String] {
        &self.refused
    }

    /// `<text>` elements in what was parsed (none in an outlines file).
    pub fn text_elements(&self) -> usize {
        self.text_elements
    }

    /// Whether the file animates itself (SMIL elements or CSS animation).
    /// Never played: it draws as its still frame, and motion comes from
    /// the layer's keyframes.
    pub fn animated(&self) -> bool {
        self.animated
    }

    /// Elements, every `<use>` expanded — what a render walks.
    pub fn elements(&self) -> u64 {
        self.elements
    }

    /// The parsed tree.
    pub fn tree(&self) -> &usvg::Tree {
        &self.tree
    }

    /// Draws the picture — or the part `crop` names, `[x, y, w, h]` in unit
    /// fractions of the picture — into `width`×`height` premultiplied RGBA
    /// rows, stretched to fill (the caller keeps the aspect). `None` for an
    /// empty or over-cap size.
    pub fn render(&self, width: u32, height: u32, crop: Option<[f64; 4]>) -> Option<Vec<u8>> {
        if width == 0
            || height == 0
            || width > MAX_RASTER_SIDE
            || height > MAX_RASTER_SIDE
            || width as u64 * height as u64 > MAX_RASTER_PIXELS
        {
            return None;
        }
        let [cx, cy, cw, ch] = crop.unwrap_or([0.0, 0.0, 1.0, 1.0]);
        if cw <= 0.0 || ch <= 0.0 {
            return None;
        }
        let (w, h) = self.size;
        let sx = width as f64 / (cw * w);
        let sy = height as f64 / (ch * h);
        let transform = tiny_skia::Transform::from_row(
            sx as f32,
            0.0,
            0.0,
            sy as f32,
            (-cx * w * sx) as f32,
            (-cy * h * sy) as f32,
        );
        let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
        resvg::render(&self.tree, transform, &mut pixmap.as_mut());
        Some(pixmap.take())
    }

    /// Draws the whole picture fitted inside `max_side`×`max_side`, keeping
    /// its aspect: `(rgba, width, height)` — a thumbnail.
    pub fn render_fit(&self, max_side: u32) -> Option<(Vec<u8>, u32, u32)> {
        let (w, h) = self.size;
        let scale = max_side as f64 / w.max(h).max(1e-6);
        let pw = ((w * scale).round() as u32).max(1);
        let ph = ((h * scale).round() as u32).max(1);
        self.render(pw, ph, None).map(|px| (px, pw, ph))
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn survey_text_elements(bytes: &[u8]) -> Option<usize> {
    survey(bytes, &Limits::default())
        .ok()
        .map(|s| s.text_elements)
}

/// The stamp the picture drawn with `overrides` carries — `stamp` itself
/// when none changes its tree. What a host compares to skip work.
pub fn edited_stamp(stamp: u64, overrides: &[VectorOverride]) -> u64 {
    let edits: Vec<&VectorOverride> = overrides.iter().filter(|o| o.edits_tree()).collect();
    if edits.is_empty() {
        return stamp;
    }
    let mut stamped = stamp.to_le_bytes().to_vec();
    stamped.extend(serde_json::to_vec(&edits).unwrap_or_default());
    fnv(&stamped)
}

/// Which bytes draw a vector resource: its outlines while they are the
/// original's (the block's `sourceHash`), else the original itself.
/// `(bytes, from_outlines)`.
pub fn render_source(
    resources_dir: &Path,
    resource: &ProjectResource,
) -> Result<(Vec<u8>, bool), String> {
    render_source_at(
        &resources_dir.join(&resource.filename),
        promo_model::vector::block(resource).as_ref(),
    )
}

/// [`render_source`] for the file at `path` (its outlines beside it, in
/// `.svg/`) and the block it was imported with.
pub fn render_source_at(
    path: &Path,
    block: Option<&VectorBlock>,
) -> Result<(Vec<u8>, bool), String> {
    sources_at(path, block).map(|s| {
        if s.from_outlines {
            (s.render, true)
        } else {
            (s.original, false)
        }
    })
}

/// [`render_source_at`] with the file kept beside its outlines.
pub fn sources_at(path: &Path, block: Option<&VectorBlock>) -> Result<Sources, String> {
    let original = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if let Some((Some(name), Some(hash))) =
        block.map(|b| (b.outlines.as_deref(), b.source_hash.as_deref()))
    {
        let outlines = path
            .parent()
            .unwrap_or(Path::new("."))
            .join(OUTLINES_DIR)
            .join(name);
        if let Ok(bytes) = std::fs::read(&outlines) {
            if sha256_hex(&original) == hash {
                return Ok(Sources {
                    render: bytes,
                    from_outlines: true,
                    original,
                });
            }
        }
    }
    Ok(Sources {
        render: Vec::new(),
        from_outlines: false,
        original,
    })
}

/// The picture a vector resource draws.
pub fn load(resources_dir: &Path, resource: &ProjectResource) -> Result<Picture, String> {
    load_path(
        &resources_dir.join(&resource.filename),
        promo_model::vector::block(resource).as_ref(),
    )
    .map_err(|e| format!("{}: {e}", resource.filename))
}

/// The picture the file at `path` draws, with the block it was imported
/// with; hrefs may reach the project folder above its `Resources/`.
pub fn load_path(path: &Path, block: Option<&VectorBlock>) -> Result<Picture, String> {
    let mut sources = sources_at(path, block)?;
    if !sources.from_outlines {
        sources.render = std::mem::take(&mut sources.original);
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    Picture::from_sources(sources, &Context::for_resources(dir)).map_err(|e| e.to_string())
}

/// [`load_path`] with the block's overrides applied — the resource's
/// look, as a thumbnail or the vector editor shows it.
pub fn load_edited(path: &Path, block: Option<&VectorBlock>) -> Result<Picture, String> {
    let picture = load_path(path, block)?;
    match block {
        Some(b) if !b.overrides.is_empty() => picture.with_overrides(&b.overrides),
        _ => Ok(picture),
    }
}

/// The stamp a picture parsed from these bytes carries — to skip a parse
/// when the engine already holds them.
pub fn stamp(bytes: &[u8]) -> u64 {
    fnv(bytes)
}

/// What validate says about a project's vector resources, read from the
/// files: refused hrefs, a file over the caps, text that will not draw,
/// the fonts the import could not find. Warnings, one line each.
pub fn findings(meta: &ProjectMetadata, resources_dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for resource in meta.resources.iter().flatten() {
        if resource.kind != ProjectResourceKind::Vector {
            continue;
        }
        let name = &resource.display_name;
        let path = resources_dir.join(&resource.filename);
        let Ok(original) = std::fs::read(&path) else {
            // Missing media is named by the layer checks.
            continue;
        };
        let context = Context::for_resources(resources_dir);
        let parsed = match Picture::parse(&original, &context) {
            Ok(picture) => picture,
            Err(why) => {
                out.push(format!(
                    "the SVG \"{name}\" ({}) draws nothing — {why}",
                    resource.filename
                ));
                continue;
            }
        };
        for href in parsed.refused() {
            out.push(format!(
                "the SVG \"{name}\" names {href}: refused, that part draws nothing"
            ));
        }
        if parsed.animated() {
            out.push(format!(
                "the SVG \"{name}\" animates itself (SMIL or CSS animation), which does not \
                 play: it draws as its still frame — move it with keyframes"
            ));
        }
        let block = promo_model::vector::block(resource);
        if parsed.text_elements() > 0 {
            match render_source(resources_dir, resource) {
                Ok((_, true)) => {}
                _ => out.push(format!(
                    "the SVG \"{name}\" has text but no current outlines, so its text draws \
                     nothing — import it (`promo resource add`, or Resources in the app) to \
                     turn the text into shapes"
                )),
            }
        }
        for font in block
            .as_ref()
            .and_then(|b| b.missing_fonts.clone())
            .unwrap_or_default()
        {
            out.push(format!(
                "the SVG \"{name}\" asks for the font \"{font}\", which the machine that \
                 imported it lacked — that text is drawn in a fallback face; install the font \
                 and import it again"
            ));
        }
        // Overrides: each names an element and stays inside its tier; a
        // word change is drawn from outlines made with those words.
        if let Some(b) = block.as_ref().filter(|b| !b.overrides.is_empty()) {
            let drawn = load(resources_dir, resource);
            let model = drawn.as_ref().ok().and_then(|p| p.model().ok());
            for o in &b.overrides {
                let Some(model) = &model else { break };
                if let Err(why) = model.check(o, false) {
                    if model.resolve(&o.node).is_err() {
                        out.push(format!(
                            "the SVG \"{name}\" has an override for \"{}\", which names no \
                             element of the file — it does nothing; remove it (Reset in the \
                             app) or name an element promo_inspect lists",
                            o.node
                        ));
                    } else {
                        out.push(format!(
                            "the SVG \"{name}\" has an override that does not apply — {why}"
                        ));
                    }
                }
            }
            let texts = text_overrides(b);
            if !texts.is_empty()
                && b.outlines.as_deref()
                    != Some(outlines_name_for(&resource.filename, &texts).as_str())
            {
                out.push(format!(
                    "the SVG \"{name}\" has new words for its text that are not outlined yet, so \
                     the old words draw — set them again with promo_apply (setVectorOverride) or \
                     in the app"
                ));
            }
        }
        if block.and_then(|b| b.natural_size()).is_none()
            && (resource.pixel_width.is_none() || resource.pixel_height.is_none())
        {
            let (w, h) = parsed.size();
            out.push(format!(
                "the SVG \"{name}\" has no stated size — import it, or set \"svg\": \
                 {{\"size\": [{w}, {h}]}} with pixelWidth/pixelHeight, so placement reads its \
                 shape"
            ));
        }
    }
    out
}

/// The block a resource should carry after an import — [`install`]'s
/// answer applied to a resource JSON in place: kind `vector`, the `svg`
/// block, the natural size.
pub fn apply_install(resource: &mut Value, installed: &Value) {
    resource["kind"] = json!("vector");
    resource[KEY] = installed["svg"].clone();
    resource["pixelWidth"] = installed["pixelWidth"].clone();
    resource["pixelHeight"] = installed["pixelHeight"].clone();
}

/// The natural size a vector resource lays out with: the block's, the
/// stamped pixel size, or `None`.
pub fn natural_size(resource: &ProjectResource) -> Option<(f64, f64)> {
    let stamped = match (resource.pixel_width, resource.pixel_height) {
        (Some(w), Some(h)) if w > 0.0 && h > 0.0 => Some((w, h)),
        _ => None,
    };
    promo_model::vector::block(resource)
        .and_then(|b: VectorBlock| b.natural_size())
        .or(stamped)
}

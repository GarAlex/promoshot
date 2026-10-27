//! The element tree, its tiers and their reasons (SVG-PLAN.md slice 2).
//!
//! The editor restricts the CONTROLS, never the picture: every element of
//! the file is listed with a tier and a plain-words reason, and the apps,
//! the MCP servers and validate all read this one report.
//!
//! - **A** — any drawn element: visible, opacity, a move/scale/turn about
//!   its own centre, its order among its siblings.
//! - **B** — a painted shape (solid, linear or radial paint): A plus fill
//!   and stroke colours, gradient stops and stroke width.
//! - **C** — a B shape with no clip, mask or filter on it or above it and
//!   not a `<use>` copy: plain geometry. Reported only; point editing is
//!   slice 5's.
//! - **Locked** — what the picture depends on in ways an edit could not
//!   keep: the inside of a mask, a clip, a pattern tile, a marker, a
//!   `<use>` original, what a filter draws; and what the file never draws.
//!
//! **Addresses.** An element's address is its `id`, else its path from
//! the nearest ancestor that has one — `badge/rect[0]`, `g[2]/path[0]`:
//! the tag and its index among its parent's elements of that tag. The file
//! never changes under a resource (a new file is a new import), so the
//! path is stable; the author's names for unnamed elements (`inkscape:
//! label`, `data-name`, a `<title>`) are kept on import as the block's
//! `handles`, and a tool may name an element by them.
//!
//! **Mapping to the drawn tree.** usvg keeps every `<g>` and `<use>` and
//! every element's `id`, so an id injected on each unnamed element before
//! the parse (`promo:<address>`, never matched by an author's CSS) names
//! the node it became. The import's text outlines are the same file with
//! each `<text>` swapped for its glyphs, so they carry the same names (and
//! say so with [`MARKER`]).

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use serde::Serialize;
use usvg::roxmltree;

use promo_model::vector::VectorOverride;

/// The prefix of an injected id.
pub const SYNTHETIC: &str = "promo:";
/// The attribute an outlines file carries when its nodes are named by
/// address.
pub const MARKER: &str = "data-promo-nodes";

const SVG_NS: &str = "http://www.w3.org/2000/svg";
const INKSCAPE_NS: &str = "http://www.inkscape.org/namespaces/inkscape";
const XLINK_NS: &str = "http://www.w3.org/1999/xlink";

/// What an element is, as a row of the list.
fn row_kind(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "g" | "a" => "group",
        "path" | "rect" | "circle" | "ellipse" | "line" | "polyline" | "polygon" => "shape",
        "image" => "image",
        "text" => "text",
        "use" => "use",
        "svg" => "svg",
        "switch" => "switch",
        "defs" => "defs",
        "symbol" => "symbol",
        "clipPath" => "clip",
        "mask" => "mask",
        "pattern" => "pattern",
        "marker" => "marker",
        "filter" => "filter",
        _ => return None,
    })
}

/// The elements usvg keeps the id of — the ones an injected id can name.
fn takes_id(tag: &str) -> bool {
    matches!(
        tag,
        "g" | "a"
            | "path"
            | "rect"
            | "circle"
            | "ellipse"
            | "line"
            | "polyline"
            | "polygon"
            | "image"
            | "text"
            | "use"
    )
}

/// A tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Tier {
    A,
    B,
    C,
    #[serde(rename = "locked")]
    Locked,
}

impl Tier {
    /// Whether paint may change.
    pub fn paints(self) -> bool {
        matches!(self, Tier::B | Tier::C)
    }
}

/// A shape's paint, as the file draws it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaintInfo {
    /// `color`, `linear`, `radial` or `pattern`.
    pub kind: &'static str,
    /// `RRGGBB`, for a colour.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// The paint's own opacity (fill-opacity / stroke-opacity).
    pub opacity: f64,
    /// A gradient's stops, `RRGGBBAA` each.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stops: Vec<String>,
}

/// One row of the element list.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    /// The address (`id`, else a path).
    pub node: String,
    /// The author's name for it, when the file gives one besides the id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// group, shape, image, text, use, svg, link, switch, defs, symbol,
    /// clip, mask, pattern, marker, filter.
    pub kind: &'static str,
    /// The element's tag in the file (`rect`, `circle`, …).
    pub tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub depth: usize,
    pub tier: Tier,
    /// Why this tier, in plain words.
    pub reason: String,
    /// Whether it draws anything (a definition, a hidden or empty element
    /// does not).
    pub drawn: bool,
    /// Its place among its drawn siblings (0 = drawn first) and how many
    /// there are — what `order` moves within.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub siblings: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill: Option<PaintInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stroke: Option<PaintInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stroke_width: Option<f64>,
    /// A text element's words, as the file has them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// `[x, y, w, h]` in the picture's CSS pixels.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<[f64; 4]>,
}

/// Where a drawn node sits — what an edit needs to move, turn or
/// reorder it, and to copy its gradients.
#[derive(Debug, Clone)]
pub(crate) struct Place {
    /// The absolute transform of its parent element's user space.
    pub parent_abs: usvg::Transform,
    /// The node's centre on the picture.
    pub centre: (f32, f32),
    /// Its gradients, resolved, for a copy with other stops.
    pub fill_gradient: Option<Gradient>,
    pub stroke_gradient: Option<Gradient>,
    /// The ids of its drawn siblings' elements, in order, itself among
    /// them — what `order` moves within.
    pub sibling_ids: Vec<String>,
}

/// A gradient as usvg resolved it: in the shape's own user space.
#[derive(Debug, Clone)]
pub(crate) struct Gradient {
    pub radial: bool,
    /// x1 y1 x2 y2, or cx cy r fx fy fr.
    pub coords: Vec<f32>,
    pub transform: usvg::Transform,
    pub spread: &'static str,
    /// offset, colour, opacity.
    pub stops: Vec<(f32, [u8; 3], f32)>,
}

fn gradient_of(paint: &usvg::Paint) -> Option<Gradient> {
    let (radial, coords, base_stops, transform, spread) = match paint {
        usvg::Paint::LinearGradient(g) => (
            false,
            vec![g.x1(), g.y1(), g.x2(), g.y2()],
            g.stops(),
            g.transform(),
            g.spread_method(),
        ),
        usvg::Paint::RadialGradient(g) => (
            true,
            vec![g.cx(), g.cy(), g.r().get(), g.fx(), g.fy(), g.fr().get()],
            g.stops(),
            g.transform(),
            g.spread_method(),
        ),
        _ => return None,
    };
    Some(Gradient {
        radial,
        coords,
        transform,
        spread: match spread {
            usvg::SpreadMethod::Pad => "pad",
            usvg::SpreadMethod::Reflect => "reflect",
            usvg::SpreadMethod::Repeat => "repeat",
        },
        stops: base_stops
            .iter()
            .map(|s| {
                let c = s.color();
                (
                    s.offset().get(),
                    [c.red, c.green, c.blue],
                    s.opacity().get(),
                )
            })
            .collect(),
    })
}

/// An element of the file, before mapping.
#[derive(Debug, Clone)]
struct Row {
    address: String,
    name: Option<String>,
    tag: String,
    kind: &'static str,
    parent: Option<usize>,
    depth: usize,
    /// Where an id goes in the start tag, or the value range of an id to
    /// replace (an empty or repeated one).
    id_insert: Option<IdSpot>,
    locked: Option<String>,
    text: Option<String>,
    /// A text element's character data, in the file.
    text_ranges: Vec<Range<usize>>,
}

#[derive(Debug, Clone)]
enum IdSpot {
    Insert(usize),
    Replace(Range<usize>),
}

/// The file's elements: addresses, names, what locks them.
#[derive(Debug, Clone)]
pub(crate) struct Survey {
    rows: Vec<Row>,
    by_address: HashMap<String, usize>,
}

fn label_of(node: roxmltree::Node) -> Option<String> {
    let named = node
        .attribute((INKSCAPE_NS, "label"))
        .or_else(|| node.attribute("data-name"))
        .or_else(|| node.attribute("aria-label"))
        .map(str::to_string)
        .or_else(|| {
            node.children()
                .find(|c| c.is_element() && c.tag_name().name() == "title")
                .map(|t| {
                    t.descendants()
                        .filter(|d| d.is_text())
                        .filter_map(|d| d.text())
                        .collect::<String>()
                })
        })?;
    let named = named.trim().to_string();
    (!named.is_empty()).then_some(named)
}

fn href_of<'a>(node: roxmltree::Node<'a, 'a>) -> Option<&'a str> {
    node.attribute((XLINK_NS, "href"))
        .or_else(|| node.attribute("href"))
        .and_then(|h| h.trim().strip_prefix('#'))
}

/// The kind of effect a `<filter>` element is, in words.
fn filter_words(node: roxmltree::Node) -> &'static str {
    let tags: Vec<&str> = node
        .descendants()
        .filter(|n| n.is_element())
        .map(|n| n.tag_name().name())
        .collect();
    let has = |t: &str| tags.contains(&t);
    if has("feDropShadow") || (has("feGaussianBlur") && has("feOffset")) {
        "a drop shadow filter"
    } else if has("feGaussianBlur") {
        "a blur filter"
    } else if has("feColorMatrix") || has("feComponentTransfer") {
        "a colour filter"
    } else if has("feTurbulence") {
        "a noise filter"
    } else if has("feImage") {
        "an image filter"
    } else {
        "a filter"
    }
}

impl Survey {
    /// Reads the file's elements. `text` is the file itself — never its
    /// outlines.
    pub fn read(text: &str) -> Result<Survey, String> {
        let options = roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        };
        let doc =
            roxmltree::Document::parse_with_options(text, options).map_err(|e| e.to_string())?;
        let root = doc.root_element();
        // Every id, first occurrence; what <use> and feImage point at.
        let mut ids: HashMap<&str, roxmltree::NodeId> = HashMap::new();
        for n in doc.descendants().filter(|n| n.is_element()) {
            if let Some(id) = n.attribute("id").filter(|i| !i.is_empty()) {
                ids.entry(id).or_insert(n.id());
            }
        }
        let mut used: HashSet<roxmltree::NodeId> = HashSet::new();
        let mut filtered: HashSet<roxmltree::NodeId> = HashSet::new();
        for n in doc.descendants().filter(|n| n.is_element()) {
            let set = match n.tag_name().name() {
                "use" => &mut used,
                "feImage" => &mut filtered,
                _ => continue,
            };
            if let Some(&target) = href_of(n).and_then(|h| ids.get(h)) {
                set.insert(target);
            }
        }
        let mut survey = Survey {
            rows: Vec::new(),
            by_address: HashMap::new(),
        };
        let mut seen_ids: HashSet<String> = HashSet::new();
        survey.walk(root, None, 0, None, &used, &filtered, &mut seen_ids, text);
        Ok(survey)
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &mut self,
        parent_el: roxmltree::Node,
        parent: Option<usize>,
        depth: usize,
        inherited: Option<&str>,
        used: &HashSet<roxmltree::NodeId>,
        filtered: &HashSet<roxmltree::NodeId>,
        seen_ids: &mut HashSet<String>,
        text: &str,
    ) {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for el in parent_el.children().filter(|n| n.is_element()) {
            if el.tag_name().namespace() != Some(SVG_NS) {
                continue;
            }
            let tag = el.tag_name().name();
            let Some(kind) = row_kind(tag) else {
                continue;
            };
            let index = {
                let c = counts.entry(tag).or_insert(0);
                *c += 1;
                *c - 1
            };
            let own_id = el.attribute("id").filter(|i| {
                !i.is_empty() && !i.starts_with(SYNTHETIC) && !i.contains('/') && !i.contains('[')
            });
            let unique = own_id.filter(|id| seen_ids.insert(id.to_string()));
            let address = match unique {
                Some(id) => id.to_string(),
                None => {
                    let base = parent.map(|p| self.rows[p].address.as_str());
                    match base {
                        Some(b) => format!("{b}/{tag}[{index}]"),
                        None => format!("{tag}[{index}]"),
                    }
                }
            };
            let id_insert = if unique.is_some() || !takes_id(tag) {
                None
            } else if let Some(attr) = el
                .attributes()
                .find(|a| a.name() == "id" && a.namespace().is_none())
            {
                Some(IdSpot::Replace(attr.range_value()))
            } else {
                // After `<` and the qualified tag name.
                let start = el.range().start + 1;
                let len = text[start..]
                    .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
                    .unwrap_or(0);
                Some(IdSpot::Insert(start + len))
            };
            // What locks it: the nearest reason, a plain definition giving
            // way to a more particular one.
            let own: Option<String> = match tag {
                "clipPath" => Some("kept as drawn — used as a clip".into()),
                "mask" => Some("kept as drawn — used as a mask".into()),
                "pattern" => Some("kept as drawn — a pattern tile".into()),
                "marker" => Some("kept as drawn — a marker, drawn at a path's points".into()),
                "filter" => Some(format!("kept as drawn — {}", filter_words(el))),
                _ if used.contains(&el.id()) => {
                    Some("kept as drawn — copied elsewhere with <use>".into())
                }
                _ if filtered.contains(&el.id()) => {
                    Some("kept as drawn — drawn by a filter".into())
                }
                _ => None,
            };
            const DEFINITION: &str = "kept as drawn — a definition, not drawn itself";
            // The innermost reason wins; a definition says so only when
            // nothing more particular does.
            let locked = own
                .or_else(|| inherited.map(str::to_string))
                .or_else(|| matches!(tag, "defs" | "symbol").then(|| DEFINITION.to_string()));
            let (words, text_ranges) = if tag == "text" {
                let nodes: Vec<roxmltree::Node> =
                    el.descendants().filter(|d| d.is_text()).collect();
                let words: String = nodes.iter().filter_map(|d| d.text()).collect();
                let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
                (Some(words), nodes.iter().map(|d| d.range()).collect())
            } else {
                (None, Vec::new())
            };
            let row = Row {
                address: address.clone(),
                name: label_of(el).filter(|n| *n != address),
                tag: tag.to_string(),
                kind,
                parent,
                depth,
                id_insert,
                locked: locked.clone(),
                text: words,
                text_ranges,
            };
            let at = self.rows.len();
            self.rows.push(row);
            self.by_address.entry(address).or_insert(at);
            // A text element's spans are its words, not rows.
            if tag != "text" {
                self.walk(
                    el,
                    Some(at),
                    depth + 1,
                    locked.as_deref(),
                    used,
                    filtered,
                    seen_ids,
                    text,
                );
            }
        }
    }

    /// The file with an id on every unnamed element the drawn tree keeps,
    /// and — `texts`, address → words — text elements' words replaced.
    pub fn named_source(&self, text: &str, texts: &[(String, String)]) -> String {
        let mut edits: Vec<(Range<usize>, String)> = Vec::new();
        for row in &self.rows {
            match &row.id_insert {
                Some(IdSpot::Insert(at)) => edits.push((
                    *at..*at,
                    format!(" id=\"{}\"", escape(&format!("{SYNTHETIC}{}", row.address))),
                )),
                Some(IdSpot::Replace(range)) => edits.push((
                    range.clone(),
                    escape(&format!("{SYNTHETIC}{}", row.address)),
                )),
                None => {}
            }
            if let Some((_, words)) = texts.iter().find(|(a, _)| *a == row.address) {
                // The words go where the first words were (a tspan's
                // position and style kept); the rest of the text empties.
                let first = row
                    .text_ranges
                    .iter()
                    .position(|r| !text[r.clone()].trim().is_empty())
                    .or((!row.text_ranges.is_empty()).then_some(0));
                for (i, range) in row.text_ranges.iter().enumerate() {
                    let new = if Some(i) == first {
                        escape(words)
                    } else {
                        String::new()
                    };
                    edits.push((range.clone(), new));
                }
            }
        }
        edits.sort_by_key(|(r, _)| (r.start, r.end));
        let mut out = String::with_capacity(text.len() + edits.len() * 24);
        let mut at = 0;
        for (range, new) in edits {
            if range.start < at {
                continue;
            }
            out.push_str(&text[at..range.start]);
            out.push_str(&new);
            at = range.end;
        }
        out.push_str(&text[at..]);
        out
    }

    /// address → author name, for every element with one.
    pub fn handles(&self) -> std::collections::BTreeMap<String, String> {
        self.rows
            .iter()
            .filter_map(|r| r.name.clone().map(|n| (r.address.clone(), n)))
            .collect()
    }
}

pub(crate) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// The prefix of a probe group's id: one empty group first in each
/// parent, whose absolute transform is that parent's user space.
pub(crate) const PROBE: &str = "promo:probe:";

/// The id the element at `address` carries in a named source: the address
/// itself when it is the author's id (an id never holds a `[`), else the
/// injected one.
pub(crate) fn element_id(address: &str) -> String {
    if address.contains('[') {
        format!("{SYNTHETIC}{address}")
    } else {
        address.to_string()
    }
}

/// The address a drawn node's id names, if any.
pub(crate) fn address_of(id: &str) -> Option<&str> {
    if id.is_empty() {
        None
    } else {
        Some(id.strip_prefix(SYNTHETIC).unwrap_or(id))
    }
}

pub(crate) fn hex(c: usvg::Color) -> String {
    format!("{:02X}{:02X}{:02X}", c.red, c.green, c.blue)
}

fn paint_info(paint: &usvg::Paint, opacity: f32) -> PaintInfo {
    let stops = |stops: &[usvg::Stop]| -> Vec<String> {
        stops
            .iter()
            .map(|s| {
                format!(
                    "{}{:02X}",
                    hex(s.color()),
                    (s.opacity().get() * 255.0).round() as u8
                )
            })
            .collect()
    };
    let (kind, color, stops) = match paint {
        usvg::Paint::Color(c) => ("color", Some(hex(*c)), Vec::new()),
        usvg::Paint::LinearGradient(g) => ("linear", None, stops(g.stops())),
        usvg::Paint::RadialGradient(g) => ("radial", None, stops(g.stops())),
        usvg::Paint::Pattern(_) => ("pattern", None, Vec::new()),
    };
    PaintInfo {
        kind,
        color,
        opacity: round(opacity as f64),
        stops,
    }
}

fn round(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

/// The words for the nearest clip, mask or filter above (or on) a node.
fn effect_words(group: &usvg::Group) -> Option<&'static str> {
    if let Some(filter) = group.filters().first() {
        let kinds: Vec<&usvg::filter::Kind> =
            filter.primitives().iter().map(|p| p.kind()).collect();
        let blur = kinds
            .iter()
            .any(|k| matches!(k, usvg::filter::Kind::GaussianBlur(_)));
        let offset = kinds
            .iter()
            .any(|k| matches!(k, usvg::filter::Kind::Offset(_)));
        let shadow = kinds
            .iter()
            .any(|k| matches!(k, usvg::filter::Kind::DropShadow(_)));
        return Some(if shadow || (blur && offset) {
            "under a drop shadow filter"
        } else if blur {
            "under a blur filter"
        } else {
            "under a filter"
        });
    }
    if group.mask().is_some() {
        return Some("masked");
    }
    if group.clip_path().is_some() {
        return Some("clipped");
    }
    None
}

/// The classified tree of one picture.
#[derive(Debug)]
pub struct Model {
    nodes: Vec<Node>,
    by_address: HashMap<String, usize>,
    by_name: HashMap<String, Vec<usize>>,
    pub(crate) places: HashMap<String, Place>,
    /// Whether the drawn tree is named by address, so edits can find
    /// their nodes (false for outlines from before slice 2).
    pub(crate) editable: bool,
}

impl Model {
    /// Classifies the file's elements (`survey`) against `tree`, the
    /// picture as drawn with every unnamed element named by address and a
    /// probe group first in each parent (`probes`: an element's id → the
    /// probe in its parent) — or `None` when no such tree exists: then
    /// everything is listed and nothing is drawn.
    pub(crate) fn build(
        survey: Survey,
        tree: Option<&usvg::Tree>,
        probes: &HashMap<String, String>,
        editable: bool,
    ) -> Model {
        struct Mapped {
            centre: (f32, f32),
            effect: Option<&'static str>,
            visible: bool,
            opacity: f32,
            fill: Option<PaintInfo>,
            stroke: Option<PaintInfo>,
            fill_gradient: Option<Gradient>,
            stroke_gradient: Option<Gradient>,
            stroke_width: Option<f32>,
            bounds: usvg::Rect,
            is_path: bool,
        }
        let mut mapped: HashMap<String, Mapped> = HashMap::new();
        let mut probe_abs: HashMap<String, usvg::Transform> = HashMap::new();
        fn walk<'t>(
            group: &'t usvg::Group,
            stack: &mut Vec<&'t usvg::Group>,
            survey: &Survey,
            mapped: &mut HashMap<String, Mapped>,
            probe_abs: &mut HashMap<String, usvg::Transform>,
        ) {
            stack.push(group);
            for child in group.children() {
                if let usvg::Node::Group(g) = child {
                    if g.id().starts_with(PROBE) {
                        probe_abs.insert(g.id().to_string(), g.abs_transform());
                        continue;
                    }
                }
                let address = address_of(child.id())
                    .filter(|a| survey.by_address.contains_key(*a) && !mapped.contains_key(*a));
                let mut is_use = false;
                if let Some(address) = address {
                    let row = &survey.rows[survey.by_address[address]];
                    is_use = row.kind == "use";
                    // The element's own groups (its opacity, clip, transform
                    // became them): id-less, one child each.
                    let mut j = stack.len() - 1;
                    while j > 0 && stack[j].id().is_empty() && stack[j].children().len() == 1 {
                        j -= 1;
                    }
                    let mut effect = None;
                    for g in stack.iter().skip(1).rev() {
                        if let Some(words) = effect_words(g) {
                            effect = Some(words);
                            break;
                        }
                    }
                    let own_opacity: f32 = stack[j + 1..]
                        .iter()
                        .map(|g| g.opacity().get())
                        .product::<f32>()
                        * match child {
                            usvg::Node::Group(g) => g.opacity().get(),
                            _ => 1.0,
                        };
                    let bounds = child.abs_bounding_box();
                    let mut m = Mapped {
                        centre: (
                            bounds.x() + bounds.width() / 2.0,
                            bounds.y() + bounds.height() / 2.0,
                        ),
                        effect,
                        visible: true,
                        opacity: own_opacity,
                        fill: None,
                        stroke: None,
                        fill_gradient: None,
                        stroke_gradient: None,
                        stroke_width: None,
                        bounds,
                        is_path: false,
                    };
                    match child {
                        usvg::Node::Path(p) => {
                            m.fill = p.fill().map(|f| paint_info(f.paint(), f.opacity().get()));
                            m.stroke = p.stroke().map(|s| paint_info(s.paint(), s.opacity().get()));
                            m.fill_gradient = p.fill().and_then(|f| gradient_of(f.paint()));
                            m.stroke_gradient = p.stroke().and_then(|s| gradient_of(s.paint()));
                            m.stroke_width = p.stroke().map(|s| s.width().get());
                            m.visible = p.is_visible();
                            m.is_path = true;
                        }
                        usvg::Node::Image(img) => m.visible = img.is_visible(),
                        _ => {}
                    }
                    mapped.insert(address.to_string(), m);
                }
                // A <use> copy's inside is its original's, named twice.
                if let (usvg::Node::Group(g), false) = (child, is_use) {
                    walk(g, stack, survey, mapped, probe_abs);
                }
            }
            stack.pop();
        }
        if let Some(tree) = tree {
            walk(
                tree.root(),
                &mut Vec::new(),
                &survey,
                &mut mapped,
                &mut probe_abs,
            );
        }

        // A <defs> holding only paint (gradients, filters aside) is no
        // element of the picture: it gets no row.
        let parents: HashSet<usize> = survey.rows.iter().filter_map(|r| r.parent).collect();
        let mut nodes = Vec::with_capacity(survey.rows.len());
        let mut places: HashMap<String, Place> = HashMap::new();
        for (index, row) in survey.rows.iter().enumerate() {
            if row.kind == "defs" && !parents.contains(&index) {
                continue;
            }
            let m = mapped.get(&row.address);
            let (tier, reason) = if let Some(locked) = &row.locked {
                (Tier::Locked, locked.clone())
            } else if let Some(m) = m {
                match row.kind {
                    "shape" if m.is_path => {
                        let pattern = [&m.fill, &m.stroke]
                            .iter()
                            .any(|p| p.as_ref().is_some_and(|p| p.kind == "pattern"));
                        if pattern {
                            (
                                Tier::A,
                                "painted with a pattern — its paint stays as drawn".into(),
                            )
                        } else if let Some(effect) = m.effect {
                            (Tier::B, format!("a painted shape, {effect}"))
                        } else {
                            (Tier::C, "a plain shape".into())
                        }
                    }
                    "group" => (Tier::A, "a group".into()),
                    "use" => (
                        Tier::A,
                        "a copy made with <use> — its look is its original's".into(),
                    ),
                    "image" => (Tier::A, "a picture".into()),
                    "text" => (Tier::A, "text — its words, place and opacity".into()),
                    _ => (Tier::A, format!("a {}", row.tag)),
                }
            } else if row.kind == "text" {
                (
                    Tier::Locked,
                    "not drawn — text with no outlines; import the SVG again".into(),
                )
            } else if matches!(row.kind, "svg" | "switch") {
                (
                    Tier::Locked,
                    "kept as drawn — merged into what holds it when drawn".into(),
                )
            } else {
                (
                    Tier::Locked,
                    "not drawn — hidden, empty or off the picture in the file".into(),
                )
            };
            let paints = matches!(tier, Tier::B | Tier::C);
            let editable_row = tier != Tier::Locked;
            nodes.push(Node {
                node: row.address.clone(),
                name: row.name.clone(),
                kind: row.kind,
                tag: row.tag.clone(),
                parent: row.parent.map(|p| survey.rows[p].address.clone()),
                depth: row.depth,
                tier,
                reason,
                drawn: m.is_some() && row.locked.is_none(),
                order: None,
                siblings: None,
                visible: m.filter(|_| editable_row).map(|m| m.visible),
                opacity: m.filter(|_| editable_row).map(|m| round(m.opacity as f64)),
                fill: m.filter(|_| paints).and_then(|m| m.fill.clone()),
                stroke: m.filter(|_| paints).and_then(|m| m.stroke.clone()),
                stroke_width: m
                    .filter(|_| paints)
                    .and_then(|m| m.stroke_width.map(|w| round(w as f64))),
                text: row.text.clone(),
                bounds: m.map(|m| {
                    [
                        round(m.bounds.x() as f64),
                        round(m.bounds.y() as f64),
                        round(m.bounds.width() as f64),
                        round(m.bounds.height() as f64),
                    ]
                }),
            });
            if let (Some(m), true) = (m, editable_row) {
                let id = element_id(&row.address);
                let parent_abs = probes
                    .get(&id)
                    .and_then(|probe| probe_abs.get(probe))
                    .copied()
                    .unwrap_or_default();
                places.insert(
                    row.address.clone(),
                    Place {
                        parent_abs,
                        centre: m.centre,
                        fill_gradient: m.fill_gradient.clone(),
                        stroke_gradient: m.stroke_gradient.clone(),
                        sibling_ids: Vec::new(),
                    },
                );
            }
        }
        // Order among drawn siblings: the rows under one parent that edits
        // reach, in the file's order.
        let mut groups: HashMap<Option<String>, Vec<usize>> = HashMap::new();
        for (i, n) in nodes.iter().enumerate() {
            if places.contains_key(&n.node) {
                groups.entry(n.parent.clone()).or_default().push(i);
            }
        }
        for members in groups.values() {
            let ids: Vec<String> = members
                .iter()
                .map(|&i| element_id(&nodes[i].node))
                .collect();
            for (k, &i) in members.iter().enumerate() {
                nodes[i].order = Some(k);
                nodes[i].siblings = Some(members.len());
                if let Some(place) = places.get_mut(&nodes[i].node) {
                    place.sibling_ids = ids.clone();
                }
            }
        }
        let by_address = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.node.clone(), i))
            .collect::<HashMap<_, _>>();
        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, n) in nodes.iter().enumerate() {
            if let Some(name) = &n.name {
                by_name.entry(name.clone()).or_default().push(i);
            }
        }
        Model {
            nodes,
            by_address,
            by_name,
            places,
            editable,
        }
    }

    /// Every element, in the file's order.
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// The element at `address`.
    pub fn node(&self, address: &str) -> Option<&Node> {
        self.by_address.get(address).map(|&i| &self.nodes[i])
    }

    /// Whether edits can reach this picture's nodes. False for text
    /// outlines made before element edits existed: import it again.
    pub fn editable(&self) -> bool {
        self.editable
    }

    /// The address `name` means: an address, else the one element the
    /// author gave that name.
    pub fn resolve(&self, name: &str) -> Result<&str, String> {
        if let Some(&i) = self.by_address.get(name) {
            return Ok(&self.nodes[i].node);
        }
        match self.by_name.get(name).map(Vec::as_slice) {
            Some([one]) => Ok(&self.nodes[*one].node),
            Some(many) => Err(format!(
                "\"{name}\" names {} elements — use an address: {}",
                many.len(),
                many.iter()
                    .map(|&i| self.nodes[i].node.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            None => Err(format!(
                "no element \"{name}\" in this SVG — promo_inspect lists its elements"
            )),
        }
    }

    /// Whether `o` may apply, and to which address. `strict` (an editor's
    /// or an agent's edit) also refuses keys it does not know; a render
    /// skips what fails and validate names it.
    pub fn check(&self, o: &VectorOverride, strict: bool) -> Result<String, String> {
        let address = self.resolve(&o.node)?.to_string();
        let node = self.node(&address).expect("resolved");
        let shown = match &node.name {
            Some(name) => format!("\"{address}\" ({name})"),
            None => format!("\"{address}\""),
        };
        if strict {
            if let Some(key) = o.extra.keys().next() {
                return Err(format!(
                    "an override takes visible, opacity, transform, order, fill, stroke, \
                     fillStops, strokeStops, strokeWidth and text — not \"{key}\""
                ));
            }
        }
        if node.tier == Tier::Locked {
            return Err(format!("{shown} is {}, so it cannot change", node.reason));
        }
        if o.edits_paint() && !node.tier.paints() {
            return Err(format!(
                "{shown} takes no paint of its own — {}; fill, stroke and stops are a painted \
                 shape's",
                node.reason
            ));
        }
        if o.text.is_some() && node.kind != "text" {
            return Err(format!("{shown} is not text — {}", node.reason));
        }
        if let Some(words) = &o.text {
            if words.trim().is_empty() {
                return Err(format!("{shown}: text cannot be empty — hide it instead"));
            }
        }
        for (key, value) in [("fill", &o.fill), ("stroke", &o.stroke)] {
            if let Some(v) = value {
                parse_colour(v).ok_or_else(|| {
                    format!("{key}: \"{v}\" is not a colour — RRGGBB or RRGGBBAA")
                })?;
            }
        }
        for (key, stops, paint) in [
            ("fillStops", &o.fill_stops, &node.fill),
            ("strokeStops", &o.stroke_stops, &node.stroke),
        ] {
            let Some(stops) = stops else { continue };
            let what = if key == "fillStops" { "fill" } else { "stroke" };
            let have = match paint {
                Some(p) if p.kind == "linear" || p.kind == "radial" => p.stops.len(),
                _ => {
                    return Err(format!(
                        "{shown}'s {what} is not a gradient — set {what} for a colour"
                    ))
                }
            };
            if stops.len() != have {
                return Err(format!(
                    "{shown}'s {what} gradient has {have} stops — {key} gives one colour each, \
                     not {}",
                    stops.len()
                ));
            }
            for v in stops {
                parse_colour(v).ok_or_else(|| {
                    format!("{key}: \"{v}\" is not a colour — RRGGBB or RRGGBBAA")
                })?;
            }
        }
        if let Some(v) = o.opacity {
            if !(0.0..=1.0).contains(&v) {
                return Err(format!("opacity {v} is outside 0–1"));
            }
        }
        if let Some(v) = o.stroke_width {
            if !(v.is_finite() && v >= 0.0) {
                return Err(format!("strokeWidth {v} must be zero or more"));
            }
        }
        if let Some(v) = o.order {
            if v < 0 {
                return Err(format!("order {v} must be zero or more (0 = drawn first)"));
            }
        }
        if let Some(t) = &o.transform {
            let (sx, sy) = t.scales();
            let finite = [sx, sy, t.rotate.unwrap_or(0.0)]
                .iter()
                .chain(t.offset.iter().flatten())
                .all(|v| v.is_finite());
            if !finite {
                return Err("transform: every number must be finite".into());
            }
        }
        if o.edits_tree() && !self.editable {
            return Err(
                "this SVG's text outlines were made before its elements could be edited — \
                 import it again"
                    .into(),
            );
        }
        Ok(address)
    }
}

/// `RRGGBB` / `RRGGBBAA` (a leading `#` allowed) → (r, g, b, alpha 0–1
/// when given).
pub(crate) fn parse_colour(text: &str) -> Option<([u8; 3], Option<f64>)> {
    let t = text.trim().trim_start_matches('#');
    if !(t.len() == 6 || t.len() == 8) || !t.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&t[i..i + 2], 16).ok();
    let rgb = [byte(0)?, byte(2)?, byte(4)?];
    let alpha = if t.len() == 8 {
        Some(byte(6)? as f64 / 255.0)
    } else {
        None
    };
    Some((rgb, alpha))
}

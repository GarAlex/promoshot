//! Overrides applied to the file (SVG-PLAN.md slice 2).
//!
//! An edit works on the file's own XML — the file with every unnamed
//! element named by its address — read into a small tree, changed, and
//! written back as XML for usvg to parse. Nothing the edit does not touch
//! is re-printed: geometry, CSS and `<use>` stay as the author wrote them
//! (usvg's own writer would round a `-3.5e-15` to `-0` and move a corner's
//! anti-aliasing). The file in `Resources/` is never touched, and a picture
//! with no override is drawn from the file's own parse, so removing every
//! override gives back exactly its picture.
//!
//! - visible, opacity and paint go into the element's inline `style`, so
//!   they win over the author's CSS and presentation attributes.
//! - the transform is put first in the element's `transform`: a move,
//!   scale and turn about its centre in the picture's pixels, carried into
//!   its parent's user space — so its own clip and transform go with it.
//! - order moves the element among its drawn siblings.
//! - gradient stops go on a copy of the shape's gradient as usvg resolved
//!   it, so a gradient shared with another shape keeps its stops there.

use std::collections::HashMap;

use usvg::roxmltree;

use promo_model::vector::VectorOverride;

use crate::model::{element_id, escape, parse_colour, Gradient, Model, PROBE, SYNTHETIC};

const SVG_NS: &str = "http://www.w3.org/2000/svg";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

#[derive(Debug, Clone)]
pub(crate) enum X {
    El(El),
    Text(String),
}

#[derive(Debug, Clone)]
pub(crate) struct El {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<X>,
}

impl El {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn set(&mut self, name: &str, value: String) {
        match self.attrs.iter_mut().find(|(n, _)| n == name) {
            Some(slot) => slot.1 = value,
            None => self.attrs.push((name.to_string(), value)),
        }
    }

    fn el_mut(&mut self, i: usize) -> Option<&mut El> {
        match self.children.get_mut(i) {
            Some(X::El(e)) => Some(e),
            _ => None,
        }
    }

    /// Adds declarations to the inline style; later ones win.
    fn style(&mut self, decls: &[(String, String)]) {
        if decls.is_empty() {
            return;
        }
        let mut style = self.attr("style").unwrap_or_default().trim().to_string();
        for (k, v) in decls {
            if !style.is_empty() && !style.ends_with(';') {
                style.push(';');
            }
            style.push_str(&format!("{k}:{v}"));
        }
        self.set("style", style);
    }
}

/// The file's XML as a tree: elements, attributes and text, every
/// namespace declared once at the root.
pub(crate) struct Dom {
    pub root: El,
    namespaces: Vec<(String, String)>,
}

impl Dom {
    pub fn parse(text: &str) -> Result<Dom, String> {
        let options = roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        };
        let doc =
            roxmltree::Document::parse_with_options(text, options).map_err(|e| e.to_string())?;
        // uri → prefix, the first spelling seen; a clash gets a new one.
        let mut prefixes: HashMap<String, String> = HashMap::new();
        let mut taken: Vec<String> = Vec::new();
        for n in doc.descendants().filter(|n| n.is_element()) {
            for ns in n.namespaces() {
                let uri = ns.uri();
                if uri == XML_NS || prefixes.contains_key(uri) {
                    continue;
                }
                let mut prefix = ns.name().unwrap_or("").to_string();
                if uri == SVG_NS {
                    prefix.clear();
                }
                while taken.contains(&prefix) {
                    prefix = format!("ns{}", taken.len());
                }
                taken.push(prefix.clone());
                prefixes.insert(uri.to_string(), prefix);
            }
        }
        prefixes.entry(SVG_NS.to_string()).or_default();
        fn read(node: roxmltree::Node, prefixes: &HashMap<String, String>) -> El {
            let qualified = |ns: Option<&str>, local: &str, attribute: bool| -> String {
                match ns {
                    None => local.to_string(),
                    Some(XML_NS) => format!("xml:{local}"),
                    Some(uri) => match prefixes.get(uri).map(String::as_str) {
                        Some("") if !attribute => local.to_string(),
                        Some("") | None => local.to_string(),
                        Some(p) => format!("{p}:{local}"),
                    },
                }
            };
            let tag = node.tag_name();
            El {
                name: qualified(tag.namespace(), tag.name(), false),
                attrs: node
                    .attributes()
                    .map(|a| {
                        (
                            qualified(a.namespace(), a.name(), true),
                            a.value().to_string(),
                        )
                    })
                    .collect(),
                children: node
                    .children()
                    .filter_map(|c| {
                        if c.is_element() {
                            Some(X::El(read(c, prefixes)))
                        } else if c.is_text() {
                            c.text().map(|t| X::Text(t.to_string()))
                        } else {
                            None
                        }
                    })
                    .collect(),
            }
        }
        let root = read(doc.root_element(), &prefixes);
        let mut namespaces: Vec<(String, String)> =
            prefixes.into_iter().map(|(uri, p)| (p, uri)).collect();
        namespaces.sort();
        Ok(Dom { root, namespaces })
    }

    pub fn write(&self) -> String {
        fn write(el: &El, out: &mut String, namespaces: Option<&[(String, String)]>) {
            out.push('<');
            out.push_str(&el.name);
            if let Some(namespaces) = namespaces {
                for (prefix, uri) in namespaces {
                    if prefix.is_empty() {
                        out.push_str(&format!(" xmlns=\"{}\"", escape(uri)));
                    } else {
                        out.push_str(&format!(" xmlns:{prefix}=\"{}\"", escape(uri)));
                    }
                }
            }
            for (n, v) in &el.attrs {
                out.push(' ');
                out.push_str(n);
                out.push_str("=\"");
                out.push_str(&escape(v));
                out.push('"');
            }
            if el.children.is_empty() {
                out.push_str("/>");
                return;
            }
            out.push('>');
            for c in &el.children {
                match c {
                    X::El(e) => write(e, out, None),
                    X::Text(t) => out.push_str(&escape(t)),
                }
            }
            out.push_str("</");
            out.push_str(&el.name);
            out.push('>');
        }
        let mut out = String::new();
        write(&self.root, &mut out, Some(&self.namespaces));
        out
    }

    /// Sets an attribute on the root element.
    pub fn mark(&mut self, name: &str, value: &str) {
        self.root.set(name, value.to_string());
    }
}

/// The child-index path to the element with `id`.
fn find(el: &El, id: &str, path: &mut Vec<usize>) -> bool {
    for (i, c) in el.children.iter().enumerate() {
        let X::El(child) = c else { continue };
        path.push(i);
        if child.attr("id") == Some(id) || find(child, id, path) {
            return true;
        }
        path.pop();
    }
    false
}

fn at<'a>(root: &'a mut El, path: &[usize]) -> Option<&'a mut El> {
    let mut el = root;
    for &i in path {
        el = el.el_mut(i)?;
    }
    Some(el)
}

/// `source` with an empty probe group first in every element that may
/// hold drawn children, and which probe each element's parent holds (an
/// element's id → its probe): the abs transform of a probe is the user
/// space of the element's parent. A `<switch>` takes no probe (it would
/// draw it in place of its child); its children read its parent's.
pub(crate) fn probed(source: &str) -> Result<(String, HashMap<String, String>), String> {
    let mut dom = Dom::parse(source)?;
    let mut parents = HashMap::new();
    let mut n = 0usize;
    fn walk(
        el: &mut El,
        inherited: Option<&str>,
        n: &mut usize,
        parents: &mut HashMap<String, String>,
    ) {
        let local = el.name.rsplit(':').next().unwrap_or(&el.name).to_string();
        let holds = matches!(local.as_str(), "svg" | "g" | "a");
        let probe = if holds {
            *n += 1;
            let id = format!("{PROBE}{n}");
            el.children.insert(
                0,
                X::El(El {
                    name: "g".into(),
                    attrs: vec![("id".into(), id.clone())],
                    children: Vec::new(),
                }),
            );
            Some(id)
        } else if local == "switch" {
            inherited.map(str::to_string)
        } else {
            None
        };
        for child in el.children.iter_mut() {
            let X::El(child) = child else { continue };
            if child.attr("id").is_some_and(|id| id.starts_with(PROBE)) {
                continue;
            }
            if let (Some(id), Some(p)) = (child.attr("id"), probe.as_deref()) {
                parents.insert(id.to_string(), p.to_string());
            }
            walk(child, probe.as_deref(), n, parents);
        }
    }
    walk(&mut dom.root, None, &mut n, &mut parents);
    Ok((dom.write(), parents))
}

fn colour_value(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

fn number(v: f64) -> String {
    format!("{}", v as f32)
}

fn matrix(m: usvg::Transform) -> String {
    format!(
        "matrix({} {} {} {} {} {})",
        m.sx, m.ky, m.kx, m.sy, m.tx, m.ty
    )
}

fn gradient_copy(g: &Gradient, id: &str, colours: &[String]) -> El {
    let mut attrs = vec![
        ("id".to_string(), id.to_string()),
        ("gradientUnits".into(), "userSpaceOnUse".into()),
    ];
    let names: &[&str] = if g.radial {
        &["cx", "cy", "r", "fx", "fy", "fr"]
    } else {
        &["x1", "y1", "x2", "y2"]
    };
    for (name, v) in names.iter().zip(&g.coords) {
        attrs.push((name.to_string(), format!("{v}")));
    }
    if !g.transform.is_identity() {
        attrs.push(("gradientTransform".into(), matrix(g.transform)));
    }
    attrs.push(("spreadMethod".into(), g.spread.into()));
    let stops = g
        .stops
        .iter()
        .enumerate()
        .map(|(i, (offset, rgb, opacity))| {
            let (rgb, alpha) = colours
                .get(i)
                .and_then(|c| parse_colour(c))
                .map(|(c, a)| (c, a.map(|a| a as f32).unwrap_or(*opacity)))
                .unwrap_or((*rgb, *opacity));
            X::El(El {
                name: "stop".into(),
                attrs: vec![
                    ("offset".into(), format!("{offset}")),
                    ("stop-color".into(), colour_value(rgb)),
                    ("stop-opacity".into(), format!("{alpha}")),
                ],
                children: Vec::new(),
            })
        })
        .collect();
    El {
        name: if g.radial {
            "radialGradient"
        } else {
            "linearGradient"
        }
        .into(),
        attrs,
        children: stops,
    }
}

/// The overrides that change the drawn tree, applied to `source` (the
/// file, every unnamed element named): XML ready to parse. Overrides
/// `model.check` refuses are skipped.
pub(crate) fn apply(
    source: &str,
    model: &Model,
    overrides: &[VectorOverride],
) -> Result<String, String> {
    let mut dom = Dom::parse(source)?;
    let mut copies: Vec<El> = Vec::new();
    for o in overrides.iter().filter(|o| o.edits_tree()) {
        let Ok(address) = model.check(o, false) else {
            continue;
        };
        let Some(place) = model.places.get(&address) else {
            continue;
        };
        let id = element_id(&address);
        let mut path = Vec::new();
        if !find(&dom.root, &id, &mut path) {
            continue;
        }
        let mut decls: Vec<(String, String)> = Vec::new();
        for (key, value) in [("fill", &o.fill), ("stroke", &o.stroke)] {
            if let Some((rgb, alpha)) = value.as_deref().and_then(parse_colour) {
                decls.push((key.into(), colour_value(rgb)));
                if let Some(a) = alpha {
                    decls.push((format!("{key}-opacity"), number(a)));
                }
            }
        }
        if let Some(w) = o.stroke_width {
            decls.push(("stroke-width".into(), number(w)));
        }
        // A reveal: one dash as long as the shown share, a gap past the
        // whole, offset to where the share starts. A path of several
        // sub-paths draws them at one pace (the dash restarts on each):
        // the longest finishes at the end.
        if o.trims() {
            if let Some(length) = place.stroke_length.filter(|l| *l > 0.0) {
                let l = length as f64;
                let a = o.trim_start.unwrap_or(0.0).clamp(0.0, 1.0) * l;
                let b = o.trim_end.unwrap_or(1.0).clamp(0.0, 1.0) * l;
                if b - a <= 1e-6 {
                    decls.push(("stroke".into(), "none".into()));
                } else {
                    decls.push((
                        "stroke-dasharray".into(),
                        format!("{} {}", number(b - a), number(2.0 * l + 1.0)),
                    ));
                    decls.push(("stroke-dashoffset".into(), number(-a)));
                }
            }
        }
        for (key, stops, gradient) in [
            ("fill", &o.fill_stops, &place.fill_gradient),
            ("stroke", &o.stroke_stops, &place.stroke_gradient),
        ] {
            if let (Some(stops), Some(g)) = (stops, gradient) {
                let copy_id = format!("{SYNTHETIC}stops{}", copies.len() + 1);
                copies.push(gradient_copy(g, &copy_id, stops));
                decls.push((key.into(), format!("url(#{copy_id})")));
            }
        }
        if let Some(a) = o.opacity {
            decls.push(("opacity".into(), number(a.clamp(0.0, 1.0))));
        }
        match o.visible {
            Some(false) => decls.push(("display".into(), "none".into())),
            Some(true) => decls.push(("visibility".into(), "visible".into())),
            None => {}
        }
        let Some(el) = at(&mut dom.root, &path) else {
            continue;
        };
        el.style(&decls);
        if let Some(t) = o.transform.as_ref().filter(|t| !t.is_identity()) {
            let (cx, cy) = place.centre;
            let [dx, dy] = t.offset.unwrap_or([0.0, 0.0]);
            let (sx, sy) = t.scales();
            let on_picture = usvg::Transform::from_translate(cx + dx as f32, cy + dy as f32)
                .pre_rotate(t.rotate.unwrap_or(0.0) as f32)
                .pre_scale(sx as f32, sy as f32)
                .pre_translate(-cx, -cy);
            let p = place.parent_abs;
            if let Some(inverse) = p.invert() {
                let w = inverse.pre_concat(on_picture).pre_concat(p);
                let value = match el.attr("transform") {
                    Some(own) if !own.trim().is_empty() => format!("{} {own}", matrix(w)),
                    _ => matrix(w),
                };
                el.set("transform", value);
            }
        }
        if let Some(order) = o.order {
            let (holder_path, index) = path.split_at(path.len() - 1);
            let Some(holder) = at(&mut dom.root, holder_path) else {
                continue;
            };
            let moved = holder.children.remove(index[0]);
            let others: Vec<&String> = place.sibling_ids.iter().filter(|s| **s != id).collect();
            let position_of = |holder: &El, sid: &str| {
                holder
                    .children
                    .iter()
                    .position(|c| matches!(c, X::El(e) if e.attr("id") == Some(sid)))
            };
            let order = (order.max(0) as usize).min(others.len());
            let at = if order < others.len() {
                position_of(holder, others[order])
            } else {
                others
                    .last()
                    .and_then(|last| position_of(holder, last))
                    .map(|i| i + 1)
            };
            let at = at.unwrap_or(index[0]).min(holder.children.len());
            holder.children.insert(at, moved);
        }
    }
    if !copies.is_empty() {
        dom.root.children.push(X::El(El {
            name: "defs".into(),
            attrs: Vec::new(),
            children: copies.into_iter().map(X::El).collect(),
        }));
    }
    Ok(dom.write())
}

/// The import's outlines in the file's own form: `named` (the file, every
/// unnamed element named) with each `<text>` replaced by its glyphs from
/// `written` (usvg's writing of the same file parsed with fonts, where a
/// text is a group under its id). The element keeps its attributes — its
/// transform, opacity, filter and class — on the group; the glyphs' paint
/// goes inline so no author CSS restyles them; the paint servers they use
/// are copied under new ids. `None` when a text sits where a group cannot
/// stand in for it (a clip, a mask, a pattern, a marker, a definition, a
/// switch) — the caller then keeps usvg's writing whole.
pub(crate) fn splice_outlines(named: &str, written: &str) -> Option<String> {
    let mut dom = Dom::parse(named).ok()?;
    let glyphs = Dom::parse(written).ok()?;
    let mut defs_by_id: HashMap<String, El> = HashMap::new();
    fn collect_defs(el: &El, out: &mut HashMap<String, El>) {
        for c in &el.children {
            let X::El(c) = c else { continue };
            if c.name == "defs" {
                for d in &c.children {
                    if let X::El(d) = d {
                        if let Some(id) = d.attr("id") {
                            out.insert(id.to_string(), d.clone());
                        }
                    }
                }
            }
        }
    }
    collect_defs(&glyphs.root, &mut defs_by_id);
    let mut group_by_id: HashMap<String, El> = HashMap::new();
    fn collect_groups(el: &El, out: &mut HashMap<String, El>) {
        for c in &el.children {
            let X::El(c) = c else { continue };
            if c.name == "g" {
                if let Some(id) = c.attr("id") {
                    out.entry(id.to_string()).or_insert_with(|| c.clone());
                }
            }
            if c.name != "defs" {
                collect_groups(c, out);
            }
        }
    }
    collect_groups(&glyphs.root, &mut group_by_id);

    const PAINT: &[&str] = &[
        "fill",
        "fill-opacity",
        "fill-rule",
        "stroke",
        "stroke-opacity",
        "stroke-width",
        "stroke-linecap",
        "stroke-linejoin",
        "stroke-miterlimit",
        "stroke-dasharray",
        "stroke-dashoffset",
        "paint-order",
        "visibility",
    ];
    let mut needed: Vec<String> = Vec::new();
    fn restyle(el: &mut El, needed: &mut Vec<String>) {
        let mut decls = Vec::new();
        el.attrs.retain(|(n, v)| {
            if PAINT.contains(&n.as_str()) {
                let v = match v.strip_prefix("url(#").and_then(|r| r.strip_suffix(')')) {
                    Some(id) => {
                        needed.push(id.to_string());
                        format!("url(#{SYNTHETIC}text:{id})")
                    }
                    None => v.clone(),
                };
                decls.push((n.clone(), v));
                false
            } else {
                true
            }
        });
        for (n, v) in el.attrs.iter_mut() {
            if matches!(n.as_str(), "clip-path" | "mask" | "filter") {
                *v = v.replace("url(#", &format!("url(#{SYNTHETIC}text:"));
                if let Some(id) = v
                    .strip_prefix(&format!("url(#{SYNTHETIC}text:"))
                    .and_then(|r| r.strip_suffix(')'))
                {
                    needed.push(id.to_string());
                }
            }
        }
        el.style(&decls);
        for c in el.children.iter_mut() {
            if let X::El(c) = c {
                restyle(c, needed);
            }
        }
    }

    // Replace every <text>; refuse where a group cannot stand in.
    fn replace(
        el: &mut El,
        drawn: bool,
        groups: &HashMap<String, El>,
        needed: &mut Vec<String>,
    ) -> bool {
        let local = el.name.rsplit(':').next().unwrap_or(&el.name).to_string();
        let drawn_here = drawn && matches!(local.as_str(), "svg" | "g" | "a");
        for c in el.children.iter_mut() {
            let X::El(child) = c else { continue };
            let child_local = child
                .name
                .rsplit(':')
                .next()
                .unwrap_or(&child.name)
                .to_string();
            if child_local == "text" {
                if !drawn_here {
                    return false;
                }
                let Some(id) = child.attr("id") else {
                    return false;
                };
                let mut inner = match groups.get(id) {
                    Some(g) => g.clone(),
                    // A text that draws nothing (no words, no face): an
                    // empty group keeps its place.
                    None => El {
                        name: "g".into(),
                        attrs: Vec::new(),
                        children: Vec::new(),
                    },
                };
                inner.attrs.retain(|(n, _)| n != "id");
                restyle(&mut inner, needed);
                let mut group = El {
                    name: "g".into(),
                    attrs: child
                        .attrs
                        .iter()
                        .filter(|(n, _)| {
                            !matches!(
                                n.as_str(),
                                "x" | "y" | "dx" | "dy" | "rotate" | "textLength" | "lengthAdjust"
                            )
                        })
                        .cloned()
                        .collect(),
                    children: vec![X::El(inner)],
                };
                // The text's own paint was the glyphs'; the group keeps
                // only what applies to it as a whole.
                group
                    .attrs
                    .retain(|(n, _)| !PAINT.contains(&n.as_str()) || n == "visibility");
                *c = X::El(group);
            } else if !replace(child, drawn_here, groups, needed) {
                return false;
            }
        }
        true
    }
    if !replace(&mut dom.root, true, &group_by_id, &mut needed) {
        return None;
    }
    // The paint servers (and what they point at) the glyphs use.
    let mut copied: Vec<String> = Vec::new();
    let mut defs: Vec<X> = Vec::new();
    while let Some(id) = needed.pop() {
        if copied.contains(&id) {
            continue;
        }
        copied.push(id.clone());
        let Some(mut def) = defs_by_id.get(&id).cloned() else {
            continue;
        };
        def.set("id", format!("{SYNTHETIC}text:{id}"));
        restyle(&mut def, &mut needed);
        for (n, v) in def.attrs.iter_mut() {
            if n == "xlink:href" || n == "href" {
                if let Some(target) = v.strip_prefix('#') {
                    needed.push(target.to_string());
                    *v = format!("#{SYNTHETIC}text:{target}");
                }
            }
        }
        defs.push(X::El(def));
    }
    if !defs.is_empty() {
        dom.root.children.push(X::El(El {
            name: "defs".into(),
            attrs: Vec::new(),
            children: defs,
        }));
    }
    Some(dom.write())
}

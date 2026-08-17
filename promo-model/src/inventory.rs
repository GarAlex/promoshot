//! The filesystem as the resource inventory.
//!
//! `metadata.json`'s `resources` array was doing two jobs: saying WHAT the
//! project has (pure duplication of `Resources/` — and a lie the moment
//! someone drops a file in or deletes one), and holding the editing state
//! about each asset (trims, cuts, speed, volume, the narration receipt, a
//! caption's text). Only the second job needs the file.
//!
//! So the listing is the inventory and the array is the state:
//!
//! - A media file with no entry becomes a resource with defaults. Nothing has
//!   to be declared before it can be used — drop `clip.mp4` in `Resources/`,
//!   reference it, done.
//! - An entry whose file is gone is reported MISSING rather than silently
//!   rendering nothing: the layers using it keep their timing and keyframes
//!   and can be pointed at something else.
//! - An entry that needs no file (captions; a speech resource whose audio has
//!   not been synthesized yet) is neither derived nor missing.
//!
//! The id of a derived resource is `uuid5(project namespace, relative path)`,
//! so anyone — the app, the CLI, an assistant writing JSON by hand — computes
//! the same id for the same file without being told it. That is only the
//! BIRTH rule: once an entry exists in the file its stored id is the identity,
//! which is what makes renaming a file a filename edit rather than a
//! re-referencing exercise.

use crate::{ProjectMetadata, ProjectResource, ProjectResourceKind};
use std::collections::HashSet;

/// Why a resource is in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceOrigin {
    /// Declared in `metadata.json`, and its file is present (or it needs none).
    Declared,
    /// Found in `Resources/` with no entry — defaults, id derived from path.
    Derived,
    /// Declared, but its file is not in `Resources/` any more.
    Missing,
}

/// One resolved resource: the record plus where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedResource {
    pub resource: ProjectResource,
    pub origin: ResourceOrigin,
}

impl ResolvedResource {
    pub fn is_missing(&self) -> bool {
        self.origin == ResourceOrigin::Missing
    }
}

/// Media extensions the scan adopts, by the kind they become. Anything else
/// in `Resources/` is left alone — a stray note or a sidecar is not a
/// resource, and guessing would put junk in the timeline.
fn kind_for_extension(name: &str) -> Option<ProjectResourceKind> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    match ext.as_str() {
        "mp4" | "mov" | "m4v" => Some(ProjectResourceKind::Video),
        "png" | "jpg" | "jpeg" | "heic" | "gif" | "tiff" | "bmp" => {
            Some(ProjectResourceKind::Image)
        }
        "mp3" | "m4a" | "wav" | "aac" | "caf" | "aiff" => Some(ProjectResourceKind::Audio),
        _ => None,
    }
}

/// True when this kind of resource is backed by a file at all. A caption is
/// pure state; a speech resource is a caption-like spec until it has been
/// synthesized, and only then names a file.
fn needs_file(resource: &ProjectResource) -> bool {
    match resource.kind {
        // A caption is words, a drawing is shapes, a path is two anchors and
        // a curve — all of them live entirely in metadata.json.
        ProjectResourceKind::Caption
        | ProjectResourceKind::Drawing
        | ProjectResourceKind::Path => false,
        _ => !resource.filename.is_empty(),
    }
}

/// `uuid5(namespace, name)` — RFC 4122 §4.3, SHA-1 based, uppercase
/// hyphenated to match every other id in the format.
///
/// Hand-rolled because the workspace has no uuid dependency and this is the
/// whole of it; the Swift twin uses CryptoKit's Insecure.SHA1 over the same
/// bytes, and a parity test pins them together.
pub fn uuid5(namespace: [u8; 16], name: &str) -> String {
    let mut bytes = Vec::with_capacity(16 + name.len());
    bytes.extend_from_slice(&namespace);
    bytes.extend_from_slice(name.as_bytes());
    let mut hash = sha1(&bytes);
    hash[6] = (hash[6] & 0x0F) | 0x50; // version 5
    hash[8] = (hash[8] & 0x3F) | 0x80; // RFC 4122 variant
    format!(
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6], hash[7],
        hash[8], hash[9], hash[10], hash[11], hash[12], hash[13], hash[14], hash[15]
    )
}

/// The namespace a project derives ids in: `uuid5(URL namespace, project id)`,
/// so the same filename in two projects is two different resources.
pub fn project_namespace(project_id: &str) -> [u8; 16] {
    // RFC 4122 namespace_URL.
    const NAMESPACE_URL: [u8; 16] = [
        0x6b, 0xa7, 0xb8, 0x11, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30,
        0xc8,
    ];
    let text = uuid5(NAMESPACE_URL, &format!("promoshot.resources/{project_id}"));
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap_or(0);
    }
    out
}

/// The id a file gets when nothing has declared it.
pub fn derived_id(project_id: &str, relative_path: &str) -> String {
    uuid5(project_namespace(project_id), relative_path)
}

/// Resolves the project's resources against what is actually in `Resources/`.
///
/// `filenames` is that directory's listing (names, not paths — the format
/// stores names). Order is stable: declared entries first, in their stored
/// order, then derived files sorted by name, so two runs of the same project
/// produce the same list.
pub fn effective_resources(meta: &ProjectMetadata, filenames: &[String]) -> Vec<ResolvedResource> {
    let present: HashSet<&str> = filenames.iter().map(|s| s.as_str()).collect();
    let declared = meta.resources.clone().unwrap_or_default();
    let claimed: HashSet<String> = declared.iter().map(|r| r.filename.clone()).collect();

    let mut out: Vec<ResolvedResource> = declared
        .into_iter()
        .map(|resource| {
            let origin = if !needs_file(&resource) || present.contains(resource.filename.as_str()) {
                ResourceOrigin::Declared
            } else {
                ResourceOrigin::Missing
            };
            ResolvedResource { resource, origin }
        })
        .collect();

    let mut derived: Vec<&String> = filenames
        .iter()
        .filter(|name| !claimed.contains(*name) && kind_for_extension(name).is_some())
        .collect();
    derived.sort();
    for name in derived {
        let Some(kind) = kind_for_extension(name) else {
            continue;
        };
        let mut resource = ProjectResource::placeholder(kind);
        resource.id = derived_id(&meta.id, name);
        resource.filename = name.clone();
        resource.display_name = name
            .rsplit_once('.')
            .map(|(stem, _)| stem.to_string())
            .unwrap_or_else(|| name.clone());
        out.push(ResolvedResource {
            resource,
            origin: ResourceOrigin::Derived,
        });
    }
    out
}

/// Layers whose resource is missing, by layer id — what the editor badges and
/// what `promo_validate` reports. A layer pointing at nothing at all counts
/// too: it renders no differently.
pub fn layers_with_missing_media(meta: &ProjectMetadata, filenames: &[String]) -> Vec<String> {
    let resolved = effective_resources(meta, filenames);
    let missing: HashSet<&str> = resolved
        .iter()
        .filter(|r| r.is_missing())
        .map(|r| r.resource.id.as_str())
        .collect();
    let known: HashSet<&str> = resolved.iter().map(|r| r.resource.id.as_str()).collect();
    meta.layers
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter(|layer| match layer.resource_id.as_deref() {
            Some(id) => missing.contains(id) || !known.contains(id),
            None => false,
        })
        .map(|layer| layer.id.clone())
        .collect()
}

/// Minimal SHA-1 (FIPS 180-4). Only uuid5 needs it, and it is 40 lines.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut message = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in message.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(extra: &str) -> ProjectMetadata {
        let json = format!(
            r#"{{
                "id": "PRJ", "name": "T", "createdAt": 0, "state": "recorded",
                "subtitles": [], "trimStart": 0, "trimEnd": 0,
                "videoDuration": 0, "compositionSettings": {{}}{extra}
            }}"#
        );
        ProjectMetadata::from_json(&json).expect("test project parses")
    }

    #[test]
    fn uuid5_matches_the_rfc_vector() {
        // RFC 4122 / Python uuid5(NAMESPACE_DNS, "python.org").
        const NAMESPACE_DNS: [u8; 16] = [
            0x6b, 0xa7, 0xb8, 0x10, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4,
            0x30, 0xc8,
        ];
        assert_eq!(
            uuid5(NAMESPACE_DNS, "python.org"),
            "886313E1-3B8A-5372-9B90-0C9AEE199E5D"
        );
    }

    #[test]
    fn an_unlisted_file_becomes_a_resource_with_a_derived_id() {
        let meta = project("");
        let files = vec!["clip.mp4".to_string(), "notes.txt".to_string()];
        let resolved = effective_resources(&meta, &files);
        assert_eq!(resolved.len(), 1, "only media is adopted, not notes.txt");
        assert_eq!(resolved[0].origin, ResourceOrigin::Derived);
        assert_eq!(resolved[0].resource.kind, ProjectResourceKind::Video);
        assert_eq!(resolved[0].resource.display_name, "clip");
        // Anyone can compute this id without asking the app.
        assert_eq!(resolved[0].resource.id, derived_id("PRJ", "clip.mp4"));
        // And it is stable across runs and across machines.
        assert_eq!(
            effective_resources(&meta, &files)[0].resource.id,
            resolved[0].resource.id
        );
    }

    #[test]
    fn the_same_name_in_two_projects_is_two_resources() {
        assert_ne!(derived_id("A", "clip.mp4"), derived_id("B", "clip.mp4"));
    }

    #[test]
    fn a_declared_entry_wins_and_keeps_its_state() {
        let meta = project(
            r#", "resources": [{
                "id": "R1", "kind": "video", "filename": "clip.mp4",
                "displayName": "The good take", "addedAt": 0,
                "trimStart": 2.0, "trimEnd": 6.0
            }]"#,
        );
        let resolved = effective_resources(&meta, &["clip.mp4".to_string()]);
        assert_eq!(resolved.len(), 1, "the file is not adopted twice");
        assert_eq!(resolved[0].origin, ResourceOrigin::Declared);
        assert_eq!(resolved[0].resource.id, "R1");
        assert_eq!(resolved[0].resource.display_name, "The good take");
        assert_eq!(resolved[0].resource.trim_start, Some(2.0));
    }

    #[test]
    fn a_vanished_file_is_missing_not_silently_empty() {
        let meta = project(
            r#", "resources": [{
                "id": "R1", "kind": "video", "filename": "gone.mp4",
                "displayName": "Gone", "addedAt": 0
            }],
            "layers": [{
                "id": "L1", "name": "Clip", "sortIndex": 1, "kind": "video",
                "isEnabled": true, "startTime": 0, "keyframes": [],
                "resourceID": "R1"
            }, {
                "id": "L2", "name": "Cap", "sortIndex": 2, "kind": "caption",
                "isEnabled": true, "startTime": 0, "keyframes": []
            }]"#,
        );
        let resolved = effective_resources(&meta, &[]);
        assert_eq!(resolved[0].origin, ResourceOrigin::Missing);
        // The layer is named so the editor can badge it and offer a replace;
        // the caption layer, which needs no file, is not.
        assert_eq!(layers_with_missing_media(&meta, &[]), vec!["L1"]);
    }

    #[test]
    fn captions_and_unsynthesized_speech_need_no_file() {
        let meta = project(
            r#", "resources": [{
                "id": "C1", "kind": "caption", "filename": "",
                "displayName": "Title", "addedAt": 0, "captionText": "Hello"
            }, {
                "id": "S1", "kind": "audio", "filename": "",
                "displayName": "VO", "addedAt": 0,
                "speech": {"text": "Not spoken yet", "provider": "openai", "voiceID": "shimmer"}
            }]"#,
        );
        let resolved = effective_resources(&meta, &[]);
        assert!(
            resolved.iter().all(|r| r.origin == ResourceOrigin::Declared),
            "nothing that needs no file may be reported missing: {resolved:?}"
        );
    }
}

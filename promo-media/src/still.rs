//! Still pictures as every renderer should see them: upright and sRGB.
//!
//! An iPhone photo arrives as HEIC, stored sideways with an EXIF
//! orientation, in Display P3; a Mac screenshot as a P3-tagged PNG. The
//! apps' decoder (ImageIO) bakes the orientation and converts the colour.
//! The headless decoder did neither and could not read HEIC, so the same
//! project rendered a desaturated screenshot and a sideways photo headless,
//! or failed outright (review 2026-09-27, F9). One decode here, for the
//! CLI's renderer and for import through the CLI's authoring verbs.

use crate::MediaError;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageEncoder, ImageReader};
use std::io::Cursor;
use std::path::Path;
use std::process::Command;

/// Upright sRGB pixels, straight (not premultiplied) RGBA, row-major.
#[derive(Debug, Clone)]
pub struct Still {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// What a file needed before it read the way every renderer reads it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Normalized {
    /// HEIC/HEIF, which only ffmpeg decodes here.
    pub converted_format: bool,
    /// An EXIF orientation other than upright, now baked into the pixels.
    pub rotated: bool,
    /// An ICC profile other than sRGB, now converted to sRGB.
    pub recoloured: bool,
}

impl Normalized {
    pub fn any(&self) -> bool {
        self.converted_format || self.rotated || self.recoloured
    }
}

fn is_heif(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "heic" | "heif" | "hif"))
}

/// Upright sRGB pixels from any still the renderers take.
pub fn decode(path: &Path) -> Result<Still, MediaError> {
    decode_reporting(path).map(|(still, _)| still)
}

/// [`decode`], saying what it had to do.
pub fn decode_reporting(path: &Path) -> Result<(Still, Normalized), MediaError> {
    let mut done = Normalized::default();
    let bytes = if is_heif(path) {
        done.converted_format = true;
        heif_as_png(path)?
    } else {
        std::fs::read(path).map_err(|e| MediaError::Backend(format!("{}: {e}", path.display())))?
    };
    let unreadable =
        |e: image::ImageError| MediaError::Unsupported(format!("{}: {e}", path.display()));
    let mut decoder = ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|e| MediaError::Backend(format!("{}: {e}", path.display())))?
        .into_decoder()
        .map_err(unreadable)?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let profile = decoder.icc_profile().ok().flatten();
    let mut picture = DynamicImage::from_decoder(decoder).map_err(unreadable)?;
    if orientation != Orientation::NoTransforms {
        picture.apply_orientation(orientation);
        done.rotated = true;
    }
    let rgba = picture.to_rgba8();
    let (width, height) = rgba.dimensions();
    let mut rgba = rgba.into_raw();
    if let Some(converted) = profile.as_deref().and_then(|icc| to_srgb(icc, &rgba)) {
        rgba = converted;
        done.recoloured = true;
    }
    Ok((
        Still {
            rgba,
            width,
            height,
        },
        done,
    ))
}

/// The picture as a PNG every host reads the same way, when the file needed
/// anything to get there — `None` when it is already upright sRGB in a
/// format both hosts decode, and should be kept exactly as it arrived.
pub fn normalized_png(path: &Path) -> Result<Option<(Vec<u8>, u32, u32)>, MediaError> {
    let (still, done) = decode_reporting(path)?;
    if !done.any() {
        return Ok(None);
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            &still.rgba,
            still.width,
            still.height,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| MediaError::Backend(format!("{}: {e}", path.display())))?;
    Ok(Some((png, still.width, still.height)))
}

/// RGBA pixels in `icc`'s colour space, converted to sRGB — `None` when the
/// profile IS sRGB (same primaries; a curve of 2.2 against the sRGB curve
/// is not worth a conversion), is not an RGB profile, or cannot be read.
fn to_srgb(icc: &[u8], rgba: &[u8]) -> Option<Vec<u8>> {
    use moxcms::{ColorProfile, Layout, TransformOptions};
    let source = ColorProfile::new_from_slice(icc).ok()?;
    let srgb = ColorProfile::new_srgb();
    let near = |a: moxcms::Xyzd, b: moxcms::Xyzd| {
        (a.x - b.x).abs() < 0.002 && (a.y - b.y).abs() < 0.002 && (a.z - b.z).abs() < 0.002
    };
    if near(source.red_colorant, srgb.red_colorant)
        && near(source.green_colorant, srgb.green_colorant)
        && near(source.blue_colorant, srgb.blue_colorant)
    {
        return None;
    }
    let transform = source
        .create_transform_8bit(
            Layout::Rgba,
            &srgb,
            Layout::Rgba,
            TransformOptions::default(),
        )
        .ok()?;
    let mut out = vec![0u8; rgba.len()];
    transform.transform(rgba, &mut out).ok()?;
    Some(out)
}

/// HEIC/HEIF through ffmpeg, as PNG bytes (orientation applied by ffmpeg's
/// autorotate). The error says what to do when this ffmpeg cannot.
fn heif_as_png(path: &Path) -> Result<Vec<u8>, MediaError> {
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-f", "image2pipe", "-vcodec", "png", "-"])
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                MediaError::ToolMissing("ffmpeg", "not found on PATH — it decodes HEIC here".into())
            }
            _ => MediaError::Backend(format!("ffmpeg: {e}")),
        })?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err(MediaError::Unsupported(format!(
            "{}: this ffmpeg cannot decode HEIC (it needs HEIF support, ffmpeg 7.1 or later) — \
             convert the picture to PNG or JPEG",
            path.display()
        )));
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("promo-still-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The standard Display P3 → sRGB conversion, done here by hand as the
    /// independent answer: linearise with the sRGB curve (P3 shares it),
    /// the published P3→sRGB matrix, encode.
    fn p3_to_srgb(rgb: [u8; 3]) -> [f64; 3] {
        let lin = |v: u8| {
            let c = v as f64 / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        let enc = |c: f64| {
            let c = c.clamp(0.0, 1.0);
            255.0
                * if c <= 0.0031308 {
                    12.92 * c
                } else {
                    1.055 * c.powf(1.0 / 2.4) - 0.055
                }
        };
        let (r, g, b) = (lin(rgb[0]), lin(rgb[1]), lin(rgb[2]));
        [
            enc(1.2249 * r - 0.2247 * g),
            enc(-0.0420 * r + 1.0419 * g),
            enc(-0.0197 * r - 0.0786 * g + 1.0979 * b),
        ]
    }

    /// A P3-tagged screenshot reads as the colour it shows, not as its raw
    /// numbers: (180, 60, 60) in P3 is a redder (196, 46, 54)-ish in sRGB.
    /// Headless used to draw the raw numbers — the desaturated screenshot.
    #[test]
    fn a_p3_picture_reads_as_srgb() {
        let dir = scratch("p3");
        let path = dir.join("p3.png");
        let pixel = [180u8, 60, 60, 255];
        let mut bytes = Vec::new();
        let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
        encoder
            .set_icc_profile(moxcms::ColorProfile::new_display_p3().encode().unwrap())
            .unwrap();
        encoder
            .write_image(&pixel.repeat(4), 2, 2, image::ExtendedColorType::Rgba8)
            .unwrap();
        std::fs::write(&path, &bytes).unwrap();

        let (still, done) = decode_reporting(&path).unwrap();
        assert!(done.recoloured && !done.rotated);
        let want = p3_to_srgb([180, 60, 60]);
        for (got, want) in still.rgba[..3].iter().zip(want) {
            assert!(
                (*got as f64 - want).abs() <= 3.0,
                "{:?} vs {want:?}",
                &still.rgba[..3]
            );
        }
        assert_eq!(still.rgba[3], 255, "alpha passes through");
        let (png, w, h) = normalized_png(&path)
            .unwrap()
            .expect("a P3 file is normalised");
        assert_eq!((w, h), (2, 2));
        let reread = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(&reread.as_raw()[..4], &still.rgba[..4]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A photo stored sideways with EXIF orientation 6 (rotate 90° clockwise)
    /// reads upright: its 4x2 storage comes back 2x4, the left column's
    /// colour on top.
    #[test]
    fn an_exif_rotated_photo_reads_upright() {
        let dir = scratch("exif");
        let path = dir.join("sideways.png");
        // Stored 4 wide, 2 tall: the left half red, the right half blue.
        let mut stored = Vec::new();
        for _y in 0..2 {
            for x in 0..4 {
                stored.extend_from_slice(if x < 2 {
                    &[255, 0, 0, 255]
                } else {
                    &[0, 0, 255, 255]
                });
            }
        }
        // Minimal little-endian TIFF/EXIF with one tag: Orientation = 6.
        let exif: Vec<u8> = vec![
            b'I', b'I', 42, 0, 8, 0, 0, 0, // header, IFD at 8
            1, 0, // one entry
            0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, // 0x0112 SHORT 1 = 6
            0, 0, 0, 0, // no next IFD
        ];
        let mut bytes = Vec::new();
        let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
        encoder.set_exif_metadata(exif).unwrap();
        encoder
            .write_image(&stored, 4, 2, image::ExtendedColorType::Rgba8)
            .unwrap();
        std::fs::write(&path, &bytes).unwrap();

        let (still, done) = decode_reporting(&path).unwrap();
        assert!(done.rotated, "the orientation was applied");
        assert_eq!((still.width, still.height), (2, 4));
        // Rotating 90° clockwise puts the stored left column along the top.
        assert_eq!(&still.rgba[..4], &[255, 0, 0, 255], "red on top");
        let last = still.rgba.len() - 4;
        assert_eq!(&still.rgba[last..], &[0, 0, 255, 255], "blue at the bottom");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An upright sRGB picture is kept exactly as it arrived.
    #[test]
    fn a_plain_picture_is_left_alone() {
        let dir = scratch("plain");
        let path = dir.join("plain.png");
        image::RgbaImage::from_pixel(3, 2, image::Rgba([10, 20, 30, 255]))
            .save(&path)
            .unwrap();
        assert!(normalized_png(&path).unwrap().is_none());
        let still = decode(&path).unwrap();
        assert_eq!(&still.rgba[..4], &[10, 20, 30, 255]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// HEIC, where this machine can make one (macOS `sips`): decoded through
    /// ffmpeg into upright pixels — or, where this ffmpeg has no HEIF, an
    /// error that says to convert the picture. Both answers are checked.
    #[cfg(target_os = "macos")]
    #[test]
    fn heic_decodes_or_says_how_to_convert_it() {
        let dir = scratch("heic");
        let source = dir.join("wide.png");
        image::RgbaImage::from_pixel(8, 4, image::Rgba([40, 160, 80, 255]))
            .save(&source)
            .unwrap();
        let heic = dir.join("wide.heic");
        let made = Command::new("sips")
            .args(["-s", "format", "heic"])
            .arg(&source)
            .arg("--out")
            .arg(&heic)
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(made, "sips makes HEIC on every supported macOS");
        match decode_reporting(&heic) {
            Ok((still, done)) => {
                assert!(done.converted_format);
                assert_eq!((still.width, still.height), (8, 4));
                let g = still.rgba[1] as i32;
                assert!(
                    (g - 160).abs() < 12,
                    "the picture came through: {:?}",
                    &still.rgba[..4]
                );
            }
            Err(e) => assert!(e.to_string().contains("convert the picture"), "{e}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

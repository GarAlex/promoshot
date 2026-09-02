//! `.cube` colour look-up tables, and the 2D strip the compositor samples.
//!
//! A 3D LUT of size N is laid out as N slices of N×N side by side: the
//! strip is N·N wide and N tall, pixel (x, y) = (r + N·b, g). The shader
//! samples the two slices around the blue coordinate bilinearly and mixes
//! them — a trilinear lookup with a plain 2D sampler on every backend.

/// A parsed `.cube` table: `size` points per axis, `rgb` in scan order
/// (red fastest, then green, then blue), values already mapped to 0…1.
#[derive(Debug, Clone, PartialEq)]
pub struct Lut {
    pub size: usize,
    pub rgb: Vec<[f32; 3]>,
}

/// Parses the Adobe/Resolve `.cube` text form: `TITLE`, `LUT_3D_SIZE N`,
/// optional `DOMAIN_MIN`/`DOMAIN_MAX`, then N³ rows of `r g b`. 1D tables
/// (`LUT_1D_SIZE`) are refused — the compositor samples a cube.
pub fn parse_cube(text: &str) -> Result<Lut, String> {
    let mut size = 0usize;
    let mut domain_min = [0.0f32; 3];
    let mut domain_max = [1.0f32; 3];
    let mut rows: Vec<[f32; 3]> = Vec::new();
    for (line_no, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let head = parts.next().unwrap_or("");
        match head {
            "TITLE" => {}
            "LUT_1D_SIZE" => return Err("1D tables are not supported; a cube is".into()),
            "LUT_3D_SIZE" => {
                size = parts
                    .next()
                    .and_then(|v| v.parse().ok())
                    .filter(|n: &usize| (2..=256).contains(n))
                    .ok_or_else(|| format!("line {}: LUT_3D_SIZE must be 2…256", line_no + 1))?;
            }
            "DOMAIN_MIN" | "DOMAIN_MAX" => {
                let mut values = [0.0f32; 3];
                for value in values.iter_mut() {
                    *value = parts
                        .next()
                        .and_then(|v| v.parse().ok())
                        .ok_or_else(|| format!("line {}: three numbers expected", line_no + 1))?;
                }
                if head == "DOMAIN_MIN" {
                    domain_min = values;
                } else {
                    domain_max = values;
                }
            }
            _ => {
                let r: f32 = head
                    .parse()
                    .map_err(|_| format!("line {}: not a number: {head}", line_no + 1))?;
                let g: f32 = parts
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| format!("line {}: three numbers expected", line_no + 1))?;
                let b: f32 = parts
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| format!("line {}: three numbers expected", line_no + 1))?;
                rows.push([r, g, b]);
            }
        }
    }
    if size == 0 {
        return Err("no LUT_3D_SIZE".into());
    }
    if rows.len() != size * size * size {
        return Err(format!(
            "{} rows for a size-{size} cube ({} expected)",
            rows.len(),
            size * size * size
        ));
    }
    for row in rows.iter_mut() {
        for c in 0..3 {
            let span = (domain_max[c] - domain_min[c]).abs().max(1e-6);
            row[c] = ((row[c] - domain_min[c]) / span).clamp(0.0, 1.0);
        }
    }
    Ok(Lut { size, rgb: rows })
}

impl Lut {
    /// The strip as tightly packed BGRA8 (the compositor's input format):
    /// `(pixels, width = size², height = size)`.
    pub fn strip_bgra8(&self) -> (Vec<u8>, u32, u32) {
        let n = self.size;
        let (width, height) = (n * n, n);
        let mut out = vec![0u8; width * height * 4];
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    let rgb = self.rgb[(b * n + g) * n + r];
                    let x = r + n * b;
                    let y = g;
                    let i = (y * width + x) * 4;
                    out[i] = (rgb[2] * 255.0).round() as u8;
                    out[i + 1] = (rgb[1] * 255.0).round() as u8;
                    out[i + 2] = (rgb[0] * 255.0).round() as u8;
                    out[i + 3] = 255;
                }
            }
        }
        (out, width as u32, height as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The identity of size 2, and an inverting table, parse and lay out
    /// as (r + N·b, g); a domain is honoured; a wrong row count refuses.
    #[test]
    fn cube_parses_and_lays_out_the_strip() {
        let identity = "TITLE \"id\"\nLUT_3D_SIZE 2\n\
            0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
        let lut = parse_cube(identity).unwrap();
        assert_eq!(lut.size, 2);
        let (px, w, h) = lut.strip_bgra8();
        assert_eq!((w, h), (4, 2));
        // (x=1,y=0) = r=1,b=0,g=0 -> red; (x=2,y=1) = r=0,b=1,g=1 -> cyan.
        assert_eq!(&px[4..8], &[0, 0, 255, 255]);
        assert_eq!(&px[(4 + 2) * 4..(4 + 2) * 4 + 4], &[255, 255, 0, 255]);

        let inverting = "LUT_3D_SIZE 2\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 2 2 2\n\
            2 2 2\n0 2 2\n2 0 2\n0 0 2\n2 2 0\n0 2 0\n2 0 0\n0 0 0\n";
        let lut = parse_cube(inverting).unwrap();
        assert_eq!(lut.rgb[0], [1.0, 1.0, 1.0]);
        assert_eq!(lut.rgb[7], [0.0, 0.0, 0.0]);
        assert!(parse_cube("LUT_3D_SIZE 2\n0 0 0\n").is_err());
        assert!(parse_cube("LUT_1D_SIZE 2\n0\n1\n").is_err());
    }
}

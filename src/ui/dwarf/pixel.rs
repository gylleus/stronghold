//! Pixel rendering: shapes rasterized small on the CPU (supersampled, then averaged down),
//! with hard alpha, optionally mapped to a fixed palette. Lets code-drawn art be shown as
//! pixel art at any resolution and in any palette.

use std::collections::HashMap;

use eframe::egui::epaint::{TessellationOptions, Tessellator};
use eframe::egui::{Color32, ColorImage, Mesh, Pos2, Rect, Shape};

/// A fixed set of colours, such as a Lospec palette.
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub name: String,
    pub colors: Vec<Color32>,
}

impl Palette {
    /// Reads a Lospec palette JSON: `{"name": …, "colors": ["rrggbb", …]}`.
    pub fn from_lospec(json: &str) -> Result<Self, String> {
        let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let name = v["name"].as_str().unwrap_or("palette").to_string();
        let colors = v["colors"]
            .as_array()
            .ok_or("no colors")?
            .iter()
            .map(|c| {
                let hex = c.as_str().ok_or("colour is not a string")?;
                let n = u32::from_str_radix(hex.trim_start_matches('#'), 16)
                    .map_err(|e| e.to_string())?;
                Ok(Color32::from_rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
            })
            .collect::<Result<Vec<_>, String>>()?;
        if colors.is_empty() {
            return Err("empty palette".into());
        }
        Ok(Self { name, colors })
    }
}

/// Maps colours to their nearest palette colour (in CIELAB), remembering past answers.
pub struct Mapper {
    lab: Vec<[f32; 3]>,
    colors: Vec<Color32>,
    cache: HashMap<[u8; 3], Color32>,
}

impl Mapper {
    pub fn new(palette: &Palette) -> Self {
        Self {
            lab: palette.colors.iter().map(|c| lab(*c)).collect(),
            colors: palette.colors.clone(),
            cache: HashMap::new(),
        }
    }

    pub fn map(&mut self, c: Color32) -> Color32 {
        let key = [c.r(), c.g(), c.b()];
        if let Some(hit) = self.cache.get(&key) {
            return *hit;
        }
        let l = lab(c);
        let best = self
            .lab
            .iter()
            .enumerate()
            .map(|(i, p)| {
                // Lightness counts double: shading reads better than exact hue.
                let d = 2.0 * (l[0] - p[0]).powi(2) + (l[1] - p[1]).powi(2) + (l[2] - p[2]).powi(2);
                (i, d)
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map_or(c, |(i, _)| self.colors[i]);
        self.cache.insert(key, best);
        best
    }

    /// Every opaque pixel of `image` mapped; transparent ones stay transparent.
    pub fn apply(&mut self, image: &mut ColorImage) {
        for px in &mut image.pixels {
            if px.a() > 0 {
                *px = self.map(*px);
            }
        }
    }
}

/// sRGB → CIELAB (D65).
fn lab(c: Color32) -> [f32; 3] {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (lin(c.r()), lin(c.g()), lin(c.b()));
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
    let f = |t: f32| {
        if t > 0.008856 {
            t.cbrt()
        } else {
            7.787 * t + 16.0 / 116.0
        }
    };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// Rasterizes `shapes` (in points) inside `region` to a `size` image: each pixel averages
/// `supersample`² samples, and is then either opaque or transparent. Text shapes are skipped.
pub fn rasterize(
    shapes: &[Shape],
    region: Rect,
    size: [usize; 2],
    supersample: usize,
) -> ColorImage {
    let ss = supersample.max(1);
    let (w, h) = (size[0] * ss, size[1] * ss);
    let scale = w as f32 / region.width();
    let mut tess = Tessellator::new(
        scale,
        TessellationOptions {
            feathering: false,
            ..Default::default()
        },
        [1, 1],
        Vec::new(),
    );
    // Premultiplied sRGB, blended in order.
    let mut buf = vec![[0.0f32; 4]; w * h];
    for shape in shapes {
        if matches!(shape, Shape::Text(_)) {
            continue;
        }
        let mut mesh = Mesh::default();
        tess.tessellate_shape(shape.clone(), &mut mesh);
        for tri in mesh.indices.chunks_exact(3) {
            let tri: [u32; 3] = [tri[0], tri[1], tri[2]];
            let v = tri.map(|i| &mesh.vertices[i as usize]);
            let p = v.map(|v| {
                let q = (v.pos - region.min) * scale;
                Pos2::new(q.x, q.y)
            });
            fill_triangle(&mut buf, w, h, p, v.map(|v| v.color));
        }
    }
    let mut image = ColorImage::filled(size, Color32::TRANSPARENT);
    let n = (ss * ss) as f32;
    for y in 0..size[1] {
        for x in 0..size[0] {
            let mut sum = [0.0f32; 4];
            for sy in 0..ss {
                for sx in 0..ss {
                    let s = buf[(y * ss + sy) * w + x * ss + sx];
                    for (a, b) in sum.iter_mut().zip(s) {
                        *a += b;
                    }
                }
            }
            let a = sum[3] / n;
            if a >= 0.5 {
                let rgb = |i: usize| (sum[i] / n / a * 255.0).round().clamp(0.0, 255.0) as u8;
                image[(x, y)] = Color32::from_rgb(rgb(0), rgb(1), rgb(2));
            }
        }
    }
    image
}

fn fill_triangle(buf: &mut [[f32; 4]], w: usize, h: usize, p: [Pos2; 3], c: [Color32; 3]) {
    let edge = |a: Pos2, b: Pos2, q: Pos2| (b.x - a.x) * (q.y - a.y) - (b.y - a.y) * (q.x - a.x);
    let area = edge(p[0], p[1], p[2]);
    if area.abs() < 1e-6 {
        return;
    }
    let min_x = p
        .iter()
        .map(|q| q.x)
        .fold(f32::MAX, f32::min)
        .floor()
        .max(0.0) as usize;
    let max_x = (p.iter().map(|q| q.x).fold(f32::MIN, f32::max).ceil() as usize).min(w);
    let min_y = p
        .iter()
        .map(|q| q.y)
        .fold(f32::MAX, f32::min)
        .floor()
        .max(0.0) as usize;
    let max_y = (p.iter().map(|q| q.y).fold(f32::MIN, f32::max).ceil() as usize).min(h);
    let col = c.map(|c| {
        [
            c.r() as f32 / 255.0,
            c.g() as f32 / 255.0,
            c.b() as f32 / 255.0,
            c.a() as f32 / 255.0,
        ]
    });
    for y in min_y..max_y {
        for x in min_x..max_x {
            let q = Pos2::new(x as f32 + 0.5, y as f32 + 0.5);
            let w0 = edge(p[1], p[2], q) / area;
            let w1 = edge(p[2], p[0], q) / area;
            let w2 = edge(p[0], p[1], q) / area;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            let src: [f32; 4] =
                std::array::from_fn(|i| col[0][i] * w0 + col[1][i] * w1 + col[2][i] * w2);
            let dst = &mut buf[y * w + x];
            for i in 0..4 {
                dst[i] = src[i] + dst[i] * (1.0 - src[3]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{pos2, vec2};

    #[test]
    fn reads_lospec_palettes() {
        let p = Palette::from_lospec(r##"{"name":"Two","colors":["ff0000","#00ff00"]}"##).unwrap();
        assert_eq!(p.name, "Two");
        assert_eq!(p.colors, vec![Color32::RED, Color32::GREEN]);
        assert!(Palette::from_lospec(r#"{"name":"None","colors":[]}"#).is_err());
    }

    #[test]
    fn maps_to_the_nearest_palette_colour() {
        let palette = Palette {
            name: "bw".into(),
            colors: vec![Color32::BLACK, Color32::WHITE],
        };
        let mut m = Mapper::new(&palette);
        assert_eq!(m.map(Color32::from_rgb(30, 20, 25)), Color32::BLACK);
        assert_eq!(m.map(Color32::from_rgb(230, 240, 220)), Color32::WHITE);
    }

    /// A square covering the left half of the region fills exactly the left half of the
    /// image, opaque, and leaves the rest transparent.
    #[test]
    fn rasterizes_with_hard_alpha() {
        let region = Rect::from_min_size(pos2(10.0, 10.0), vec2(8.0, 8.0));
        let square = Shape::rect_filled(
            Rect::from_min_size(pos2(10.0, 10.0), vec2(4.0, 8.0)),
            0.0,
            Color32::from_rgb(200, 100, 50),
        );
        let image = rasterize(&[square], region, [8, 8], 4);
        for y in 0..8 {
            for x in 0..8 {
                let px = image[(x, y)];
                if x < 4 {
                    assert_eq!(px, Color32::from_rgb(200, 100, 50), "({x}, {y})");
                } else {
                    assert_eq!(px.a(), 0, "({x}, {y})");
                }
            }
        }
    }
}

//! Paper cutouts: flat shapes with a cream border and a drop shadow, as if cut from card and
//! laid on the scene. A piece is a set of polygons in its own coordinates, placed by an
//! affine [`Xf`]; pieces drawn later lie on top and cast their shadow on the ones below.
//!
//! egui fills only convex paths, so polygons are triangulated here (ear clipping) and drawn
//! as meshes. Shape helpers build smooth outlines from a few control points.

use eframe::egui::{Color32, Mesh, Painter, Pos2, Shape, Stroke, Vec2, pos2, vec2};

/// An affine placement: scale, then rotate, then translate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Xf {
    /// Column vectors of the linear part.
    pub x: Vec2,
    pub y: Vec2,
    pub origin: Pos2,
}

impl Xf {
    pub const IDENTITY: Self = Self {
        x: Vec2::X,
        y: Vec2::Y,
        origin: Pos2::ZERO,
    };

    pub fn translate(offset: Vec2) -> Self {
        Self {
            origin: offset.to_pos2(),
            ..Self::IDENTITY
        }
    }

    pub fn scale(x: f32, y: f32) -> Self {
        Self {
            x: vec2(x, 0.0),
            y: vec2(0.0, y),
            origin: Pos2::ZERO,
        }
    }

    /// Clockwise on screen (y down) by `radians`.
    pub fn rotate(radians: f32) -> Self {
        let (s, c) = radians.sin_cos();
        Self {
            x: vec2(c, s),
            y: vec2(-s, c),
            origin: Pos2::ZERO,
        }
    }

    /// Rotation by `radians` about `pivot`.
    pub fn rotate_about(pivot: Pos2, radians: f32) -> Self {
        Self::translate(pivot.to_vec2())
            .then(Self::rotate(radians))
            .then(Self::translate(-pivot.to_vec2()))
    }

    /// `self` applied after `inner`: points go through `inner` first.
    pub fn then(self, inner: Self) -> Self {
        Self {
            x: self.vec(inner.x),
            y: self.vec(inner.y),
            origin: self.apply(inner.origin),
        }
    }

    pub fn vec(&self, v: Vec2) -> Vec2 {
        self.x * v.x + self.y * v.y
    }

    pub fn apply(&self, p: Pos2) -> Pos2 {
        self.origin + self.vec(p.to_vec2())
    }

    /// The factor lengths grow by (for stroke widths), assuming no extreme shear.
    pub fn length_scale(&self) -> f32 {
        (self.x.length() * self.y.length()).sqrt()
    }
}

/// One polygon of a piece.
#[derive(Debug, Clone)]
pub struct Poly {
    pub points: Vec<Pos2>,
    pub fill: Color32,
    /// Whether it adds to the piece's border and shadow (false for details drawn inside it).
    pub outline: bool,
}

/// Shapes cut from one card: they share a border and a shadow.
#[derive(Debug, Clone, Default)]
pub struct Piece {
    pub polys: Vec<Poly>,
    pub xf: Option<Xf>,
}

impl Piece {
    /// Adds a polygon that shapes the piece's silhouette.
    pub fn cut(&mut self, points: Vec<Pos2>, fill: Color32) -> &mut Self {
        self.polys.push(Poly {
            points,
            fill,
            outline: true,
        });
        self
    }

    /// Adds a detail painted on the piece, inside its silhouette.
    pub fn paint(&mut self, points: Vec<Pos2>, fill: Color32) -> &mut Self {
        self.polys.push(Poly {
            points,
            fill,
            outline: false,
        });
        self
    }
}

/// How the cutouts look.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Card {
    /// Border width in piece units.
    pub border: f32,
    pub border_color: Color32,
    /// Shadow offset in piece units, before the piece's placement.
    pub shadow: Vec2,
    pub shadow_color: Color32,
}

impl Default for Card {
    fn default() -> Self {
        Self {
            border: 1.6,
            border_color: Color32::from_rgb(0xee, 0xe2, 0xc6),
            shadow: vec2(1.6, 2.2),
            shadow_color: Color32::from_black_alpha(120),
        }
    }
}

/// Draws `piece` placed by `xf` (after its own placement): shadow, border, then fills.
pub fn draw(painter: &Painter, piece: &Piece, xf: Xf, card: &Card) {
    let xf = piece.xf.map_or(xf, |own| xf.then(own));
    let outlined = || piece.polys.iter().filter(|p| p.outline);
    let width = 2.0 * card.border * xf.length_scale();
    if card.shadow_color.a() > 0 {
        let shadow = xf.vec(card.shadow);
        for poly in outlined() {
            let points: Vec<Pos2> = poly.points.iter().map(|p| xf.apply(*p) + shadow).collect();
            fill(painter, &points, card.shadow_color);
            if width > 0.0 {
                painter.add(Shape::closed_line(
                    points,
                    Stroke::new(width, card.shadow_color),
                ));
            }
        }
    }
    if width > 0.0 {
        for poly in outlined() {
            let points: Vec<Pos2> = poly.points.iter().map(|p| xf.apply(*p)).collect();
            painter.add(Shape::closed_line(
                points,
                Stroke::new(width, card.border_color),
            ));
        }
    }
    for poly in &piece.polys {
        let points: Vec<Pos2> = poly.points.iter().map(|p| xf.apply(*p)).collect();
        fill(painter, &points, poly.fill);
    }
}

/// A brass split pin at `at` (piece units), placed by `xf`.
pub fn pin(painter: &Painter, at: Pos2, radius: f32, xf: Xf) {
    let c = xf.apply(at);
    let r = radius * xf.length_scale();
    painter.circle_filled(c, r, Color32::from_rgb(0x6b, 0x4a, 0x1c));
    painter.circle_filled(c, r * 0.72, Color32::from_rgb(0xd9, 0xa8, 0x3e));
    painter.circle_filled(
        c - vec2(r, r) * 0.25,
        r * 0.28,
        Color32::from_rgb(0xff, 0xe6, 0x9a),
    );
}

/// Fills a simple polygon (convex or not) of any winding.
pub fn fill(painter: &Painter, points: &[Pos2], color: Color32) {
    let triangles = triangulate(points);
    if triangles.is_empty() {
        return;
    }
    let mut mesh = Mesh::default();
    for p in points {
        mesh.colored_vertex(*p, color);
    }
    for [a, b, c] in triangles {
        mesh.add_triangle(a, b, c);
    }
    painter.add(Shape::mesh(mesh));
}

/// Ear-clipping triangulation of a simple polygon; indices into `points`.
pub fn triangulate(points: &[Pos2]) -> Vec<[u32; 3]> {
    let n = points.len();
    if n < 3 {
        return Vec::new();
    }
    let ccw = signed_area(points) > 0.0;
    let mut left: Vec<usize> = (0..n).collect();
    let mut out = Vec::with_capacity(n - 2);
    let mut guard = 0;
    while left.len() > 3 && guard < n * n {
        guard += 1;
        let m = left.len();
        let ear = (0..m).find(|&i| {
            let (a, b, c) = (left[(i + m - 1) % m], left[i], left[(i + 1) % m]);
            let (pa, pb, pc) = (points[a], points[b], points[c]);
            let turn = cross(pb - pa, pc - pb);
            let convex = if ccw { turn > 0.0 } else { turn < 0.0 };
            convex
                && left
                    .iter()
                    .filter(|&&j| j != a && j != b && j != c)
                    .all(|&j| !inside(points[j], pa, pb, pc))
        });
        // A degenerate or self-touching outline has no ear: clip anyway rather than loop.
        let i = ear.unwrap_or(0);
        let (a, b, c) = (left[(i + m - 1) % m], left[i], left[(i + 1) % m]);
        out.push([a as u32, b as u32, c as u32]);
        left.remove(i);
    }
    if left.len() == 3 {
        out.push([left[0] as u32, left[1] as u32, left[2] as u32]);
    }
    out
}

fn cross(a: Vec2, b: Vec2) -> f32 {
    a.x * b.y - a.y * b.x
}

fn signed_area(points: &[Pos2]) -> f32 {
    let n = points.len();
    (0..n)
        .map(|i| cross(points[i].to_vec2(), points[(i + 1) % n].to_vec2()))
        .sum::<f32>()
        / 2.0
}

fn inside(p: Pos2, a: Pos2, b: Pos2, c: Pos2) -> bool {
    let d1 = cross(b - a, p - a);
    let d2 = cross(c - b, p - b);
    let d3 = cross(a - c, p - c);
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

/// An ellipse centred on `c`, `steps` points.
pub fn ellipse(c: Pos2, rx: f32, ry: f32, steps: usize) -> Vec<Pos2> {
    (0..steps)
        .map(|i| {
            let a = i as f32 / steps as f32 * std::f32::consts::TAU;
            pos2(c.x + rx * a.cos(), c.y + ry * a.sin())
        })
        .collect()
}

/// A stadium from `a` to `b`, `r` wide on each side.
pub fn capsule(a: Pos2, b: Pos2, r: f32) -> Vec<Pos2> {
    let d = (b - a).normalized();
    let base = d.y.atan2(d.x);
    let half = 8;
    let arc = |c: Pos2, from: f32| {
        (0..=half).map(move |i| {
            let t = from + i as f32 / half as f32 * std::f32::consts::PI;
            pos2(c.x + r * t.cos(), c.y + r * t.sin())
        })
    };
    let quarter = std::f32::consts::FRAC_PI_2;
    arc(b, base - quarter)
        .chain(arc(a, base + quarter))
        .collect()
}

/// A smooth closed outline through `control` (Catmull-Rom), `per` points per segment.
pub fn blob(control: &[Pos2], per: usize) -> Vec<Pos2> {
    let n = control.len();
    let mut out = Vec::with_capacity(n * per);
    for i in 0..n {
        let p0 = control[(i + n - 1) % n].to_vec2();
        let p1 = control[i].to_vec2();
        let p2 = control[(i + 1) % n].to_vec2();
        let p3 = control[(i + 2) % n].to_vec2();
        for s in 0..per {
            let t = s as f32 / per as f32;
            let (t2, t3) = (t * t, t * t * t);
            let v = 0.5
                * ((2.0 * p1)
                    + (p2 - p0) * t
                    + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
                    + (3.0 * p1 - p0 - 3.0 * p2 + p3) * t3);
            out.push(v.to_pos2());
        }
    }
    out
}

/// `points` placed by `xf`.
pub fn place(points: Vec<Pos2>, xf: Xf) -> Vec<Pos2> {
    points.into_iter().map(|p| xf.apply(p)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area_of(points: &[Pos2], tris: &[[u32; 3]]) -> f32 {
        tris.iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| points[i as usize]);
                cross(b - a, c - a).abs() / 2.0
            })
            .sum()
    }

    /// A concave outline (an L) is covered exactly, whichever way it winds.
    #[test]
    fn triangulates_concave_polygons() {
        let mut l = vec![
            pos2(0.0, 0.0),
            pos2(4.0, 0.0),
            pos2(4.0, 1.0),
            pos2(1.0, 1.0),
            pos2(1.0, 3.0),
            pos2(0.0, 3.0),
        ];
        for _ in 0..2 {
            let tris = triangulate(&l);
            assert_eq!(tris.len(), l.len() - 2);
            assert!((area_of(&l, &tris) - signed_area(&l).abs()).abs() < 1e-4);
            l.reverse();
        }
    }

    #[test]
    fn blobs_pass_through_their_control_points() {
        let control = [pos2(0.0, 0.0), pos2(10.0, 0.0), pos2(5.0, 8.0)];
        let outline = blob(&control, 6);
        assert_eq!(outline.len(), 18);
        for (i, c) in control.iter().enumerate() {
            assert!((outline[i * 6] - *c).length() < 1e-4);
        }
    }

    #[test]
    fn transforms_compose_inner_first() {
        let xf = Xf::translate(vec2(10.0, 0.0)).then(Xf::scale(2.0, 2.0));
        assert_eq!(xf.apply(pos2(1.0, 1.0)), pos2(12.0, 2.0));
        let turn = Xf::rotate_about(pos2(1.0, 0.0), std::f32::consts::FRAC_PI_2);
        let p = turn.apply(pos2(2.0, 0.0));
        assert!((p - pos2(1.0, 1.0)).length() < 1e-5);
    }
}

//! Repo regions: a labelled translucent rounded rectangle behind each repo's nodes.

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use eframe::egui::epaint::RectShape;
use eframe::egui::{
    Color32, CornerRadius, FontFamily, FontId, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2, Visuals,
};

use super::layout::{REGION_PADDING, region_rect};

/// Fill opacity of a region.
const FILL_ALPHA: f32 = 0.07;
/// Outline opacity of a region.
const STROKE_ALPHA: f32 = 0.3;
/// Repo name size in canvas units; clamped on screen.
const LABEL_SIZE: f32 = 13.0;
const LABEL_MIN_PX: f32 = 7.0;
const LABEL_MAX_PX: f32 = 18.0;

/// One repo's region in canvas coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    pub repo: String,
    pub rect: Rect,
}

/// The region of each repo among `nodes` (repo, canvas position), by repo name.
pub fn regions<'a>(nodes: impl IntoIterator<Item = (&'a str, Pos2)>) -> Vec<Region> {
    let mut points: BTreeMap<&str, Vec<Pos2>> = BTreeMap::new();
    for (repo, pos) in nodes {
        points.entry(repo).or_default().push(pos);
    }
    points
        .into_iter()
        .map(|(repo, points)| Region {
            repo: repo.to_string(),
            rect: region_rect(&points),
        })
        .collect()
}

/// A muted color per repo, stable across launches.
pub fn region_color(repo: &str, dark_mode: bool) -> Color32 {
    let mut hasher = DefaultHasher::new();
    repo.hash(&mut hasher);
    let hue = (hasher.finish() % 360) as f32 / 360.0;
    let value = if dark_mode { 0.85 } else { 0.55 };
    eframe::egui::ecolor::Hsva::new(hue, 0.45, value, 1.0).into()
}

/// Screen size of a region label at `zoom`.
fn label_px(zoom: f32) -> f32 {
    (LABEL_SIZE * zoom).clamp(LABEL_MIN_PX, LABEL_MAX_PX)
}

/// Where a region's label starts relative to the region's top-left corner, on screen.
fn label_offset(screen_width: f32) -> Vec2 {
    Vec2::new(8.0_f32.min(screen_width / 4.0), 4.0)
}

/// Size of `repo`'s label drawn at its largest; [`region_extent`] scales it.
pub fn label_size_at_max(ctx: &eframe::egui::Context, repo: &str) -> Vec2 {
    ctx.fonts_mut(|f| {
        f.layout_no_wrap(
            repo.to_owned(),
            FontId::new(LABEL_MAX_PX, FontFamily::Proportional),
            Color32::WHITE,
        )
    })
    .size()
}

/// Screen area a region of canvas size `size` covers at `zoom`, relative to its center's
/// screen position: its rectangle and its label (`label` from [`label_size_at_max`]).
pub fn region_extent(size: Vec2, label: Vec2, zoom: f32) -> Rect {
    let rect = Rect::from_center_size(Pos2::ZERO, size * zoom);
    let label = label * (label_px(zoom) / LABEL_MAX_PX);
    let min = rect.min + label_offset(rect.width());
    rect.union(Rect::from_min_size(min, label + Vec2::splat(2.0)))
}

/// Shapes of `regions` drawn at `zoom` with canvas origin at screen point `origin`.
pub fn region_shapes(
    regions: &[Region],
    zoom: f32,
    origin: Vec2,
    visuals: &Visuals,
    fonts: &eframe::egui::Context,
) -> Vec<Shape> {
    let mut shapes = Vec::with_capacity(regions.len() * 2);
    for region in regions {
        let rect = Rect::from_min_max(
            (region.rect.min.to_vec2() * zoom + origin).to_pos2(),
            (region.rect.max.to_vec2() * zoom + origin).to_pos2(),
        );
        let color = region_color(&region.repo, visuals.dark_mode);
        let radius = (REGION_PADDING * zoom * 0.5).clamp(2.0, 255.0) as u8;
        shapes.push(
            RectShape::new(
                rect,
                CornerRadius::same(radius),
                color.gamma_multiply(FILL_ALPHA),
                Stroke::new(1.0, color.gamma_multiply(STROKE_ALPHA)),
                StrokeKind::Inside,
            )
            .into(),
        );
        let px = label_px(zoom);
        let text_color = visuals.text_color().gamma_multiply(0.75);
        let galley = fonts.fonts_mut(|f| {
            f.layout_no_wrap(
                region.repo.clone(),
                FontId::new(px, FontFamily::Proportional),
                text_color,
            )
        });
        let pos = rect.min + label_offset(rect.width());
        shapes.push(Shape::galley(pos, galley, text_color));
    }
    shapes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_region_per_repo_around_its_nodes() {
        let regions = regions([
            ("b", Pos2::new(100.0, 100.0)),
            ("a", Pos2::new(0.0, 0.0)),
            ("b", Pos2::new(200.0, 150.0)),
        ]);
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0].repo, "a");
        assert!(regions[0].rect.contains(Pos2::ZERO));
        let b = &regions[1];
        assert!(b.rect.contains(Pos2::new(100.0, 100.0)));
        assert!(b.rect.contains(Pos2::new(200.0, 150.0)));
        assert!(b.rect.min.x <= 100.0 - REGION_PADDING);
    }

    #[test]
    fn repo_colors_are_stable_and_differ() {
        assert_eq!(region_color("shop", true), region_color("shop", true));
        assert_ne!(region_color("shop", true), region_color("infra", true));
    }
}

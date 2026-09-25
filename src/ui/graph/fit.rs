//! Fitting the whole graph into the widget: nodes, rings and labels, with a margin.

use eframe::egui::{Pos2, Rect, Ui, Vec2};
use egui_graphs::MetadataFrame;

use super::shape::{label_size_at_max, node_extent};
use super::{GraphView, MIN_ZOOM};

/// Space kept free between the graph and the widget edge, in screen points.
pub const FIT_MARGIN: f32 = 24.0;
/// Largest zoom a fit may pick, so a small graph isn't blown up.
pub const FIT_MAX_ZOOM: f32 = 2.0;
/// Bisection steps when searching the fitting zoom.
const FIT_STEPS: usize = 32;

/// Something drawn around a canvas point whose screen size depends on the zoom.
pub trait FitItem {
    fn center(&self) -> Pos2;
    /// Screen area covered at `zoom`, relative to the center's screen position.
    fn extent(&self, zoom: f32) -> Rect;
}

/// Screen bounds of `items` at `zoom`, with zero pan.
fn bounds_at<T: FitItem>(items: &[T], zoom: f32) -> Option<Rect> {
    items
        .iter()
        .map(|item| item.extent(zoom).translate(item.center().to_vec2() * zoom))
        .reduce(|a, b| a.union(b))
}

/// The largest zoom (up to `max_zoom`) at which `items` fit a view of `view_size` minus
/// `margin` on every side, and the widget-local pan centering them. `None` without items.
pub fn fit_transform<T: FitItem>(
    items: &[T],
    view_size: Vec2,
    margin: f32,
    max_zoom: f32,
) -> Option<(f32, Vec2)> {
    let room = (view_size - Vec2::splat(2.0 * margin)).max(Vec2::splat(1.0));
    let fits = |zoom: f32| {
        bounds_at(items, zoom).is_some_and(|b| b.width() <= room.x && b.height() <= room.y)
    };
    // The bounds only grow with the zoom, so the fitting zooms form an interval from MIN_ZOOM.
    let max_zoom = max_zoom.max(MIN_ZOOM);
    let zoom = if fits(max_zoom) {
        max_zoom
    } else {
        let (mut lo, mut hi) = (MIN_ZOOM, max_zoom);
        for _ in 0..FIT_STEPS {
            let mid = (lo + hi) / 2.0;
            if fits(mid) { lo = mid } else { hi = mid }
        }
        lo
    };
    let bounds = bounds_at(items, zoom)?;
    Some((zoom, view_size / 2.0 - bounds.center().to_vec2()))
}

/// A graph node as the fit sees it.
struct NodeFit {
    center: Pos2,
    payload: super::NodePayload,
    label: Vec2,
    selected: bool,
}

impl FitItem for NodeFit {
    fn center(&self) -> Pos2 {
        self.center
    }

    fn extent(&self, zoom: f32) -> Rect {
        node_extent(&self.payload, self.label, self.selected, zoom)
    }
}

/// A repo region as the fit sees it.
struct RegionFit {
    center: Pos2,
    size: Vec2,
    label: Vec2,
}

impl FitItem for RegionFit {
    fn center(&self) -> Pos2 {
        self.center
    }

    fn extent(&self, zoom: f32) -> Rect {
        super::regions::region_extent(self.size, self.label, zoom)
    }
}

/// A node or a region.
enum Fit {
    Node(NodeFit),
    Region(RegionFit),
}

impl FitItem for Fit {
    fn center(&self) -> Pos2 {
        match self {
            Fit::Node(n) => n.center(),
            Fit::Region(r) => r.center(),
        }
    }

    fn extent(&self, zoom: f32) -> Rect {
        match self {
            Fit::Node(n) => n.extent(zoom),
            Fit::Region(r) => r.extent(zoom),
        }
    }
}

impl GraphView {
    /// Fits every visible node into a graph widget at `rect`, which egui_graphs is about to
    /// draw into this frame. Returns whether there was anything to fit.
    pub(super) fn apply_fit(&mut self, ui: &mut Ui, rect: Rect) -> bool {
        let ctx = ui.ctx().clone();
        let mut items: Vec<Fit> = self
            .graph
            .nodes_iter()
            .map(|(_, n)| {
                Fit::Node(NodeFit {
                    center: n.location(),
                    payload: n.payload().clone(),
                    label: label_size_at_max(&ctx, &n.label()),
                    selected: n.selected(),
                })
            })
            .collect();
        items.extend(self.repo_regions().into_iter().map(|r| {
            Fit::Region(RegionFit {
                center: r.rect.center(),
                size: r.rect.size(),
                label: super::regions::label_size_at_max(&ctx, &r.repo),
            })
        }));
        let Some((zoom, pan)) = fit_transform(&items, rect.size(), FIT_MARGIN, FIT_MAX_ZOOM) else {
            return false;
        };
        let mut meta = MetadataFrame::new(Some(self.id.clone())).load(ui);
        meta.zoom = zoom;
        // egui_graphs shifts the pan by how far the widget moved since its last frame.
        meta.pan = pan - self.moved_by(rect);
        meta.save(ui);
        self.fitted_size = Some(rect.size());
        true
    }

    /// How far the widget's top-left corner moved since the last frame it was shown.
    fn moved_by(&self, rect: Rect) -> Vec2 {
        self.last_top_left
            .map_or(Vec2::ZERO, |last| rect.left_top() - last)
    }
}

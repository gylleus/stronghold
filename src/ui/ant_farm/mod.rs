//! The ant farm: a side view of the fleet where an ant's place shows its state. An
//! alternative to the graph, drawn from the same snapshot, filters and selection.
//!
//! [`layout`] decides where everything goes, [`motion`] walks ants to new places and
//! [`sprites`] draws them; this module ties them to egui and handles input.

pub mod layout;
#[cfg(test)]
mod layout_tests;
pub mod motion;
pub mod sprites;

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use eframe::egui::{self, Align2, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, vec2};

use crate::model::{FleetSnapshot, NodeId};
use crate::ui::graph::GraphView;
use layout::{FarmInput, Pose, Scene, Spot, layout};
use motion::{Walk, route};
use sprites::{AntLook, Item, Palette, Sprites};

/// Redraw interval while an ant walks.
const WALK_FRAME: Duration = Duration::from_millis(33);
/// Redraw interval of the idle animations (digging, waving, the whip): stepped like pixel
/// art, and slow enough that a farm full of working ants stays cheap.
const STEP_FRAME: Duration = Duration::from_millis(250);
/// Walk frames per second.
const WALK_FPS: f64 = 8.0;
/// Dig and wave steps per second; matches [`STEP_FRAME`].
const STEP_FPS: f64 = 4.0;

/// What happened in the farm this frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AntFarmResponse {
    /// The ant clicked this frame.
    pub clicked: Option<NodeId>,
    /// Empty ground was clicked (clears the selection, as in the graph).
    pub clicked_empty: bool,
    pub hovered: Option<NodeId>,
    /// Nodes to close or whip, picked in an ant's context menu.
    pub close_requested: Vec<NodeId>,
    pub whip_requested: Vec<NodeId>,
}

/// An ant's place and its walk there, if it is still walking.
#[derive(Debug, Clone)]
struct Track {
    spot: Spot,
    /// Where the spot was last frame, relative to the farm's rect (see [`motion::to_unit`]).
    at: Pos2,
    walk: Option<Walk>,
}

/// Where an ant is drawn this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
    pub pos: Pos2,
    /// Direction of travel while walking.
    pub heading: Option<Vec2>,
}

#[derive(Default)]
pub struct AntFarmView {
    sprites: Option<Sprites>,
    tracks: HashMap<NodeId, Track>,
    /// Ant whose context menu is open (the one right-clicked last).
    context_node: Option<NodeId>,
}

impl AntFarmView {
    pub fn new() -> Self {
        Self::default()
    }

    /// Draws the farm filling the available space and handles clicks, hover and the context
    /// menu. `graph` supplies the filters, selection, PR badges, tooltips and menu, so both
    /// views act alike.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        snapshot: &FleetSnapshot,
        graph: &GraphView,
    ) -> AntFarmResponse {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click());
        let now_s = ui.input(|i| i.time);
        let input = FarmInput {
            snapshot,
            hidden: graph.hidden(),
            pr_ready: graph.pr_ready(),
            now_ms: now_ms(),
        };
        let scene = layout(&input, rect);
        let placed = self.advance(&scene, now_s);
        let sprites = self.sprites.get_or_insert_with(|| Sprites::load(ui.ctx()));
        let painter = ui.painter_at(rect);
        let palette = Palette::new(ui.visuals().dark_mode);
        let selection = graph.selection();

        paint_ground(&painter, sprites, &scene, palette);
        let pointer = response.hover_pos();
        let hovered = pointer.and_then(|p| hit(&scene, &placed, p));
        paint_ants(
            &painter,
            sprites,
            &scene,
            &placed,
            now_s,
            &selection,
            hovered.as_ref(),
            ui,
        );

        let walking = placed.iter().any(|p| p.heading.is_some());
        let stepping = scene.ants.iter().any(|a| {
            matches!(a.pose, Pose::Dig | Pose::Wait) || a.mark == Some(layout::Mark::Whip)
        });
        if walking {
            ui.ctx().request_repaint_after(WALK_FRAME);
        } else if stepping {
            ui.ctx().request_repaint_after(STEP_FRAME);
        }

        if let Some(id) = &hovered {
            response
                .clone()
                .on_hover_ui_at_pointer(|ui| graph.node_tooltip_ui(ui, id));
        }
        if response.secondary_clicked() {
            self.context_node = hovered.clone();
        }
        let menu = match &self.context_node {
            Some(id) => graph.node_menu(&response, id),
            None => Default::default(),
        };
        AntFarmResponse {
            clicked: response.clicked().then(|| hovered.clone()).flatten(),
            clicked_empty: response.clicked() && hovered.is_none(),
            hovered,
            close_requested: menu.close,
            whip_requested: menu.whip,
        }
    }

    /// Starts a walk for every ant whose spot changed, drops finished walks and gone ants,
    /// and returns where each of `scene.ants` is at `now_s`.
    pub fn advance(&mut self, scene: &Scene, now_s: f64) -> Vec<Placed> {
        self.tracks
            .retain(|id, _| scene.ants.iter().any(|a| &a.id == id));
        let mut current: HashMap<&NodeId, Placed> = HashMap::new();
        let mut placed = Vec::with_capacity(scene.ants.len());
        for ant in &scene.ants {
            // Subagents keep their place behind their parent wherever it walks.
            if let Spot::Trail(parent, _) = &ant.spot
                && let (Some(p), Some(parent_ant)) = (current.get(parent), scene.ant(parent))
            {
                let at = Placed {
                    pos: p.pos + (ant.pos - parent_ant.pos),
                    heading: p.heading,
                };
                self.tracks.insert(
                    ant.id.clone(),
                    Track {
                        spot: ant.spot.clone(),
                        at: motion::to_unit(ant.pos, scene.rect),
                        walk: None,
                    },
                );
                current.insert(&ant.id, at);
                placed.push(at);
                continue;
            }
            let track = self.tracks.entry(ant.id.clone()).or_insert_with(|| Track {
                spot: ant.spot.clone(),
                at: motion::to_unit(ant.pos, scene.rect),
                walk: None,
            });
            if track.spot != ant.spot {
                let from = match &track.walk {
                    Some(walk) => walk.sample(scene.rect, now_s).0,
                    None => motion::from_unit(track.at, scene.rect),
                };
                let same_area = track.spot.area() == ant.spot.area();
                let path = route(from, ant.pos, same_area, scene.gallery.center().y);
                track.walk = Some(Walk::new(&path, scene.rect, now_s));
                track.spot = ant.spot.clone();
            }
            track.at = motion::to_unit(ant.pos, scene.rect);
            if let Some(walk) = &mut track.walk {
                walk.retarget(ant.pos, scene.rect);
                if walk.done(now_s) {
                    track.walk = None;
                }
            }
            let at = match &track.walk {
                Some(walk) => {
                    let (pos, heading) = walk.sample(scene.rect, now_s);
                    Placed {
                        pos,
                        heading: Some(heading),
                    }
                }
                None => Placed {
                    pos: ant.pos,
                    heading: None,
                },
            };
            current.insert(&ant.id, at);
            placed.push(at);
        }
        placed
    }
}

/// The ant under `pointer`, topmost first.
fn hit(scene: &Scene, placed: &[Placed], pointer: Pos2) -> Option<NodeId> {
    scene
        .ants
        .iter()
        .zip(placed)
        .rev()
        .find(|(ant, at)| Rect::from_center_size(at.pos, Vec2::splat(ant.size)).contains(pointer))
        .map(|(ant, _)| ant.id.clone())
}

fn paint_ground(painter: &egui::Painter, sprites: &Sprites, scene: &Scene, palette: Palette) {
    let px = scene.px;
    let a = scene.cell();
    sprites.ground(painter, scene.rect, scene.surface_y, px, palette);
    // Shafts up from the gallery to the landing, the Queen's chamber and storage.
    let gy = scene.gallery.center().y;
    let up = |x: f32, top: f32| {
        Rect::from_min_max(egui::pos2(x - 0.6 * a, top), egui::pos2(x + 0.6 * a, gy))
    };
    for shaft in [
        up(scene.landing.center().x, scene.surface_y - px),
        up(
            scene.queen_chamber.center().x,
            scene.queen_chamber.bottom() - px,
        ),
        up(scene.storage.center().x, scene.storage.bottom() - px),
    ] {
        sprites.passage(painter, shaft, px);
    }
    for tunnel in &scene.tunnels {
        sprites.passage(painter, tunnel.shaft, px);
    }
    sprites.passage(painter, scene.gallery, px);
    sprites.passage(painter, scene.queen_chamber, px);
    sprites.passage(painter, scene.storage, px);
    for tunnel in &scene.tunnels {
        sprites.passage(painter, tunnel.chamber, px);
    }

    // Each tunnel's name and progress under the gallery, above its shaft.
    let font = FontId::monospace(9.0 + 2.0 * px);
    for tunnel in &scene.tunnels {
        let top = egui::pos2(tunnel.shaft.center().x, scene.gallery.bottom() + 3.0 * px);
        let width = tunnel.chamber.width() + 0.4 * a;
        let text = label(
            painter,
            top,
            Align2::CENTER_TOP,
            &tunnel.label,
            font.clone(),
            width,
            palette,
        );
        // The run's task progress: a bar filling as tasks complete, and the count.
        if let (Some(progress), Some((done, total))) = (tunnel.progress, tunnel.tasks) {
            let count = format!("{done}/{total} tasks done");
            let small = FontId::monospace(8.0 + 1.5 * px);
            let bar_w = width.min(text.width().max(3.0 * a));
            let bar = Rect::from_center_size(
                egui::pos2(top.x, text.bottom() + 3.0 * px),
                vec2(bar_w, 2.0 * px),
            );
            painter.rect_filled(bar.expand(px), 0.0, palette.label_shadow);
            painter.rect_filled(bar, 0.0, egui::Color32::from_rgb(0x5a, 0x3a, 0x22));
            let filled = Rect::from_min_size(bar.min, vec2(bar.width() * progress, bar.height()));
            painter.rect_filled(filled, 0.0, egui::Color32::from_rgb(0x8f, 0xd0, 0x5a));
            label(
                painter,
                egui::pos2(top.x, bar.bottom() + 2.0 * px),
                Align2::CENTER_TOP,
                &count,
                small,
                width,
                palette,
            );
        }
    }
    sprites.sign(
        painter,
        egui::pos2(scene.rect.left() + 0.3 * a, scene.surface_y),
        px,
        "Karl",
        FontId::monospace(10.0 + 2.0 * px),
        palette,
    );
    label(
        painter,
        scene.storage.left_top() + vec2(2.0 * px, 2.0 * px),
        Align2::LEFT_TOP,
        "Ready for review",
        font,
        scene.storage.width(),
        palette,
    );
    // PR-ready crumbs pile up in storage, left of the ants.
    for i in 0..scene.pr_ready {
        let (row, col) = (i / 4, i % 4);
        let min = scene.storage.left_bottom()
            + vec2(
                2.0 * px + col as f32 * 10.0 * px + (row % 2) as f32 * 5.0 * px,
                -a - row as f32 * 8.0 * px,
            );
        sprites.item(
            painter,
            Rect::from_min_size(min, Vec2::splat(a)),
            Item::Pile,
            1.0,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_ants(
    painter: &egui::Painter,
    sprites: &Sprites,
    scene: &Scene,
    placed: &[Placed],
    now_s: f64,
    selection: &BTreeSet<NodeId>,
    hovered: Option<&NodeId>,
    ui: &Ui,
) {
    let px = scene.px;
    let walk_frame = (now_s * WALK_FPS) as usize;
    let step = (now_s * STEP_FPS) as usize;
    let accent = ui.visuals().selection.bg_fill;
    let palette = Palette::new(ui.visuals().dark_mode);
    let hover = ui.visuals().widgets.hovered.fg_stroke.color;
    for (ant, at) in scene.ants.iter().zip(placed) {
        let walking = at.heading.is_some();
        let mut cell = Rect::from_center_size(at.pos, Vec2::splat(ant.size));
        let tpx = ant.size / layout::CELL;
        let facing_right = match at.heading {
            Some(h) if h.x.abs() > 0.1 => h.x > 0.0,
            _ => ant.facing_right,
        };
        let frame = match ant.pose {
            _ if walking => walk_frame,
            Pose::Dig => step,
            _ => 0,
        };
        if !walking {
            match ant.pose {
                Pose::Wait => cell = cell.translate(vec2(0.0, -((step % 2) as f32) * 2.0 * tpx)),
                Pose::Dig => cell = cell.translate(vec2(0.0, -((step % 2) as f32) * tpx)),
                Pose::Sleep => cell = cell.translate(vec2(0.0, tpx)),
                _ => {}
            }
        }
        let look = AntLook {
            red: ant.pose == Pose::Failed,
            frame,
            facing_right,
            upside_down: ant.pose == Pose::Failed && !walking,
            alpha: ant.alpha,
        };
        if ant.pose == Pose::Queen {
            sprites.ant(painter, cell, look);
            let crown = Rect::from_center_size(
                cell.center_top()
                    + vec2(
                        if facing_right { 0.25 } else { -0.25 } * ant.size,
                        ant.size * 0.3,
                    ),
                Vec2::splat(ant.size * 0.5),
            );
            sprites.item(painter, crown, Item::Crown, 1.0);
            continue;
        }
        sprites.ant(painter, cell, look);
        if ant.pose == Pose::Carry || (walking && ant.state == crate::model::NodeState::Done) {
            let crumb = Rect::from_center_size(
                cell.center()
                    + vec2(
                        if facing_right { 0.35 } else { -0.35 } * ant.size,
                        -0.1 * ant.size,
                    ),
                Vec2::splat(ant.size * 0.5),
            );
            sprites.item(painter, crumb, Item::Crumb, ant.alpha);
        }
        if !walking && ant.pose == Pose::Dig {
            sprites.dirt(painter, cell, facing_right, tpx, step);
        }
        if !walking && ant.mark == Some(layout::Mark::Whip) {
            sprites.whip(painter, cell, tpx, step);
        }
        if let Some(mark) = ant.mark
            && !walking
        {
            let size = layout::CELL * (tpx - 1.0).max(1.0);
            let anchor = cell.center() + vec2(0.25 * ant.size, -tpx);
            sprites.bubble(painter, anchor, size, mark, ant.alpha);
        }
        if let Some(name) = &ant.tag {
            let selected = selection.contains(&ant.id);
            let width = 2.0 * scene.cell() - 2.0 * px;
            tag(
                painter,
                cell.center_bottom(),
                name,
                px,
                width,
                selected.then_some(accent),
                palette,
            );
        }
        let ring = cell.expand(px);
        if selection.contains(&ant.id) {
            painter.rect_stroke(ring, 0.0, Stroke::new(px, accent), StrokeKind::Outside);
        } else if hovered == Some(&ant.id) {
            painter.rect_stroke(ring, 0.0, Stroke::new(1.0, hover), StrokeKind::Outside);
        }
    }
}

/// Text with a hard pixel shadow, cut to `width`; returns where it went.
fn label(
    painter: &egui::Painter,
    pos: Pos2,
    align: Align2,
    text: &str,
    font: FontId,
    width: f32,
    palette: Palette,
) -> Rect {
    let galley = |color| {
        let mut job =
            egui::text::LayoutJob::simple_singleline(text.to_string(), font.clone(), color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(8.0));
        painter.layout_job(job)
    };
    let shadow = galley(palette.label_shadow);
    let rect = align.anchor_size(pos, shadow.size());
    painter.galley(rect.min + vec2(1.0, 1.0), shadow, palette.label_shadow);
    painter.galley(rect.min, galley(palette.label), palette.label);
    rect
}

/// An ant's name tag under `top`: light text on a dark chip, or on `highlight` when the ant is
/// selected.
fn tag(
    painter: &egui::Painter,
    top: Pos2,
    name: &str,
    px: f32,
    width: f32,
    highlight: Option<egui::Color32>,
    palette: Palette,
) {
    let font = FontId::proportional(8.0 + 1.5 * px);
    let mut job = egui::text::LayoutJob::simple_singleline(name.to_string(), font, palette.label);
    job.wrap = egui::text::TextWrapping::truncate_at_width((width - 2.0 * px).max(8.0));
    let galley = painter.layout_job(job);
    let chip = Align2::CENTER_TOP.anchor_size(top, galley.size() + vec2(2.0 * px, 0.0));
    let fill = highlight.unwrap_or(palette.label_shadow.gamma_multiply(0.75));
    painter.rect_filled(chip, 0.0, fill);
    painter.galley(chip.min + vec2(px, 0.0), galley, palette.label);
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

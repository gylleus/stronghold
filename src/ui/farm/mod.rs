//! The farm: a side view of the clan where a dwarf's place shows its state. An
//! drawn from the clan snapshot, filters and selection.
//!
//! [`layout`] decides where everything goes, [`motion`] walks dwarves to new places and
//! [`sprites`] draws them; this module ties them to egui and handles input.

pub mod art;
mod beams;
mod comms;
pub mod death;
mod excavation;
pub(crate) mod hammer;
pub mod layout;
#[cfg(test)]
mod layout_tests;
mod ledger;
pub mod mail;
pub mod motion;
pub mod nature;
pub mod nav;
mod pops;
pub mod rooms;
#[cfg(test)]
mod rooms_tests;
pub mod rubble;
#[cfg(test)]
mod rubble_tests;
pub mod scenery;
pub mod sprites;
mod tool_throw;
mod world;
pub mod zoom;
#[cfg(test)]
mod zoom_tests;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;

use eframe::egui::{self, Align2, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, vec2};

use crate::model::{ClanSnapshot, NodeId};
use crate::ui::clan_view::ClanView;
use crate::ui::daylight;
use crate::ui::dwarf::{self, Action, Look, Role};
use crate::ui::farm::nav::{ScrollAction, ScrollSource, route_scroll};
use art::{ArtSettings, DwarfArt, Figure};
use death::{Body, Fall};
use layout::{FarmInput, Pose, Scene, Spot, TunnelKey, layout_in};
use motion::{Walk, route};
use sprites::{Coat, Palette, Sprites};
use zoom::Camera;

/// Redraw interval while a dwarf walks.
const WALK_FRAME: Duration = Duration::from_millis(33);
/// Redraw interval for mining chips, fire, snores, scrying and the pebble bonk:
/// stepped like pixel art, and slow enough that a farm full of working dwarves stays cheap.
const STEP_FRAME: Duration = Duration::from_millis(150);
/// Walk frames per second.
/// Dig and wave steps per second; matches [`STEP_FRAME`].
const STEP_FPS: f64 = 4.0;
/// The busy King's hop, in texels per step.
const KING_HOP: [f32; 4] = [0.0, 2.0, 4.0, 2.0];
/// How long a King's call takes to ripple across the farm.
const RIPPLE_S: f64 = 1.8;
/// Delay of a ripple's second ring.
const ECHO_S: f64 = 0.3;
/// How long the King glows after an inward call's last ring reaches him.
const ARRIVAL_S: f64 = 0.5;
/// How long the King glows after a dwarf emerges from the throne hall.
const BIRTH_GLOW_S: f64 = 1.2;
/// A gap between frames longer than this means the farm was not on screen: dwarves that
/// appeared meanwhile are placed, not born.
const AWAY_S: f64 = 2.5;
/// How long rooms and their dwarves take to glide to where a rearranged or grown plan put them.
pub const GLIDE_S: f64 = 0.8;

/// A call of the King that ripples across the farm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pulse {
    /// A broadcast to the coordinators: a golden ring.
    Broadcast,
    /// The user asked for a status report: a cyan ring gathering in to the King.
    StatusReport,
}

impl Pulse {
    /// Whether the call's rings close in on the King instead of spreading from him.
    fn inward(self) -> bool {
        self == Pulse::StatusReport
    }

    fn color(self) -> egui::Color32 {
        match self {
            Pulse::Broadcast => egui::Color32::from_rgb(0xff, 0xd5, 0x4a),
            Pulse::StatusReport => world::LIGHT,
        }
    }
}

/// A call rippling across the farm, started at `start_s`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Ripple {
    pulse: Pulse,
    start_s: f64,
}

/// The radius a ripple of `pulse` has reached `elapsed_s` after it started, with an
/// ease-out: growing from the King to `max_r`, or for an inward call shrinking from `max_r`
/// to him; `None` once it has passed.
pub fn ripple_radius(pulse: Pulse, elapsed_s: f64, max_r: f32) -> Option<f32> {
    if !(0.0..RIPPLE_S).contains(&elapsed_s) {
        return None;
    }
    let left = 1.0 - (elapsed_s / RIPPLE_S) as f32;
    let out = 1.0 - left * left;
    Some(max_r * if pulse.inward() { 1.0 - out } else { out })
}

/// How brightly the King glows `elapsed_s` into an inward call: rising as the first ring
/// closes in, peaking when it reaches him and fading over [`ECHO_S`] + [`ARRIVAL_S`].
fn arrival_glow(elapsed_s: f64) -> f32 {
    let since = elapsed_s - RIPPLE_S;
    let glow = if since < 0.0 {
        1.0 + since / ECHO_S
    } else {
        1.0 - since / (ECHO_S + ARRIVAL_S)
    };
    glow.clamp(0.0, 1.0) as f32
}

/// How far a dwarf `dist` from the King hops while a ripple of `radius` passes it: a jump of
/// up to `height` when the ring is on it, tapering within `width`.
pub fn hop(dist: f32, radius: f32, width: f32, height: f32) -> f32 {
    let off = (dist - radius).abs();
    if off >= width {
        return 0.0;
    }
    height * (1.0 - off / width)
}

/// What happened in the farm this frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FarmResponse {
    /// The dwarf clicked this frame.
    pub clicked: Option<NodeId>,
    /// Empty ground was clicked (clears the selection, as in the clan).
    pub clicked_empty: bool,
    pub open_envoy: bool,
    /// Open the read-only Ledger pane.
    pub open_ledger: bool,
    /// A thrown tool hit this frame.
    pub bonk: bool,
    pub hovered: Option<NodeId>,
    /// Dwarves inside a selection box dragged from empty ground, when it was released this
    /// frame, and whether Shift/Cmd was held (add to the selection rather than replace it).
    pub boxed: Option<(Vec<NodeId>, bool)>,
    /// Nodes to close or whip, picked in a dwarf's context menu.
    pub close_requested: Vec<NodeId>,
    pub whip_requested: Vec<NodeId>,
    /// Nodes to park or wake, picked in a dwarf's context menu.
    pub park_requested: Vec<NodeId>,
    pub wake_requested: Vec<NodeId>,
}

/// A dwarf's place and its walk there, if it is still walking.
#[derive(Debug, Clone)]
struct Track {
    spot: Spot,
    /// Where the spot was last frame, relative to the farm's rect (see [`motion::to_unit`]).
    at: Pos2,
    walk: Option<Walk>,
    /// How the dwarf was last drawn, for its fall should it leave the clan; `None` for the
    /// King and subagents, which never fall.
    body: Option<Body>,
}

/// Where a dwarf is drawn this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
    pub pos: Pos2,
    /// Direction of travel while walking.
    pub heading: Option<Vec2>,
}

#[derive(Default)]
pub struct FarmView {
    sprites: Option<Sprites>,
    tracks: HashMap<NodeId, Track>,
    /// Dwarf whose context menu is open (the one right-clicked last).
    context_node: Option<NodeId>,
    /// Where a selection box being dragged started, in screen points.
    box_origin: Option<Pos2>,
    /// Every dwarf drawn so far: a new one walks out of the throne hall.
    seen: HashSet<NodeId>,
    /// Time of the last frame drawn; see [`AWAY_S`].
    last_frame_s: Option<f64>,
    /// This frame follows a gap: the farm was not on screen.
    away: bool,
    /// When dwarves last emerged from the King, for his glow.
    births: Vec<f64>,
    /// Calls asked for since the last frame; see [`FarmView::pulse`].
    pending: Vec<Pulse>,
    ripples: Vec<Ripple>,
    /// Dwarves that left the clan, falling to the bottom.
    falls: Vec<Fall>,
    throws: tool_throw::Throws,
    /// The King is working on something (his chat turn runs): he hops about.
    king_busy: bool,
    /// His chat is the conversation shown: he gets the selection ring.
    king_focused: bool,
    envoy_health: Option<crate::envoy::EnvoyHealth>,
    envoy_pending: usize,
    envoy_receipt: crate::ui::envoy_receipt::ReceiptCue,
    /// What the Ledger holds: its count on the chamber's sign, the newest entries on hover.
    ledger: crate::ledger::Shelf,
    /// Messages carried between dwarves; see [`mail`].
    mail: crate::ui::envelope::Flights,
    /// Tool calls beaming from their callers; see [`beams`].
    beams: crate::ui::beam::Beams,
    /// Dwarves that just changed state; see [`pops`].
    pops: crate::ui::state_pop::Pops,
    /// The part of the farm on screen; see [`zoom`].
    camera: Camera,
    /// Device of the latest scroll over the farm; see [`ScrollSource`].
    scroll_source: ScrollSource,
    /// Where the rooms are dug, kept across frames and launches; see [`rooms`].
    rooms: rooms::RoomPlan,
    /// Each room's shape last frame, and the rooms gliding to a new one.
    room_shapes: HashMap<TunnelKey, RoomShape>,
    room_glides: HashMap<TunnelKey, RoomGlide>,
    /// The scale and the rooms of the surface band last frame, and their glide to new ones.
    frame_shape: Option<FrameShape>,
    frame_glide: Option<FrameGlide>,
    /// The rect of the last frame drawn and of the last dwarves placed: a move in an unchanged
    /// rect glides, a resize does not.
    shown_rect: Option<Rect>,
    advanced_rect: Option<Rect>,
    /// Reduce motion as set for tests; the system's setting otherwise.
    reduce_motion: Option<bool>,
    /// A heap per open worktree on the surface, and the hammer; see [`rubble`].
    rubble: rubble::RubbleView,
    /// Dwarf pixel resolution and palette; see [`art`].
    art: ArtSettings,
    dwarf_art: DwarfArt,
}

/// A room's chamber and label relative to the farm's rect (see [`motion::to_unit`]).
#[derive(Debug, Clone, Copy, PartialEq)]
struct RoomShape {
    chamber: Rect,
    label_top: Pos2,
}

impl RoomShape {
    fn of(tunnel: &layout::Tunnel, rect: Rect) -> Self {
        let unit = |r: Rect| {
            Rect::from_min_max(motion::to_unit(r.min, rect), motion::to_unit(r.max, rect))
        };
        Self {
            chamber: unit(tunnel.chamber),
            label_top: motion::to_unit(tunnel.label_top, rect),
        }
    }

    fn lerp(self, to: Self, t: f32) -> Self {
        let rect =
            |a: Rect, b: Rect| Rect::from_min_max(a.min.lerp(b.min, t), a.max.lerp(b.max, t));
        Self {
            chamber: rect(self.chamber, to.chamber),
            label_top: self.label_top.lerp(to.label_top, t),
        }
    }

    fn screen(self, rect: Rect) -> (Rect, Pos2) {
        let screen = |r: Rect| {
            Rect::from_min_max(
                motion::from_unit(r.min, rect),
                motion::from_unit(r.max, rect),
            )
        };
        (
            screen(self.chamber),
            motion::from_unit(self.label_top, rect),
        )
    }
}

/// A room gliding from `from` to its place since `start_s`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct RoomGlide {
    from: RoomShape,
    start_s: f64,
}

impl RoomGlide {
    fn at(&self, to: RoomShape, now_s: f64) -> RoomShape {
        let t = ((now_s - self.start_s) / GLIDE_S) as f32;
        self.from.lerp(to, motion::ease(t))
    }

    fn done(&self, now_s: f64) -> bool {
        now_s >= self.start_s + GLIDE_S
    }
}

/// The scale and what the surface band holds: they change when the plan is laid out afresh.
#[derive(Debug, Clone, Copy, PartialEq)]
struct FrameShape {
    px: f32,
    surface_y: f32,
    gallery: Rect,
    landing: Rect,
    throne_hall: Rect,
    storage: Rect,
    comms: Option<layout::CommsRoom>,
    ledger: Option<Rect>,
}

impl FrameShape {
    fn of(scene: &Scene) -> Self {
        Self {
            px: scene.px,
            surface_y: scene.surface_y,
            gallery: scene.gallery,
            landing: scene.landing,
            throne_hall: scene.throne_hall,
            storage: scene.storage,
            comms: scene.comms,
            ledger: scene.ledger,
        }
    }

    fn lerp(self, to: Self, t: f32) -> Self {
        let rect =
            |a: Rect, b: Rect| Rect::from_min_max(a.min.lerp(b.min, t), a.max.lerp(b.max, t));
        let num = |a: f32, b: f32| a + (b - a) * t;
        Self {
            px: num(self.px, to.px),
            surface_y: num(self.surface_y, to.surface_y),
            gallery: rect(self.gallery, to.gallery),
            landing: rect(self.landing, to.landing),
            throne_hall: rect(self.throne_hall, to.throne_hall),
            storage: rect(self.storage, to.storage),
            comms: match (self.comms, to.comms) {
                (Some(a), Some(b)) => Some(layout::CommsRoom {
                    chamber: rect(a.chamber, b.chamber),
                    shaft: rect(a.shaft, b.shaft),
                    receiver: a.receiver.lerp(b.receiver, t),
                    px: num(a.px, b.px),
                }),
                (_, to) => to,
            },
            ledger: match (self.ledger, to.ledger) {
                (Some(a), Some(b)) => Some(rect(a, b)),
                (_, to) => to,
            },
        }
    }

    /// Draws `scene` with this scale and band, its dwarves resized to the scale.
    fn apply(self, scene: &mut Scene) {
        let ratio = self.px / scene.px;
        for dwarf in &mut scene.dwarves {
            dwarf.size *= ratio;
        }
        scene.px = self.px;
        scene.surface_y = self.surface_y;
        scene.gallery = self.gallery;
        scene.landing = self.landing;
        scene.throne_hall = self.throne_hall;
        scene.storage = self.storage;
        scene.comms = self.comms;
        scene.ledger = self.ledger;
    }
}

/// The scale and band gliding from `from` to the scene's since `start_s`: the camera moving
/// in or out as the farm is laid out afresh.
#[derive(Debug, Clone, Copy, PartialEq)]
struct FrameGlide {
    from: FrameShape,
    start_s: f64,
}

impl FrameGlide {
    fn at(&self, to: FrameShape, now_s: f64) -> FrameShape {
        let t = ((now_s - self.start_s) / GLIDE_S) as f32;
        self.from.lerp(to, motion::ease(t))
    }
}

impl FarmView {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ripples a call of the King across the farm, from the next frame it is drawn. Calls
    /// made while the farm is not shown are dropped then.
    /// How the farm's figures are drawn (the Settings window's Art section).
    pub fn set_art(&mut self, art: &ArtSettings) {
        if self.art != *art {
            self.art = art.clone();
        }
    }

    /// Whether the King is busy (thinking on a chat turn). Otherwise he sits still.
    pub fn set_king_busy(&mut self, busy: bool) {
        self.king_busy = busy;
    }

    /// Whether the King's chat is the conversation shown; he is ringed then.
    pub fn set_king_focused(&mut self, focused: bool) {
        self.king_focused = focused;
    }

    pub fn set_envoy_health(&mut self, health: crate::envoy::EnvoyHealth) {
        self.envoy_health = Some(health);
    }

    pub fn set_envoy_pending(&mut self, pending: usize) {
        self.envoy_pending = pending;
    }

    /// The open worktrees' heaps and the cleanup breaking them down, if one runs.
    pub fn set_rubble(&mut self, sites: Vec<rubble::Site>, hammer: Option<rubble::CleanupJob>) {
        self.rubble.set_stale(sites, hammer);
    }

    pub fn set_envoy_receipt(&mut self, receipt: crate::ui::envoy_receipt::ReceiptCue) {
        self.envoy_receipt = receipt;
    }

    /// What the Ledger holds, for its chamber.
    pub fn set_ledger(&mut self, shelf: crate::ledger::Shelf) {
        self.ledger = shelf;
    }

    /// The room plan, to save between launches.
    pub fn rooms(&self) -> &rooms::RoomPlan {
        &self.rooms
    }

    /// Restores a room plan saved by an earlier launch.
    pub fn set_rooms(&mut self, plan: rooms::RoomPlan) {
        self.rooms = plan;
    }

    /// Overrides the system's Reduce motion setting (for tests).
    pub fn set_reduce_motion(&mut self, still: bool) {
        self.reduce_motion = Some(still);
    }

    fn still(&self) -> bool {
        self.reduce_motion.unwrap_or_else(death::reduce_motion)
    }

    pub fn pulse(&mut self, pulse: Pulse) {
        self.pending.push(pulse);
    }

    /// Shows the whole farm again.
    pub fn fit_to_screen(&mut self) {
        self.camera = Camera::default();
    }

    fn focus(
        &mut self,
        scene: &Scene,
        placed: &[Placed],
        clan: &ClanView,
        focus: crate::ui::clan_view::Focus,
    ) {
        use crate::ui::clan_view::Focus;
        let mut bounds = Rect::NOTHING;
        for room in &scene.tunnels {
            let matches = match (&focus, &room.key) {
                (Focus::Run(run), TunnelKey::Run(key)) => run == key,
                (Focus::Repo(repo), TunnelKey::Repo(key)) => repo == key,
                (Focus::Repo(repo), TunnelKey::Run(run)) => {
                    clan.run_repo(run) == Some(repo.as_str())
                }
                (Focus::Repo(repo), TunnelKey::RestingArea) => repo == crate::park::RESTING_AREA,
                _ => false,
            };
            if matches {
                bounds = bounds.union(room.chamber.expand(scene.cell() * 0.5));
            }
        }
        // Runs with everyone above ground may have no chamber yet. Focus their figures.
        if !bounds.is_finite() {
            for (dwarf, at) in scene.dwarves.iter().zip(placed) {
                let matches = match &focus {
                    Focus::Nodes(ids) => ids.contains(&dwarf.id),
                    Focus::Run(run) => clan.node_run(&dwarf.id) == Some(run.as_str()),
                    Focus::Repo(repo) => clan.node_repo(&dwarf.id) == Some(repo.as_str()),
                };
                if matches {
                    bounds = bounds.union(Rect::from_center_size(
                        at.pos,
                        Vec2::splat(dwarf.size * 2.0),
                    ));
                }
            }
        }
        if bounds.is_finite() && bounds.is_positive() {
            match focus {
                Focus::Nodes(_) => self.camera.pan_to(scene.rect, bounds.center()),
                Focus::Run(_) | Focus::Repo(_) => self.camera.focus(scene.rect, bounds),
            }
        }
    }

    /// Carries a message between dwarves, asked for at `now_s` (egui time); `still` for Reduce
    /// motion.
    pub fn message(&mut self, flight: crate::traffic::Flight, now_s: f64, still: bool) {
        self.mail.push(flight, now_s, still);
    }

    /// Shows a tool call's beam, asked for at `now_s` (egui time); `still` for Reduce motion.
    pub fn beam(&mut self, beam: crate::traffic::beam::Beam, now_s: f64, still: bool) {
        self.beams.push(beam, now_s, still);
    }

    /// Pops a dwarf that just changed state, asked for at `now_s` (egui time); `still` for
    /// Reduce motion.
    pub fn pop(&mut self, node: NodeId, cue: crate::ui::state_pop::Cue, now_s: f64, still: bool) {
        self.pops.push(node, cue, now_s, still);
    }

    /// Marks only visible targets of an accepted King close. Their bodies are held when
    /// the snapshot removes them; no change to process shutdown or the snapshot itself.
    pub fn prepare_king_close(&mut self, ids: impl Iterator<Item = NodeId>, now_s: f64) {
        if self.last_frame_s.is_none_or(|last| now_s - last > AWAY_S) {
            return;
        }
        for id in ids {
            if let Some(body) = self.tracks.get(&id).and_then(|t| t.body) {
                self.throws.prepare(id, body, now_s);
            }
        }
    }

    /// Draws the farm filling the available space and handles clicks, hover and the context
    /// menu. `clan` supplies the shared selection, filters, badges, tooltips and actions.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        snapshot: &ClanSnapshot,
        clan: &mut ClanView,
    ) -> FarmResponse {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        self.navigate(ui, &response);
        let now_s = ui.input(|i| i.time);
        let input = FarmInput {
            snapshot,
            hidden: clan.hidden(),
            pr_ready: clan.pr_ready(),
            building: clan.building(),
            now_ms: now_ms(),
        };
        let mut scene = layout_in(&input, rect, &mut self.rooms);
        let away = self.last_frame_s.is_none_or(|t| now_s - t > AWAY_S);
        self.last_frame_s = Some(now_s);
        self.away = away;
        self.glide_rooms(&mut scene, now_s);
        if away {
            self.pending.clear();
        }
        self.ripples
            .extend(self.pending.drain(..).map(|pulse| Ripple {
                pulse,
                start_s: now_s,
            }));
        self.ripples
            .retain(|r| now_s - r.start_s < RIPPLE_S + ECHO_S + ARRIVAL_S);
        self.births.retain(|t| now_s - t < BIRTH_GLOW_S);
        self.falls.retain(|f| now_s - f.start_s < f.duration());
        if away {
            self.throws.clear();
        }
        let from = scene
            .dwarf(&NodeId::king())
            .map(|d| motion::to_unit(d.pos, scene.rect));
        let bonk = self.throws.tick(
            now_s,
            from,
            self.still(),
            |id| snapshot.node(id).is_some(),
            |id| self.tracks.get(id).and_then(|t| t.body),
        );
        self.bury(&scene, snapshot, now_s, death::reduce_motion);
        let placed = self.advance(&scene, now_s);
        let still = self.still();
        let heaps = self.rubble.layout(&scene, now_s, away, still);
        if let Some(focus) = clan.take_focus() {
            self.focus(&scene, &placed, clan, focus);
        }
        let pops = self.pops.frames(now_s);
        let effects = Effects {
            ripples: &self.ripples,
            births: &self.births,
            king_busy: self.king_busy,
            king_focused: self.king_focused,
            envoy_receipt: self.envoy_receipt,
            pops: &pops,
        };
        let sprites = self.sprites.get_or_insert_with(|| Sprites::load(ui.ctx()));
        sprites.begin_frame(&self.art, scene.px, now_s);
        let painter = ui.painter_at(rect);
        let first_shape = ui
            .ctx()
            .graphics(|g| g.get(painter.layer_id()).map(|list| list.next_idx()))
            .unwrap_or(egui::layers::ShapeIdx(0));
        let daylight = daylight::daylight();
        let palette = Palette::new(daylight);
        let selection = clan.selection();

        let pointer = response.hover_pos().map(|p| self.camera.to_farm(rect, p));
        let tray_hovered = pointer.is_some_and(|p| tray_hit(&scene, p));
        let hovered = pointer
            .filter(|_| !tray_hovered)
            .and_then(|p| hit(&scene, &placed, p));
        let heap_hovered = pointer
            .filter(|_| !tray_hovered && hovered.is_none())
            .and_then(|p| rubble::hit(&heaps, p))
            .cloned();
        let ledger_hovered = pointer
            .filter(|_| !tray_hovered && hovered.is_none() && heap_hovered.is_none())
            .is_some_and(|p| scene.ledger.is_some_and(|room| room.contains(p)));
        let portal_frame = world::portal_frame(
            now_s,
            mail::portal_flare(self.mail.tick(now_s), now_s),
            still,
        );
        paint_ground(
            &painter,
            sprites,
            &scene,
            palette,
            now_s,
            daylight,
            portal_frame,
            &self.rubble,
            &heaps,
            self.ledger.count,
        );

        self.envoy_receipt.repaint(ui.ctx());
        comms::paint(
            &painter,
            sprites,
            &scene,
            self.envoy_health
                .unwrap_or(crate::envoy::EnvoyHealth::Stopped),
            now_s,
            self.envoy_receipt,
            still,
        );
        paint_calls(&painter, sprites, &scene, &effects, now_s, true);
        pops::paint(&painter, sprites, &scene, &placed, &pops, true);
        self.dwarf_art.begin_frame();
        let mut figures = Figures {
            art: &self.art,
            dwarves: &mut self.dwarf_art,
        };
        paint_dwarves(
            &painter,
            sprites,
            &mut figures,
            &scene,
            &placed,
            now_s,
            &selection,
            hovered.as_ref(),
            &effects,
            ui,
        );
        for (dwarf, at) in scene.dwarves.iter().zip(&placed) {
            if let Some(badge) = clan.unread(&dwarf.id) {
                let center = at.pos + vec2(dwarf.size * 0.45, -dwarf.size * 0.45);
                let radius = (dwarf.size * 0.2).max(5.0);
                painter.circle_filled(
                    center,
                    radius,
                    crate::ui::notifications::severity_color(
                        badge.severity,
                        ui.visuals().dark_mode,
                    ),
                );
                painter.text(
                    center,
                    egui::Align2::CENTER_CENTER,
                    if badge.count > 9 {
                        "9+".into()
                    } else {
                        badge.count.to_string()
                    },
                    egui::FontId::proportional(radius * 1.3),
                    egui::Color32::WHITE,
                );
            }
        }
        pops::paint(&painter, sprites, &scene, &placed, &pops, false);
        paint_falls(&painter, &mut figures, &scene, &self.falls, now_s);
        mail::paint(
            &painter,
            sprites,
            &scene,
            &placed,
            self.mail.tick(now_s),
            now_s,
        );
        self.rubble.paint_overlay(
            &painter,
            sprites,
            &scene,
            &heaps,
            heap_hovered.as_ref().map(|t| t.site.id.as_str()),
            palette,
            now_s,
        );
        beams::paint(&painter, &scene, &placed, self.beams.tick(now_s), now_s);
        self.throws.paint(&painter, &mut figures, &scene, now_s);
        paint_calls(&painter, sprites, &scene, &effects, now_s, false);
        comms::tray(
            &painter,
            sprites,
            &scene,
            self.envoy_pending,
            tray_hovered,
            self.envoy_receipt.offset,
        );
        sprites
            .world
            .borrow_mut()
            .map_scene(ui.ctx(), painter.layer_id(), first_shape);
        zoom::apply(
            ui.ctx(),
            painter.layer_id(),
            first_shape,
            self.camera.transform(rect),
            painter.clip_rect(),
        );
        if response.double_clicked() && hovered.is_none() && !tray_hovered && !ledger_hovered {
            self.camera = Camera::default();
        }

        // Smooth travel and calls; stepped portal veil, fire, work, snores and pebble bonks.
        let walking = placed.iter().any(|p| p.heading.is_some())
            || !self.ripples.is_empty()
            || !self.births.is_empty()
            || !self.falls.is_empty()
            || self.throws.animating()
            || !self.mail.is_empty()
            || !self.beams.is_empty()
            || !self.pops.is_empty()
            || !self.room_glides.is_empty()
            || self.frame_glide.is_some()
            || (self.rubble.animating() && !still);
        let stepping = scene.dwarves.iter().any(|a| {
            matches!(a.pose, Pose::Dig | Pose::Build | Pose::Wait | Pose::Sleep)
                || (a.pose == Pose::King && self.king_busy)
                || a.mark == Some(layout::Mark::Stalled)
        });
        if walking {
            ui.ctx().request_repaint_after(WALK_FRAME);
        } else if !still || stepping || scene.comms.is_some() || scene.gallery.is_positive() {
            ui.ctx().request_repaint_after(STEP_FRAME);
        }
        daylight::schedule_repaint(ui.ctx());

        if tray_hovered {
            response
                .clone()
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text_at_pointer(format!(
                    "Envoy inbox: {} waiting · outbox",
                    self.envoy_pending
                ));
        }
        if let Some(id) = &hovered {
            response
                .clone()
                .on_hover_ui_at_pointer(|ui| clan.node_tooltip_ui(ui, id));
        }
        if ledger_hovered {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as i64);
            response
                .clone()
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text_at_pointer(crate::ledger::describe(&self.ledger, now_ms));
        }
        if let Some(heap) = &heap_hovered {
            response
                .clone()
                .on_hover_text_at_pointer(rubble::RubbleView::tooltip(heap));
            if response.clicked() {
                self.rubble.click(heap);
            }
        }
        if response.secondary_clicked() {
            self.context_node = hovered.clone();
        }
        let boxed = self.box_select(ui, &response, &scene, &placed);
        let menu = match &self.context_node {
            Some(id) => clan.node_menu(&response, id),
            None => Default::default(),
        };
        FarmResponse {
            bonk,
            clicked: response.clicked().then(|| hovered.clone()).flatten(),
            clicked_empty: response.clicked()
                && hovered.is_none()
                && !tray_hovered
                && !ledger_hovered
                && heap_hovered.is_none(),
            open_envoy: response.clicked() && tray_hovered,
            open_ledger: response.clicked() && ledger_hovered,
            hovered,
            close_requested: menu.close,
            whip_requested: menu.whip,
            park_requested: menu.park,
            wake_requested: menu.wake,
            boxed,
        }
    }

    /// Starts, draws and finishes a selection box dragged with the primary button from empty
    /// ground, as in the clan. Returns the dwarves it covers once released.
    fn box_select(
        &mut self,
        ui: &Ui,
        response: &egui::Response,
        scene: &Scene,
        placed: &[Placed],
    ) -> Option<(Vec<NodeId>, bool)> {
        let pointer = ui.input(|i| i.pointer.latest_pos());
        if response.drag_started_by(egui::PointerButton::Primary) {
            let origin = ui.input(|i| i.pointer.press_origin()).or(pointer);
            self.box_origin = origin.filter(|o| {
                let p = self.camera.to_farm(scene.rect, *o);
                !tray_hit(scene, p) && hit(scene, placed, p).is_none()
            });
        }
        let origin = self.box_origin?;
        let current = pointer.unwrap_or(origin);
        let rect = Rect::from_two_pos(origin, current);
        let released = response.drag_stopped_by(egui::PointerButton::Primary)
            || !ui.input(|i| i.pointer.primary_down());
        if released {
            self.box_origin = None;
            let additive = ui.input(|i| crate::ui::clan_view::is_additive(i.modifiers));
            let farm = Rect::from_two_pos(
                self.camera.to_farm(scene.rect, rect.min),
                self.camera.to_farm(scene.rect, rect.max),
            );
            return Some((boxed_dwarves(scene, placed, farm), additive));
        }
        let stroke = ui.visuals().selection.stroke;
        let fill = ui.visuals().selection.bg_fill.gamma_multiply(0.2);
        ui.painter_at(response.rect)
            .rect(rect, 2.0, fill, stroke, StrokeKind::Inside);
        ui.ctx().request_repaint();
        None
    }

    /// Zooms and pans on the wheel, pinch, trackpad swipes, right- or middle-drags and arrow
    /// keys, per [`zoom`].
    fn navigate(&mut self, ui: &Ui, response: &egui::Response) {
        let rect = response.rect;
        let (source, scroll, zoom_delta, ctrl) = ui.input(|i| {
            (
                ScrollSource::from_events(&i.events),
                i.smooth_scroll_delta(),
                i.zoom_delta(),
                i.modifiers.ctrl,
            )
        });
        if let Some(source) = source {
            self.scroll_source = source;
        }
        if let Some(pointer) = response.hover_pos() {
            if zoom_delta != 1.0 {
                self.camera.zoom_at(rect, pointer, zoom_delta);
            } else {
                match route_scroll(self.scroll_source, scroll, ctrl) {
                    Some(ScrollAction::Zoom(factor)) => self.camera.zoom_at(rect, pointer, factor),
                    Some(ScrollAction::Pan(delta)) => self.camera.pan(rect, delta),
                    None => {}
                }
            }
        }
        if response.dragged_by(egui::PointerButton::Secondary)
            || response.dragged_by(egui::PointerButton::Middle)
        {
            self.camera.pan(rect, response.drag_delta());
        }
        if !self.camera.is_fit()
            && let Some(delta) = nav::arrow_pan(ui.ctx())
        {
            self.camera.pan(rect, delta);
            ui.ctx().request_repaint();
        }
    }

    /// Glides rooms whose place changed while the view kept its size (a rearrange, or a room
    /// dug deeper) from where they were drawn, and the scale and surface band with them, and
    /// draws `scene` with them where they are on the way. A resize, a return to the farm or
    /// Reduce motion moves them at once.
    pub fn glide_rooms(&mut self, scene: &mut Scene, now_s: f64) {
        let rect = scene.rect;
        let steady = !self.away && self.shown_rect == Some(rect);
        self.shown_rect = Some(rect);
        if !steady {
            self.room_glides.clear();
            self.frame_glide = None;
        }
        let mut still = None;
        let target = FrameShape::of(scene);
        if let Some(last) = self.frame_shape
            && steady
            && last != target
        {
            self.frame_glide = (!*still.get_or_insert_with(|| self.still())).then(|| {
                let from = match self.frame_glide {
                    Some(glide) => glide.at(last, now_s),
                    None => last,
                };
                FrameGlide {
                    from,
                    start_s: now_s,
                }
            });
        }
        self.frame_shape = Some(target);
        if let Some(glide) = self.frame_glide {
            if now_s >= glide.start_s + GLIDE_S {
                self.frame_glide = None;
            } else {
                glide.at(target, now_s).apply(scene);
            }
        }
        let mut shapes = HashMap::new();
        for tunnel in &mut scene.tunnels {
            let target = RoomShape::of(tunnel, rect);
            if let Some(&last) = self.room_shapes.get(&tunnel.key)
                && steady
                && last != target
            {
                if *still.get_or_insert_with(|| self.still()) {
                    self.room_glides.remove(&tunnel.key);
                } else {
                    let from = match self.room_glides.get(&tunnel.key) {
                        Some(glide) => glide.at(last, now_s),
                        None => last,
                    };
                    self.room_glides.insert(
                        tunnel.key.clone(),
                        RoomGlide {
                            from,
                            start_s: now_s,
                        },
                    );
                }
            }
            shapes.insert(tunnel.key.clone(), target);
            let Some(glide) = self.room_glides.get(&tunnel.key) else {
                continue;
            };
            if glide.done(now_s) {
                self.room_glides.remove(&tunnel.key);
                continue;
            }
            let (chamber, label_top) = glide.at(target, now_s).screen(rect);
            let shift = chamber.right_top() - tunnel.chamber.right_top();
            for bed in &mut scene.beds {
                if tunnel.chamber.contains(bed.rect.center()) {
                    bed.rect = bed.rect.translate(shift);
                }
            }
            (tunnel.chamber, tunnel.label_top) = (chamber, label_top);
        }
        self.room_glides.retain(|key, _| shapes.contains_key(key));
        self.room_shapes = shapes;
    }

    /// Starts a fall for every dwarf drawn last frame that has left the clan: gone from
    /// `snapshot` too, not just filtered out, however it was removed. `reduce_motion` is asked
    /// only when one falls.
    pub fn bury(
        &mut self,
        scene: &Scene,
        snapshot: &ClanSnapshot,
        now_s: f64,
        reduce_motion: impl Fn() -> bool,
    ) {
        if self.away {
            return;
        }
        let mut still = None;
        for (id, track) in &self.tracks {
            if let Some(body) = track.body
                && !self.throws.contains(id)
                && scene.dwarf(id).is_none()
                && !snapshot.nodes.iter().any(|n| &n.id == id)
            {
                let still = *still.get_or_insert_with(&reduce_motion);
                self.falls.push(Fall {
                    body,
                    start_s: now_s,
                    still,
                });
            }
        }
    }

    /// Starts a walk for every dwarf whose spot changed, drops finished walks and gone dwarves,
    /// and returns where each of `scene.dwarves` is at `now_s`.
    ///
    /// A dwarf never drawn before walks out of the throne hall, unless the farm was just
    /// opened (everyone is placed then). A dwarf whose spot moved while the view kept its size,
    /// as when its room was dug afresh, glides there with it, or with Reduce motion is placed.
    pub fn advance(&mut self, scene: &Scene, now_s: f64) -> Vec<Placed> {
        let fresh = self.seen.is_empty() || self.away;
        let steady = !fresh && self.advanced_rect == Some(scene.rect);
        self.advanced_rect = Some(scene.rect);
        let mut still = None;
        self.tracks
            .retain(|id, _| scene.dwarves.iter().any(|a| &a.id == id));
        let mut current: HashMap<&NodeId, Placed> = HashMap::new();
        let mut placed = Vec::with_capacity(scene.dwarves.len());
        for dwarf in &scene.dwarves {
            // Subagents keep their place behind their parent wherever it walks.
            if let Spot::Trail(parent, _) = &dwarf.spot
                && let (Some(p), Some(parent_dwarf)) = (current.get(parent), scene.dwarf(parent))
            {
                let at = Placed {
                    pos: p.pos + (dwarf.pos - parent_dwarf.pos),
                    heading: p.heading,
                };
                self.tracks.insert(
                    dwarf.id.clone(),
                    Track {
                        spot: dwarf.spot.clone(),
                        at: motion::to_unit(dwarf.pos, scene.rect),
                        walk: None,
                        body: None,
                    },
                );
                self.seen.insert(dwarf.id.clone());
                current.insert(&dwarf.id, at);
                placed.push(at);
                continue;
            }
            let born = !fresh && dwarf.spot != Spot::King && self.seen.insert(dwarf.id.clone());
            self.seen.insert(dwarf.id.clone());
            let track = self
                .tracks
                .entry(dwarf.id.clone())
                .or_insert_with(|| Track {
                    spot: dwarf.spot.clone(),
                    at: motion::to_unit(dwarf.pos, scene.rect),
                    walk: None,
                    body: None,
                });
            if born {
                let from = scene.throne_hall.center() + vec2(0.0, 0.3 * scene.cell());
                let path = route(from, dwarf.pos, &Spot::King, &dwarf.spot, scene);
                track.walk = Some(Walk::new(&path, scene.rect, now_s));
                self.births.push(now_s);
            }
            if track.spot != dwarf.spot {
                let from = match &track.walk {
                    Some(walk) => walk.sample(scene.rect, now_s).0,
                    None => motion::from_unit(track.at, scene.rect),
                };
                let path = route(from, dwarf.pos, &track.spot, &dwarf.spot, scene);
                track.walk = Some(Walk::new(&path, scene.rect, now_s));
                track.spot = dwarf.spot.clone();
            } else if steady && track.walk.is_none() {
                let from = motion::from_unit(track.at, scene.rect);
                if (from - dwarf.pos).length() > 0.5
                    && !*still.get_or_insert_with(|| {
                        self.reduce_motion.unwrap_or_else(death::reduce_motion)
                    })
                {
                    track.walk = Some(Walk::glide(from, dwarf.pos, scene.rect, now_s, GLIDE_S));
                }
            }
            track.at = motion::to_unit(dwarf.pos, scene.rect);
            if let Some(walk) = &mut track.walk {
                walk.retarget(dwarf.pos, scene.rect);
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
                    pos: dwarf.pos,
                    heading: None,
                },
            };
            let falls = dwarf.pose != Pose::King && dwarf.kind != crate::model::NodeKind::Subagent;
            track.body = falls.then(|| Body {
                kind: dwarf.kind,
                at: motion::to_unit(at.pos, scene.rect),
                size: dwarf.size,
                facing_right: match at.heading {
                    Some(h) if h.x.abs() > 0.1 => h.x > 0.0,
                    _ => dwarf.facing_right,
                },
                coat: Coat::new(dwarf.agent.as_ref()),
                alpha: dwarf.alpha,
                seed: dwarf::seed_of(dwarf.id.as_str()),
            });
            current.insert(&dwarf.id, at);
            placed.push(at);
        }
        placed
    }
}

/// The dwarves whose cell overlaps `rect`, in draw order.
pub fn boxed_dwarves(scene: &Scene, placed: &[Placed], rect: Rect) -> Vec<NodeId> {
    scene
        .dwarves
        .iter()
        .zip(placed)
        .filter(|(dwarf, at)| {
            Rect::from_center_size(at.pos, Vec2::splat(dwarf.size)).intersects(rect)
        })
        .map(|(dwarf, _)| dwarf.id.clone())
        .collect()
}

fn tray_hit(scene: &Scene, pointer: Pos2) -> bool {
    scene
        .comms
        .is_some_and(|room| room.tray_rect().contains(pointer))
}

/// The dwarf under `pointer`, topmost first.
fn hit(scene: &Scene, placed: &[Placed], pointer: Pos2) -> Option<NodeId> {
    scene
        .dwarves
        .iter()
        .zip(placed)
        .rev()
        .find(|(dwarf, at)| {
            Rect::from_center_size(at.pos, Vec2::splat(dwarf.size)).contains(pointer)
        })
        .map(|(dwarf, _)| dwarf.id.clone())
}

#[allow(clippy::too_many_arguments)]
fn paint_ground(
    painter: &egui::Painter,
    sprites: &Sprites,
    scene: &Scene,
    palette: Palette,
    now_s: f64,
    daylight: f32,
    portal_frame: u8,
    rubble_view: &rubble::RubbleView,
    heaps: &rubble::Rubble,
    ledger_count: usize,
) {
    let px = scene.px;
    let a = scene.cell();
    sprites.ground(painter, scene.rect, scene.surface_y, px, palette);
    // The sky and mountains behind the excavation heaps. Through dawn and
    // dusk the stars fade, and one orb sets before the other rises.
    if daylight < 1.0 {
        for (at, bright) in scenery::stars(scene) {
            sprites.star(painter, at, px, bright, 1.0 - daylight);
        }
    }
    let night = daylight < 0.5;
    for piece in scenery::sky(scene, now_s, night) {
        let alpha = if piece.deco == nature::Deco::Cloud {
            0.35 + 0.65 * daylight
        } else {
            (2.0 * daylight - 1.0).abs()
        };
        sprites.deco(painter, piece.deco, piece.foot, px, alpha);
    }
    sprites.peaks(painter, scene);
    sprites.surface_edge(painter, scene.rect, scene.surface_y, px);
    // The rubble stands behind everything else on the surface.
    rubble_view.paint_heaps(painter, sprites, scene, heaps, palette, now_s);
    let plants = scenery::surface(scene);
    // Shafts up from the gallery to the landing, the throne hall and storage.
    let gy = scene.gallery.center().y;
    let up = |x: f32, top: f32| {
        Rect::from_min_max(egui::pos2(x - 0.6 * a, top), egui::pos2(x + 0.6 * a, gy))
    };
    for shaft in [
        up(scene.entrance_x(), scene.surface_y - px),
        up(
            scene.throne_hall.center().x,
            scene.throne_hall.bottom() - px,
        ),
        up(scene.storage.center().x, scene.storage.bottom() - px),
    ] {
        sprites.passage(painter, shaft, px);
        sprites.ladder(painter, shaft, px);
    }
    let ladder = scene.work_ladder();
    sprites.passage(painter, ladder, px);
    for gallery in scene.work_galleries() {
        sprites.passage(painter, gallery, px);
        sprites.gallery(painter, gallery, px);
    }
    sprites.ladder(painter, ladder, px);
    sprites.passage(painter, scene.gallery, px);
    sprites.gallery(painter, scene.gallery, px);
    sprites.standing(
        painter,
        world::Prop::Spoil,
        egui::pos2(scene.entrance_x() - 17.0 * px, scene.surface_y + 2.0 * px),
        px,
        1.0,
    );
    sprites.entrance(painter, scene.entrance_x(), scene.surface_y, px);
    if scene.entrance_x() - 41.0 * px >= scene.rect.left() {
        sprites.standing(
            painter,
            world::Prop::PitheadWinch,
            egui::pos2(scene.entrance_x() - 31.0 * px, scene.surface_y),
            px * 0.8,
            1.0,
        );
    }
    for plant in &plants {
        sprites.deco(painter, plant.deco, plant.foot, px, 1.0);
    }
    ledger::paint(painter, sprites, scene, ledger_count);
    sprites.masonry(painter, scene.throne_hall);
    sprites.throne_hall(painter, scene.throne_hall, px);
    sprites.masonry(painter, scene.storage);
    for tunnel in &scene.tunnels {
        if tunnel.key != TunnelKey::RestingArea {
            let prop = world::Prop::ToolRack;
            let size = prop.size().map(|n| n as f32 * px * 0.85);
            let dressing = Rect::from_min_size(
                tunnel.chamber.min + vec2(12.0, 12.0) * px,
                vec2(size[0], size[1]),
            );
            // Fill an unused part of the working face, leaving bodies and tags readable.
            let occupied = scene.dwarves.iter().any(|dwarf| {
                Rect::from_center_size(dwarf.pos, Vec2::splat(dwarf.size))
                    .expand(10.0 * px)
                    .intersects(dressing)
            });
            if !occupied && tunnel.chamber.shrink(4.0 * px).contains_rect(dressing) {
                sprites.prop(painter, prop, dressing, 1.0);
            }
        }
    }
    for bed in &scene.beds {
        sprites.bed(painter, bed.rect, px);
    }

    // A single header line leaves the status bubbles clear of the name and task count.
    let font = FontId::monospace(9.0 + 2.0 * px);
    for tunnel in &scene.tunnels {
        let top = tunnel.label_top;
        let width = tunnel.chamber.width() + 0.4 * a;
        let title = match tunnel.tasks {
            Some((done, total)) => format!("{done}/{total} · {}", tunnel.label),
            None => tunnel.label.clone(),
        };
        let text = label(
            painter,
            top,
            Align2::CENTER_TOP,
            &title,
            font.clone(),
            width,
            px,
            palette,
        );
        if let Some(progress) = tunnel.progress {
            let bar_w = width.min(text.width().max(3.0 * a));
            let bar = Rect::from_center_size(
                egui::pos2(top.x, text.bottom() + 2.0 * px),
                vec2(bar_w, 2.0 * px),
            );
            painter.rect_filled(bar.expand(px), 0.0, palette.label_shadow);
            painter.rect_filled(bar, 0.0, world::WOOD_DARK);
            let filled = Rect::from_min_size(bar.min, vec2(bar.width() * progress, bar.height()));
            painter.rect_filled(filled, 0.0, world::GOLD);
        }
    }
    sprites.portal(
        painter,
        scene.user_portal(),
        px,
        portal_frame,
        &crate::user::name(),
        FontId::monospace(10.0 + 2.0 * px),
    );
    label(
        painter,
        scene.storage.left_top() + vec2(4.0 * px, 3.0 * px),
        Align2::LEFT_TOP,
        "Treasury · Ready for review",
        font,
        scene.storage.width(),
        px,
        palette,
    );
    sprites.standing(
        painter,
        world::Prop::Chest,
        scene.storage.left_bottom() + vec2(18.0, -3.0) * px,
        px,
        1.0,
    );
    // Each PR adds gold and one cut gem to the treasury.
    for (i, dwarf) in scene
        .dwarves
        .iter()
        .filter(|a| {
            matches!(a.spot, layout::Spot::Storage(_)) && a.mark == Some(layout::Mark::PrReady)
        })
        .enumerate()
    {
        let (row, col) = (i / 4, i % 4);
        let min = scene.storage.left_top()
            + vec2(30.0 + col as f32 * 20.0, 13.0 + row as f32 * 12.0) * px;
        sprites.pile(
            painter,
            Rect::from_min_size(min, vec2(18.0, 14.0) * px),
            (dwarf::seed_of(dwarf.id.as_str()) % 3) as u8,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_dwarves(
    painter: &egui::Painter,
    sprites: &Sprites,
    figures: &mut Figures,
    scene: &Scene,
    placed: &[Placed],
    now_s: f64,
    selection: &BTreeSet<NodeId>,
    hovered: Option<&NodeId>,
    effects: &Effects,
    ui: &Ui,
) {
    let px = scene.px;
    let origin = king_center(scene);
    let max_r = farthest(scene, origin);
    let step = (now_s * STEP_FPS) as usize;
    let accent = ui.visuals().selection.bg_fill;
    let palette = Palette::new(daylight::daylight());
    let hover = ui.visuals().widgets.hovered.fg_stroke.color;
    for (dwarf, at) in scene.dwarves.iter().zip(placed) {
        let walking = at.heading.is_some();
        let mut cell = Rect::from_center_size(at.pos, Vec2::splat(dwarf.size));
        if dwarf.kind == crate::model::NodeKind::Envoy {
            cell = cell.translate(vec2(effects.envoy_receipt.offset, 0.0));
        }
        let tpx = dwarf.size / layout::CELL;
        let facing_right = match at.heading {
            Some(h) if h.x.abs() > 0.1 => h.x > 0.0,
            _ => dwarf.facing_right,
        };
        if !walking {
            match dwarf.pose {
                Pose::Dig | Pose::Build => {
                    cell = cell.translate(vec2(0.0, -((step % 2) as f32) * tpx))
                }
                // Resting figures sit lower on the floor.
                Pose::Sleep | Pose::Failed if dwarf.state != crate::model::NodeState::Stalled => {
                    cell = cell.translate(vec2(0.0, 2.0 * tpx))
                }
                // A busy King hops about; otherwise he sits still.
                Pose::King if effects.king_busy => {
                    cell = cell.translate(vec2(0.0, -KING_HOP[step % KING_HOP.len()] * tpx))
                }
                _ => {}
            }
        }
        // Dwarves hop as a call's ring passes them.
        if dwarf.pose != Pose::King {
            let dist = (at.pos - origin).length();
            let lift: f32 = effects
                .ripples
                .iter()
                .filter_map(|r| ripple_radius(r.pulse, now_s - r.start_s, max_r))
                .map(|radius| hop(dist, radius, scene.cell(), 4.0 * tpx))
                .fold(0.0, f32::max);
            cell = cell.translate(vec2(0.0, -(lift / tpx).round() * tpx));
        }
        // A dwarf hops as it pops.
        cell = cell.translate(vec2(
            0.0,
            -pops::lift(pops::of(effects.pops, &dwarf.id), dwarf.size),
        ));
        // The name tag stays on the floor; a sleeper in bed lies on its mattress.
        let tag_at = cell.center_bottom();
        let bed = scene
            .beds
            .iter()
            .find(|b| b.sleeper.as_ref() == Some(&dwarf.id))
            .filter(|_| !walking && dwarf.pose == Pose::Sleep);
        if bed.is_some() {
            cell = cell.translate(vec2(0.0, -layout::SLEEPER_LIFT * tpx));
        }
        let carrying = dwarf.pose == Pose::Carry;
        // Messages reaching the Envoy: he casts, his crystal lighting the room.
        let casting =
            dwarf.kind == crate::model::NodeKind::Envoy && effects.envoy_receipt.casting();
        if casting {
            let dir = if facing_right { 1.0 } else { -1.0 };
            let crystal = cell.center_bottom() + vec2(0.32 * dir, -1.0) * cell.height() * 0.95;
            sprites.glow(painter, crystal, 0.45 * cell.height(), world::BLUE, 0.6);
            // Sparks wink round the crystal.
            let twinkle = (now_s * 8.0) as usize;
            for (i, (dx, dy)) in [(-0.22, -0.1), (0.2, -0.18), (0.1, 0.16)]
                .into_iter()
                .enumerate()
            {
                if !(twinkle + i).is_multiple_of(3) {
                    let at = crystal + vec2(dx * dir, dy) * cell.height();
                    let at = Pos2::new((at.x / px).round() * px, (at.y / px).round() * px);
                    sprites.sparkle(
                        painter,
                        at,
                        px,
                        world::LIGHT,
                        (twinkle + i).is_multiple_of(2),
                    );
                }
            }
        }
        let figure = Figure {
            key: dwarf.id.as_str(),
            look: dwarf_look(dwarf),
            action: if casting {
                Action::Cast
            } else if walking && at.heading.is_some_and(|h| h.y.abs() > h.x.abs()) {
                Action::Climb
            } else {
                dwarf_action(dwarf, walking, carrying, effects.king_busy)
            },
            upside_down: false,
            facing_right,
            alpha: dwarf.alpha,
        };
        if dwarf.pose == Pose::King {
            figures.body(painter, &figure, cell, px, now_s);
            // Ringed while selected or while his chat is the conversation shown.
            if selection.contains(&dwarf.id) || effects.king_focused {
                let ring = cell.expand(px);
                painter.rect_stroke(ring, 0.0, Stroke::new(px, accent), StrokeKind::Outside);
            } else if hovered == Some(&dwarf.id) {
                painter.rect_stroke(
                    cell.expand(px),
                    0.0,
                    Stroke::new(1.0, hover),
                    StrokeKind::Outside,
                );
            }
            continue;
        }
        figures.body(painter, &figure, cell, px, now_s);
        if let Some(bed) = bed {
            sprites.blanket(painter, bed.rect, cell, tpx, dwarf.alpha);
        }
        if !walking && dwarf.pose == Pose::Dig {
            let seed = (dwarf.pos.x * 0.37 + dwarf.pos.y * 0.11).rem_euclid(1.0);
            let t = (now_s * 6.0).floor() / 6.0;
            sprites.dirt(painter, cell, facing_right, tpx, t, seed);
        }
        if !walking && dwarf.mark == Some(layout::Mark::Stalled) {
            sprites.bonk(painter, cell, tpx, step);
        }
        if let Some(mark) = dwarf.mark
            && !walking
        {
            let size = (16.0 * px).max(layout::CELL * (tpx - 1.0).max(1.0));
            let mut anchor = cell.center() + vec2(0.25 * dwarf.size, -tpx);
            if mark == layout::Mark::Sleep {
                // The bubble floats, and z's drift up from it and fade.
                anchor.y -= SLEEP_BOB[step % SLEEP_BOB.len()] * tpx;
                let offset = (dwarf.pos.x * 0.37 + dwarf.pos.y * 0.11).rem_euclid(1.0) as f64;
                for i in 0..2 {
                    let t = ((step as f64 / 10.0) + offset + i as f64 * 0.5).fract() as f32;
                    let pos = anchor
                        + vec2(
                            0.35 * size + (t * 6.0).sin() * 2.0 * tpx,
                            -size - t * 1.2 * size,
                        );
                    let snapped = egui::pos2((pos.x / px).round() * px, (pos.y / px).round() * px);
                    sprites.snore(painter, snapped, tpx, (1.0 - t) * dwarf.alpha, palette);
                }
            }
            sprites.bubble(painter, anchor, size, mark, dwarf.alpha);
        }
        if let Some(name) = &dwarf.tag {
            let selected = selection.contains(&dwarf.id);
            let width = 2.0 * scene.cell() - 2.0 * px;
            tag(
                painter,
                tag_at,
                name,
                px,
                width,
                selected.then_some(accent),
                palette,
            );
        }
        let ring = cell.expand(px);
        if selection.contains(&dwarf.id) {
            painter.rect_stroke(ring, 0.0, Stroke::new(px, accent), StrokeKind::Outside);
        } else if hovered == Some(&dwarf.id) {
            painter.rect_stroke(ring, 0.0, Stroke::new(1.0, hover), StrokeKind::Outside);
        }
    }
}

/// Pixel rendering settings and cache for this frame's dwarves.
struct Figures<'a> {
    art: &'a ArtSettings,
    dwarves: &'a mut DwarfArt,
}

impl Figures<'_> {
    fn body(&mut self, painter: &egui::Painter, figure: &Figure, cell: Rect, px: f32, now_s: f64) {
        self.dwarves
            .paint(painter, self.art, figure, cell, px, now_s);
    }
}

/// How `dwarf` looks: the King role for the King node, otherwise one in the role of its
/// kind picked by its id, dressed in its agent's livery.
fn dwarf_look(dwarf: &layout::Dwarf) -> Look {
    use crate::model::NodeKind;
    let role = match dwarf.kind {
        NodeKind::King => return Look::king(),
        NodeKind::Coordinator | NodeKind::Attendant => Role::Foreman,
        NodeKind::Subagent => Role::Apprentice,
        NodeKind::Envoy => Role::RunePriest,
        NodeKind::Lorekeeper => Role::Librarian,
        NodeKind::Worker | NodeKind::Terminal => Role::Miner,
    };
    let mut look = Look::for_role(role, dwarf::seed_of(dwarf.id.as_str()));
    let coat = Coat::new(dwarf.agent.as_ref());
    art::dress(&mut look, coat);
    if dwarf.state == crate::model::NodeState::Stalled
        && !matches!(role, Role::RunePriest | Role::Librarian)
    {
        look.hat = dwarf::Hat::Helmet;
    }
    look
}

/// What `dwarf` is doing. Only work moves: waiting, sleeping and failed dwarves hold
/// a pose.
fn dwarf_action(dwarf: &layout::Dwarf, walking: bool, carrying: bool, king_busy: bool) -> Action {
    use crate::model::NodeState;
    if dwarf.kind == crate::model::NodeKind::Envoy {
        return if dwarf.state == NodeState::Working {
            Action::Scry
        } else {
            Action::Still
        };
    }
    match dwarf.pose {
        _ if walking && carrying => Action::Carry,
        _ if walking => Action::Walk,
        _ if carrying => Action::Hold,
        _ if dwarf.state == NodeState::Stalled => Action::Wait,
        Pose::Stand if dwarf.kind == crate::model::NodeKind::Lorekeeper => Action::Read,
        Pose::Dig => Action::Mine,
        Pose::Build => Action::Build,
        Pose::Wait => Action::Wait,
        Pose::Sleep => Action::Sleep,
        Pose::Failed => Action::KnockedOut,
        Pose::King if king_busy => Action::Study,
        Pose::Stand if dwarf.state == NodeState::Working => Action::Study,
        _ => Action::Still,
    }
}

/// Dwarves that left the clan, falling legs up.
fn paint_falls(
    painter: &egui::Painter,
    figures: &mut Figures,
    scene: &Scene,
    falls: &[Fall],
    now_s: f64,
) {
    for fall in falls {
        let Some(frame) = fall.frame(scene.rect, now_s) else {
            continue;
        };
        let role = match fall.body.kind {
            crate::model::NodeKind::Coordinator | crate::model::NodeKind::Attendant => {
                Role::Foreman
            }
            crate::model::NodeKind::Envoy => Role::RunePriest,
            crate::model::NodeKind::Lorekeeper => Role::Librarian,
            _ => Role::Miner,
        };
        let mut dwarf = Look::for_role(role, fall.body.seed);
        art::dress(&mut dwarf, fall.body.coat);
        let key = format!("fall {}", fall.body.seed);
        let figure = Figure {
            key: &key,
            look: dwarf,
            action: Action::KnockedOut,
            upside_down: frame.upside_down,
            facing_right: fall.body.facing_right,
            alpha: frame.alpha,
        };
        let cell = Rect::from_center_size(frame.pos, Vec2::splat(fall.body.size));
        figures.body(painter, &figure, cell, scene.px, now_s);
    }
}

/// Lines a name tag wraps onto before it is cut.
const TAG_ROWS: usize = 2;

/// Opacity of the plate behind a room's label.
const LABEL_PLATE_ALPHA: f32 = 0.78;

/// The sleep bubble's float, in texels per step.
const SLEEP_BOB: [f32; 4] = [0.0, 1.0, 2.0, 1.0];

/// The effects running this frame, from [`FarmView`].
struct Effects<'a> {
    ripples: &'a [Ripple],
    births: &'a [f64],
    king_busy: bool,
    king_focused: bool,
    envoy_receipt: crate::ui::envoy_receipt::ReceiptCue,
    pops: &'a [(NodeId, crate::ui::state_pop::PopFrame)],
}

/// Where the King sits.
fn king_center(scene: &Scene) -> Pos2 {
    scene
        .dwarves
        .iter()
        .find(|a| a.pose == Pose::King)
        .map_or(scene.throne_hall.center(), |q| q.pos)
}

/// Distance from `origin` to the farm's farthest corner.
fn farthest(scene: &Scene, origin: Pos2) -> f32 {
    let r = scene.rect;
    [
        r.left_top(),
        r.right_top(),
        r.left_bottom(),
        r.right_bottom(),
    ]
    .iter()
    .map(|c| (*c - origin).length())
    .fold(0.0, f32::max)
}

/// The King's calls: his glow and the rings behind the dwarves (`behind`), then the sparkles
/// riding the rings and those of a birth over them.
fn paint_calls(
    painter: &egui::Painter,
    sprites: &Sprites,
    scene: &Scene,
    effects: &Effects,
    now_s: f64,
    behind: bool,
) {
    let px = scene.px;
    let a = scene.cell();
    let origin = king_center(scene);
    let max_r = farthest(scene, origin);
    let step = (now_s * 12.0) as usize;
    for ripple in effects.ripples {
        let color = ripple.pulse.color();
        let elapsed = now_s - ripple.start_s;
        // An outward call fades as it spreads; an inward one holds until it reaches the King,
        // who glows and sparkles as it arrives.
        let (fade, glow) = if ripple.pulse.inward() {
            (1.0, arrival_glow(elapsed))
        } else {
            let fade = (1.0 - elapsed / (RIPPLE_S + ECHO_S)).clamp(0.0, 1.0) as f32;
            (fade, fade)
        };
        if behind {
            sprites.glow(painter, origin, 2.5 * a, color, glow);
        } else if ripple.pulse.inward() && glow > 0.0 {
            for i in 0..8 {
                let angle = i as f32 / 8.0 * std::f32::consts::TAU + elapsed as f32;
                let p = origin + vec2(angle.cos(), angle.sin() * 0.6) * (0.6 + glow * 0.6) * a;
                let big = (i + step).is_multiple_of(2);
                sprites.sparkle(painter, p, px, color.gamma_multiply(glow), big);
            }
        }
        for (delay, strength) in [(0.0, 1.0), (ECHO_S, 0.5)] {
            let Some(radius) = ripple_radius(ripple.pulse, elapsed - delay, max_r) else {
                continue;
            };
            let alpha = fade * strength;
            if behind {
                sprites.ring(
                    painter,
                    origin,
                    radius,
                    px,
                    color.gamma_multiply(alpha * 0.8),
                );
                continue;
            }
            // Sparkles ride the ring, twinkling.
            for i in 0..24 {
                if (i + step).is_multiple_of(3) {
                    continue;
                }
                let angle = i as f32 / 24.0 * std::f32::consts::TAU + radius * 0.002;
                let jitter = ((i * 7) % 5) as f32 * px;
                let p = origin + vec2(angle.cos(), angle.sin()) * (radius - jitter);
                if scene.rect.contains(p) {
                    let big = (i + step).is_multiple_of(5);
                    sprites.sparkle(painter, p, px, color.gamma_multiply(alpha), big);
                }
            }
        }
    }
    // A birth: the King glows warm and sparkles.
    for &born in effects.births {
        let t = ((now_s - born) / BIRTH_GLOW_S).clamp(0.0, 1.0) as f32;
        let warm = egui::Color32::from_rgb(0xff, 0xe0, 0x8a);
        if behind {
            sprites.glow(painter, origin, 1.6 * a, warm, 1.0 - t);
            continue;
        }
        for i in 0..6 {
            let angle = i as f32 / 6.0 * std::f32::consts::TAU + t * 2.0;
            let p = origin + vec2(angle.cos(), angle.sin() * 0.6) * (0.8 + t) * a;
            sprites.sparkle(
                painter,
                p,
                px,
                warm.gamma_multiply(1.0 - t),
                (i + step).is_multiple_of(2),
            );
        }
    }
}

/// A room's label: light text on a rounded dark plate so it reads over any rock or sky, cut to
/// `width`; returns where the text went.
#[allow(clippy::too_many_arguments)]
fn label(
    painter: &egui::Painter,
    pos: Pos2,
    align: Align2,
    text: &str,
    font: FontId,
    width: f32,
    px: f32,
    palette: Palette,
) -> Rect {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, palette.label);
    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(8.0));
    let galley = painter.layout_job(job);
    let rect = align.anchor_size(pos, galley.size());
    let plate = rect.expand2(vec2(2.0 * px + 1.0, 0.5 * px + 1.0));
    painter.rect(
        plate,
        2.0 * px,
        palette.label_shadow.gamma_multiply(LABEL_PLATE_ALPHA),
        Stroke::new(1.0, palette.label.gamma_multiply(0.18)),
        StrokeKind::Inside,
    );
    painter.galley(rect.min, galley, palette.label);
    rect
}

/// A dwarf's name tag under `top`: light text on a dark chip, or on `highlight` when the dwarf is
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
    let mut job = egui::text::LayoutJob::simple(name.to_string(), font, palette.label, 0.0);
    job.wrap = egui::text::TextWrapping {
        max_width: (width - 2.0 * px).max(8.0),
        max_rows: TAG_ROWS,
        break_anywhere: false,
        overflow_character: Some('…'),
    };
    job.halign = egui::Align::Center;
    let galley = painter.layout_job(job);
    // Centered lines sit around x = 0 in the galley; place its rect's middle under `top`.
    let text = galley.rect;
    let origin = top - vec2(text.center().x, text.top());
    let chip = text.translate(origin.to_vec2()).expand2(vec2(px, 0.0));
    let fill = highlight.unwrap_or(palette.label_shadow.gamma_multiply(0.75));
    painter.rect_filled(chip, 0.0, fill);
    painter.galley(origin, galley, palette.label);
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

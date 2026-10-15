//! Persistent packing of group tunnels into twelve-cell rows. A group keeps its cells
//! while its dwarves fit; spare slots, wrapped row height and the empty-room grace avoid
//! shuffling on ordinary updates. Repacking uses first-fit decreasing width with stable
//! ties. Every segment reserves a ladder and a stone wall, and every level has an access
//! passage above its labels. Wrapped work rows remain inside the group's rectangle.

use eframe::egui::Vec2;
use serde::{Deserialize, Serialize};

use super::layout::{
    CELL, Crowd, LADDER_GAP, SLOT, TunnelKey, band, chamber_per_row, pack, rows_height,
};

/// Dwarf slots a room is dug for beyond its dwarves, when the view has the room.
pub const SPARE_SLOTS: usize = 1;
/// Fixed packing cells across the work area.
pub const COLUMNS: usize = 12;
/// An access passage and rock above every level after the first.
pub const ACCESS_GAP: f32 = 1.6;
/// How long a room stands empty before it is filled in.
pub const GRACE_MS: i64 = 120_000;
/// Biggest points per texel: a near-empty farm draws no giant dwarves.
pub const MAX_PX: f32 = 4.5;
/// Smallest points per texel, which a farm too big for the view still gets.
const MIN_PX: f32 = 1.0;
/// Steps between scales: halves are whole pixels on a Retina screen.
const PX_STEP: f32 = 0.5;
/// Height over each level of rooms for their labels and progress bars, in cells.
pub const LABEL_GAP: f32 = 1.3;

/// One workspace band in the plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Room {
    pub key: TunnelKey,
    pub level: usize,
    pub col: usize,
    #[serde(default)]
    pub span: usize,
    /// Rows of dwarves it is dug for.
    pub rows: usize,
    /// Since when it has had no dwarves (ms since the epoch).
    pub empty_since_ms: Option<i64>,
}

/// The grid's scale and shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grid {
    /// Screen points per sprite texel.
    pub px: f32,
    pub levels: usize,
    /// Reserved height of each workspace band, excluding its label.
    pub heights: Vec<f32>,
}

/// What the plan was last fitted to: the view's size and the surface band's crowds.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
struct Fit {
    size: [f32; 2],
    landing: usize,
    storage: usize,
    envoy: bool,
}

/// The rooms wanted this frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Demand {
    /// Each room's key and its dwarves' chamber footprints, in the order a rearrange lists them.
    pub rooms: Vec<(TunnelKey, Vec<usize>)>,
    pub landing: usize,
    pub storage: usize,
    pub envoy: bool,
}

impl Demand {
    fn footprints(&self, key: &TunnelKey) -> Option<&[usize]> {
        self.rooms
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, f)| f.as_slice())
    }
}

/// What an update did to the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Every room kept its cell and size.
    Kept,
    /// A room dug deeper or emptied; the others kept their cells.
    Grew,
    /// The grid was laid out afresh.
    Rearranged,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoomPlan {
    /// Earlier full-width and column plans must be refitted with ladder reservations.
    #[serde(rename = "packed_tunnels")]
    fitted: Option<Grid>,
    pub rooms: Vec<Room>,
    /// Dwarves the surface band is sized for: it grows with them and shrinks on a rearrange.
    pub landing: usize,
    pub storage: usize,
    fit: Fit,
    /// Rearranges so far, for tests.
    #[serde(skip)]
    pub rearranges: u64,
}

/// The grid in a view: sizes of its cells.
#[derive(Debug, Clone)]
pub struct Ground {
    pub a: f32,
    pub px: f32,
    cell_w: f32,
    tops: Vec<f32>,
    heights: Vec<f32>,
}

impl Ground {
    pub fn new(size: Vec2, grid: &Grid, crowd: Crowd) -> Self {
        let a = CELL * grid.px;
        let below_top = band(size, a, crowd).gallery_top(a) + (1.2 + LABEL_GAP) * a;
        let tops = grid
            .heights
            .iter()
            .scan(below_top, |top, height| {
                let at = *top;
                *top += height + (LABEL_GAP + ACCESS_GAP) * a;
                Some(at)
            })
            .collect();
        let cell_w = (size.x - LADDER_GAP * a).max(a) / COLUMNS as f32;
        Self {
            a,
            px: grid.px,
            cell_w,
            tops,
            heights: grid.heights.clone(),
        }
    }

    /// Each packed segment reserves its own ladder on the left and stone at its edges.
    pub fn chamber_left(&self, col: usize) -> f32 {
        LADDER_GAP * self.a + col as f32 * self.cell_w + (LADDER_GAP + 0.3) * self.a
    }

    pub fn chamber_w(&self, span: usize) -> f32 {
        (span as f32 * self.cell_w - (LADDER_GAP + 0.6) * self.a).max(3.0 * self.a)
    }

    pub fn per_row(&self, span: usize) -> usize {
        chamber_per_row(self.chamber_w(span) - self.a, self.a)
    }

    /// The most rows a level's band has the height for.
    pub fn max_rows(&self, level: usize) -> usize {
        max_rows(self.heights[level], self.a)
    }

    pub fn band_top(&self, level: usize) -> f32 {
        self.tops[level]
    }
}

fn max_rows(band_h: f32, a: f32) -> usize {
    let mut rows = 0;
    while rows_height(rows + 1, a) <= band_h {
        rows += 1;
    }
    rows
}

/// Rows `footprints` and `spare` more single slots take, `per_row` to a row.
fn rows_for(footprints: &[usize], spare: usize, per_row: usize) -> usize {
    let mut all = footprints.to_vec();
    all.extend(std::iter::repeat_n(1, spare));
    pack(&all, per_row).1
}

/// Rows to dig for `footprints`: with spare slots where the band has the height.
fn target_rows(footprints: &[usize], per_row: usize, max_rows: usize) -> usize {
    (0..=SPARE_SLOTS)
        .rev()
        .map(|spare| rows_for(footprints, spare, per_row))
        .find(|rows| *rows <= max_rows)
        .unwrap_or_else(|| rows_for(footprints, 0, per_row))
}

#[derive(Clone, Copy)]
struct Place {
    level: usize,
    col: usize,
    span: usize,
}

/// First-fit decreasing width. Ties use the stable demand order, never node order.
/// The resting area retains its own final band and beds.
fn fit(
    size: Vec2,
    crowd: Crowd,
    rooms: &[&[usize]],
    resting: Option<usize>,
    px: f32,
    spare: usize,
) -> (Grid, Vec<Place>) {
    let a = CELL * px;
    let cell_w = (size.x - LADDER_GAP * a).max(a) / COLUMNS as f32;
    let mut places: Vec<Place> = rooms
        .iter()
        .map(|f| {
            let slots = (f.iter().sum::<usize>() + spare).max(1);
            let width = (LADDER_GAP + 0.6 + 3.0 + (slots - 1) as f32 * SLOT) * a;
            let span = ((width / cell_w).ceil() as usize).clamp(3, COLUMNS);
            Place {
                level: 0,
                col: 0,
                span,
            }
        })
        .collect();
    let mut order: Vec<usize> = (0..rooms.len()).collect();
    order.sort_by_key(|&i| (Some(i) == resting, std::cmp::Reverse(places[i].span), i));
    let mut used: Vec<usize> = Vec::new();
    for i in order {
        let p = &mut places[i];
        if Some(i) == resting {
            p.span = COLUMNS;
        }
        let level = if Some(i) == resting {
            None
        } else {
            used.iter().position(|n| n + p.span <= COLUMNS)
        }
        .unwrap_or_else(|| {
            used.push(0);
            used.len() - 1
        });
        p.level = level;
        p.col = used[level];
        used[level] += p.span;
    }
    let levels = used.len().max(1);
    let mut grid = Grid {
        px,
        levels,
        heights: vec![rows_height(1, a); levels],
    };
    let ground = Ground::new(size, &grid, crowd);
    for (f, p) in rooms.iter().zip(&places) {
        let height = rows_height(rows_for(f, spare, ground.per_row(p.span)), a);
        grid.heights[p.level] = grid.heights[p.level].max(height);
    }
    (grid, places)
}

fn fits(size: Vec2, crowd: Crowd, grid: &Grid) -> bool {
    let a = CELL * grid.px;
    let below_top = band(size, a, crowd).gallery_top(a) + (1.2 + LABEL_GAP) * a;
    let bottom = below_top
        + grid.heights.iter().sum::<f32>()
        + (grid.levels - 1) as f32 * (LABEL_GAP + ACCESS_GAP) * a;
    bottom <= size.y - 0.3 * a + 0.01
        && size.x >= (2.0 * LADDER_GAP + 3.6) * a
        && (!crowd.envoy
            || grid.px <= (size.x / (18.0 * CELL) / PX_STEP).floor().max(1.0) * PX_STEP)
}

/// Scales tried, biggest first.
fn scales() -> impl Iterator<Item = f32> {
    let steps = ((MAX_PX - MIN_PX) / PX_STEP).round() as usize;
    (0..=steps).map(|i| MAX_PX - i as f32 * PX_STEP)
}

impl RoomPlan {
    pub fn grid(&self) -> Option<&Grid> {
        self.fitted.as_ref()
    }

    pub fn room(&self, key: &TunnelKey) -> Option<&Room> {
        self.rooms.iter().find(|r| &r.key == key)
    }

    /// The crowds the surface band is sized for.
    pub fn crowd(&self, envoy: bool) -> Crowd {
        Crowd {
            landing: self.landing,
            storage: self.storage,
            envoy,
        }
    }

    pub fn ground(&self, size: Vec2, envoy: bool) -> Option<Ground> {
        let grid = self.fitted.as_ref()?;
        Some(Ground::new(size, grid, self.crowd(envoy)))
    }

    /// Brings the plan up to `demand` in a view of `size` at `now_ms`.
    pub fn update(&mut self, demand: &Demand, size: Vec2, now_ms: i64) -> Change {
        let mut change = Change::Kept;
        for room in &mut self.rooms {
            if demand.footprints(&room.key).is_some() {
                room.empty_since_ms = None;
            } else if room.empty_since_ms.is_none() {
                room.empty_since_ms = Some(now_ms);
                change = Change::Grew;
            }
        }
        let before = self.rooms.len();
        self.rooms
            .retain(|r| r.empty_since_ms.is_none_or(|t| now_ms - t < GRACE_MS));
        let filled_in = self.rooms.len() != before;
        let Some(grid) = self.fitted.clone() else {
            return self.rearrange(demand, size);
        };
        let new_room = demand.rooms.iter().any(|(k, _)| self.room(k).is_none());
        let resized = self.fit.size != [size.x, size.y] || self.fit.envoy != demand.envoy;
        let consistent = grid.heights.len() == grid.levels
            && self
                .rooms
                .iter()
                .all(|r| r.level < grid.levels && r.span > 0 && r.col + r.span <= COLUMNS);
        if filled_in || new_room || resized || !consistent {
            return self.rearrange(demand, size);
        }
        // More dwarves on the landing or in storage: fine while the band keeps its height.
        let landing = self.landing.max(demand.landing);
        let storage = self.storage.max(demand.storage);
        if (landing, storage) != (self.landing, self.storage) {
            let a = CELL * grid.px;
            let crowd = Crowd {
                landing,
                storage,
                envoy: demand.envoy,
            };
            let height = |crowd: Crowd| band(size, a, crowd).gallery_top(a);
            if height(crowd) > height(self.crowd(demand.envoy)) {
                return self.rearrange(demand, size);
            }
            (self.landing, self.storage) = (landing, storage);
            self.fit.landing = landing;
            self.fit.storage = storage;
        }
        let ground = Ground::new(size, &grid, self.crowd(demand.envoy));
        for room in &mut self.rooms {
            let max_rows = ground.max_rows(room.level);
            let footprints = demand.footprints(&room.key).unwrap_or_default();
            let per_row = ground.per_row(room.span);
            let need = rows_for(footprints, 0, per_row);
            if need <= room.rows {
                continue;
            }
            if need > max_rows {
                return self.rearrange(demand, size);
            }
            room.rows = target_rows(footprints, per_row, max_rows);
            change = Change::Grew;
        }
        change
    }

    /// Lays the grid out afresh: the demanded rooms in their order, then rooms still in their
    /// grace period, at the biggest scale that holds them with a spare slot or two (or, failing
    /// that, none), the band shrunk to its crowds.
    fn rearrange(&mut self, demand: &Demand, size: Vec2) -> Change {
        self.rearranges += 1;
        self.landing = demand.landing;
        self.storage = demand.storage;
        let mut keys: Vec<(TunnelKey, &[usize], Option<i64>)> = demand
            .rooms
            .iter()
            .map(|(k, f)| (k.clone(), f.as_slice(), None))
            .collect();
        for room in &self.rooms {
            if demand.footprints(&room.key).is_none() {
                keys.push((room.key.clone(), &[], room.empty_since_ms));
            }
        }
        let crowd = self.crowd(demand.envoy);
        let footprints: Vec<&[usize]> = keys.iter().map(|(_, f, _)| *f).collect();
        // Keep spare capacity so another dwarf can join without moving the rooms.
        let tries = |spares: &'static [usize]| {
            scales().flat_map(move |px| spares.iter().map(move |&spare| (px, spare)))
        };
        let resting = keys
            .iter()
            .position(|(key, _, _)| *key == TunnelKey::RestingArea);
        let (grid, places) = tries(&[SPARE_SLOTS])
            .chain(tries(&[0]))
            .map(|(px, spare)| fit(size, crowd, &footprints, resting, px, spare))
            .find(|(grid, _)| fits(size, crowd, grid))
            // Beyond the view's capacity, keep ownership and non-overlap even if the
            // lowest bands extend offscreen at the minimum scale.
            .unwrap_or_else(|| fit(size, crowd, &footprints, resting, MIN_PX, 0));
        let ground = Ground::new(size, &grid, crowd);
        self.rooms = keys
            .into_iter()
            .enumerate()
            .map(|(i, (key, footprints, empty_since_ms))| {
                let Place { level, col, span } = places[i];
                let max_rows = ground.max_rows(level);
                Room {
                    key,
                    level,
                    col,
                    span,
                    rows: target_rows(footprints, ground.per_row(span), max_rows),
                    empty_since_ms,
                }
            })
            .collect();
        self.fitted = Some(grid);
        self.fit = Fit {
            size: [size.x, size.y],
            landing: self.landing,
            storage: self.storage,
            envoy: demand.envoy,
        };
        Change::Rearranged
    }
}

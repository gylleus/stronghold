//! Persistent horizontal workspace bands below the shared gallery. Each band owns the
//! full width beside the ladder and enough rows for its dwarves. Spare slots and spare
//! height let rooms grow without moving their neighbours; empty rooms retain their grace.
//! Resizing or exhausting that capacity fits the bands again at the largest usable scale.

use eframe::egui::Vec2;
use serde::{Deserialize, Serialize};

use super::layout::{
    CELL, Crowd, LADDER_GAP, SLOT, TunnelKey, band, chamber_per_row, pack, rows_height,
};

/// Dwarf slots a room is dug for beyond its dwarves, when the view has the room.
pub const SPARE_SLOTS: usize = 2;
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
    /// Width of each column in points; they add up to the view's width.
    pub widths: Vec<f32>,
    /// Rooms stacked in each column.
    pub depths: Vec<usize>,
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
    /// Old column plans are refitted on load so they cannot reconnect workspace rows.
    #[serde(rename = "workspace_bands")]
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
    /// Left edge of each column, from the view's left.
    lefts: Vec<f32>,
    widths: Vec<f32>,
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
                *top += height + LABEL_GAP * a;
                Some(at)
            })
            .collect();
        let available = (size.x - LADDER_GAP * a).max(a);
        let total = grid.widths.iter().sum::<f32>().max(1.0);
        let widths: Vec<f32> = grid.widths.iter().map(|w| w * available / total).collect();
        let lefts = widths
            .iter()
            .scan(LADDER_GAP * a, |x, w| {
                let left = *x;
                *x += w;
                Some(left)
            })
            .collect();
        Self {
            a,
            px: grid.px,
            lefts,
            widths,
            tops,
            heights: grid.heights.clone(),
        }
    }

    /// Left edge of a room column, clear of the shared ladder.
    pub fn chamber_left(&self, col: usize) -> f32 {
        self.lefts.get(col).copied().unwrap_or_default() + 0.3 * self.a
    }

    pub fn chamber_w(&self, _level: usize, col: usize) -> f32 {
        let width = self.widths.get(col).copied().unwrap_or_default();
        (width - 0.6 * self.a).max(SLOT * self.a)
    }

    pub fn per_row(&self, level: usize, col: usize) -> usize {
        chamber_per_row(self.chamber_w(level, col) - self.a, self.a)
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

/// Full-width bands, each sized for its own packed rows. Spare vertical room is
/// shared evenly, allowing a busy workspace to grow without moving another one.
fn fit(size: Vec2, crowd: Crowd, rooms: &[&[usize]], px: f32, spare: usize) -> Grid {
    let a = CELL * px;
    let below_top = band(size, a, crowd).gallery_top(a) + (1.2 + LABEL_GAP) * a;
    let per_row = chamber_per_row(size.x - (LADDER_GAP + 1.6) * a, a);
    let mut heights: Vec<f32> = rooms
        .iter()
        .map(|f| rows_height(rows_for(f, spare, per_row), a))
        .collect();
    if heights.is_empty() {
        heights.push(rows_height(1, a));
    }
    let levels = heights.len();
    let needed = heights.iter().sum::<f32>();
    let available = size.y - 0.3 * a - below_top - (levels - 1) as f32 * LABEL_GAP * a;
    let extra = ((available - needed) / levels as f32).max(0.0);
    for height in &mut heights {
        *height += extra;
    }
    Grid {
        px,
        levels,
        widths: vec![size.x],
        depths: vec![rooms.len()],
        heights,
    }
}

fn fits(size: Vec2, crowd: Crowd, grid: &Grid) -> bool {
    let a = CELL * grid.px;
    let below_top = band(size, a, crowd).gallery_top(a) + (1.2 + LABEL_GAP) * a;
    let bottom =
        below_top + grid.heights.iter().sum::<f32>() + (grid.levels - 1) as f32 * LABEL_GAP * a;
    bottom <= size.y - 0.3 * a + 0.01
        && size.x >= (LADDER_GAP + SLOT + 1.6) * a
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
        let consistent = grid.widths.len() == 1
            && grid.heights.len() == grid.levels
            && self
                .rooms
                .iter()
                .enumerate()
                .all(|(i, r)| r.col == 0 && r.level == i);
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
            let per_row = ground.per_row(room.level, room.col);
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
        let grid = tries(&[2, 1])
            .chain(tries(&[0]))
            .map(|(px, spare)| fit(size, crowd, &footprints, px, spare))
            .find(|grid| fits(size, crowd, grid))
            // Beyond the view's capacity, keep ownership and non-overlap even if the
            // lowest bands extend offscreen at the minimum scale.
            .unwrap_or_else(|| fit(size, crowd, &footprints, MIN_PX, 0));
        let ground = Ground::new(size, &grid, crowd);
        self.rooms = keys
            .into_iter()
            .enumerate()
            .map(|(i, (key, footprints, empty_since_ms))| {
                let (level, col) = (i, 0);
                let max_rows = ground.max_rows(level);
                Room {
                    key,
                    level,
                    col,
                    rows: target_rows(footprints, ground.per_row(level, col), max_rows),
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

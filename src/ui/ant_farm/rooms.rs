//! The room plan: which cell of the grid under the gallery each tunnel's chamber holds and
//! how many rows of ants it is dug for, carried from frame to frame (and across launches) so
//! the farm grows in place instead of being laid out afresh each frame.
//!
//! The ground under the gallery is a grid of `cols` columns on `levels` levels. A room takes
//! one cell, or several side by side once it has grown; it is dug for a few more ants than it
//! holds. Each frame [`RoomPlan::update`] changes as little as it can:
//!
//! - a new room is dug in the first free cell, the others stay put;
//! - an ant that fits the room's rows moves no room;
//! - a full room digs deeper (more rows) while its level has the height, then wider into a
//!   free cell beside it, its neighbours untouched;
//! - an emptied room stays for [`GRACE_MS`] in case its ants come back;
//! - only when a new room finds no free cell, a full room can grow nowhere, or more than half
//!   the cells stand empty, the plan is rearranged: rooms in their usual order, the scale
//!   picked afresh, spare cells and rows added.
//!
//! The plan is a function of the history of demands, view sizes and times it was given.

use eframe::egui::Vec2;
use serde::{Deserialize, Serialize};

use super::layout::{
    CELL, Crowd, SHAFT_GAP, SLOT, TunnelKey, band, chamber_per_row, pack, pixel_scale, rows_height,
};

/// Ant slots a room is dug for beyond its ants, when its level has the height.
pub const SPARE_SLOTS: usize = 2;
/// How long a room stands empty before it is filled in.
pub const GRACE_MS: i64 = 120_000;
/// Scales tried, biggest first.
const SCALES: [f32; 4] = [4.0, 3.0, 2.0, 1.0];

/// Free cells a rearranged grid keeps for new rooms: one per four rooms, at least one.
pub fn spare_cells(rooms: usize) -> usize {
    (rooms / 4).max(1)
}

/// One room in the grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Room {
    pub key: TunnelKey,
    pub level: usize,
    /// Its leftmost column.
    pub col: usize,
    /// Columns it spans.
    pub span: usize,
    /// Rows of ants it is dug for.
    pub rows: usize,
    /// Since when it has had no ants (ms since the epoch).
    pub empty_since_ms: Option<i64>,
}

/// The grid's scale and shape.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Grid {
    /// Screen points per sprite texel.
    pub px: f32,
    pub levels: usize,
    pub cols: usize,
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
    /// Each room's key and its ants' chamber footprints, in the order a rearrange lists them.
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
    /// Rooms were dug, grew or were filled in; the others kept their cells.
    Grew,
    /// The grid was laid out afresh.
    Rearranged,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoomPlan {
    grid: Option<Grid>,
    pub rooms: Vec<Room>,
    /// Ants the surface band is sized for: it grows with them and shrinks on a rearrange.
    pub landing: usize,
    pub storage: usize,
    fit: Fit,
    /// Rearranges so far, for tests.
    #[serde(skip)]
    pub rearranges: u64,
}

/// The grid in a view at one scale: sizes of its cells.
#[derive(Debug, Clone, Copy)]
pub struct Ground {
    pub a: f32,
    pub px: f32,
    pub levels: usize,
    pub column_w: f32,
    /// Height of each level's band under the gallery.
    pub band_h: f32,
    /// From the view's top to the top of the first level's band.
    pub below_top: f32,
    fits: bool,
}

impl Ground {
    pub fn new(size: Vec2, grid: Grid, crowd: Crowd) -> Self {
        let a = CELL * grid.px;
        let gallery_bottom = band(size, a, crowd).gallery_top(a) + 1.2 * a;
        let below_top = gallery_bottom + a;
        let below = (size.y - 0.3 * a) - below_top;
        let band_h = (below - (grid.levels - 1) as f32 * a) / grid.levels as f32;
        let column_w = size.x / grid.cols.max(1) as f32;
        let gap = if grid.levels > 1 { SHAFT_GAP * a } else { 0.0 };
        let fits = column_w >= (SLOT + 0.6) * a + gap
            && band_h >= rows_height(1, a)
            && (!crowd.envoy || grid.px <= (size.x / (18.0 * CELL)).floor().max(1.0));
        Self {
            a,
            px: grid.px,
            levels: grid.levels,
            column_w,
            band_h,
            below_top,
            fits,
        }
    }

    /// Width of a chamber spanning `span` columns on `level`. An upper level's chambers
    /// leave room on their right for the shaft down to the chamber under them.
    pub fn chamber_w(&self, level: usize, span: usize) -> f32 {
        let gap = if level + 1 < self.levels {
            SHAFT_GAP * self.a
        } else {
            0.0
        };
        (span as f32 * self.column_w - 0.6 * self.a - gap).max(SLOT * self.a)
    }

    pub fn per_row(&self, level: usize, span: usize) -> usize {
        chamber_per_row(self.chamber_w(level, span), self.a)
    }

    /// The most rows a level's band has the height for.
    pub fn max_rows(&self) -> usize {
        let mut rows = 0;
        while rows_height(rows + 1, self.a) <= self.band_h {
            rows += 1;
        }
        rows
    }
}

/// Rows `footprints` and `spare` more single slots take, `per_row` to a row.
fn rows_for(footprints: &[usize], spare: usize, per_row: usize) -> usize {
    let mut all = footprints.to_vec();
    all.extend(std::iter::repeat_n(1, spare));
    pack(&all, per_row).1
}

/// Rows to dig for `footprints`: with spare slots where the band has the height.
fn target_rows(footprints: &[usize], per_row: usize, max_rows: usize) -> usize {
    let spare = rows_for(footprints, SPARE_SLOTS, per_row);
    if spare <= max_rows {
        spare
    } else {
        rows_for(footprints, 0, per_row)
    }
}

impl RoomPlan {
    pub fn grid(&self) -> Option<Grid> {
        self.grid
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
            ..Crowd::default()
        }
    }

    fn ground(&self, size: Vec2, grid: Grid, envoy: bool) -> Ground {
        Ground::new(size, grid, self.crowd(envoy))
    }

    /// Brings the plan up to `demand` in a view of `size` at `now_ms`.
    pub fn update(&mut self, demand: &Demand, size: Vec2, now_ms: i64) -> Change {
        let mut change = Change::Kept;
        for room in &mut self.rooms {
            if demand.footprints(&room.key).is_some() {
                room.empty_since_ms = None;
            } else {
                room.empty_since_ms.get_or_insert(now_ms);
            }
        }
        let before = self.rooms.len();
        self.rooms
            .retain(|r| r.empty_since_ms.is_none_or(|t| now_ms - t < GRACE_MS));
        if self.rooms.len() != before {
            change = Change::Grew;
        }
        self.landing = self.landing.max(demand.landing);
        self.storage = self.storage.max(demand.storage);
        let fit = Fit {
            size: [size.x, size.y],
            landing: self.landing,
            storage: self.storage,
            envoy: demand.envoy,
        };
        let Some(grid) = self.grid else {
            return self.rearrange(demand, size);
        };
        if fit != self.fit {
            let resized = fit.size != self.fit.size;
            self.fit = fit;
            if !self.refit(demand, size, grid, resized) {
                return self.rearrange(demand, size);
            }
            change = Change::Grew;
        }
        let grid = self.grid.expect("fitted");
        let ground = self.ground(size, grid, demand.envoy);
        for (key, footprints) in &demand.rooms {
            if self.room(key).is_some() {
                continue;
            }
            let Some((level, col)) = self.free_cell(grid) else {
                return self.rearrange(demand, size);
            };
            let rows = target_rows(footprints, ground.per_row(level, 1), ground.max_rows());
            self.rooms.push(Room {
                key: key.clone(),
                level,
                col,
                span: 1,
                rows,
                empty_since_ms: None,
            });
            change = Change::Grew;
        }
        for (key, footprints) in &demand.rooms {
            let i = self.rooms.iter().position(|r| &r.key == key).expect("dug");
            match self.grow(i, footprints, &ground, grid) {
                Some(true) => change = Change::Grew,
                Some(false) => {}
                None => return self.rearrange(demand, size),
            }
        }
        let used: usize = self.rooms.iter().map(|r| r.span).sum();
        let cells = grid.cols * grid.levels;
        if cells > 2 && used * 2 < cells {
            return self.rearrange(demand, size);
        }
        change
    }

    /// Makes room `i` hold `footprints`: deeper first, then wider into a free cell beside
    /// it. Whether it grew, or `None` when it cannot.
    fn grow(
        &mut self,
        i: usize,
        footprints: &[usize],
        ground: &Ground,
        grid: Grid,
    ) -> Option<bool> {
        let max_rows = ground.max_rows();
        let room = &self.rooms[i];
        let need = rows_for(footprints, 0, ground.per_row(room.level, room.span));
        if need <= room.rows {
            return Some(false);
        }
        if need <= max_rows {
            let per_row = ground.per_row(room.level, room.span);
            self.rooms[i].rows = target_rows(footprints, per_row, max_rows);
            return Some(true);
        }
        loop {
            let (col, span) = self.widen(i, grid)?;
            let level = self.rooms[i].level;
            let per_row = ground.per_row(level, span);
            let room = &mut self.rooms[i];
            (room.col, room.span) = (col, span);
            if rows_for(footprints, 0, per_row) <= max_rows {
                room.rows = target_rows(footprints, per_row, max_rows);
                return Some(true);
            }
        }
    }

    /// Room `i` one column wider, to the right if free, else to the left: its new
    /// `(col, span)`. On an upper level a room may only cover a column's right edge, where the
    /// shaft to the chamber under it passes, while that chamber's cell is free; it then stays
    /// free.
    fn widen(&self, i: usize, grid: Grid) -> Option<(usize, usize)> {
        let room = &self.rooms[i];
        // A lower level's chambers do not widen: their shafts come down at their right edge.
        if room.level > 0 {
            return None;
        }
        let upper = grid.levels > 1;
        let open = |col: usize| self.occupant(room.level, col).is_none();
        let under_free = |col: usize| !upper || self.occupant(1, col).is_none();
        let right = room.col + room.span;
        if right < grid.cols && open(right) && under_free(right - 1) {
            return Some((room.col, room.span + 1));
        }
        if room.col > 0 && open(room.col - 1) && under_free(room.col - 1) {
            return Some((room.col - 1, room.span + 1));
        }
        None
    }

    /// The room covering `(level, col)`.
    fn occupant(&self, level: usize, col: usize) -> Option<&Room> {
        self.rooms
            .iter()
            .find(|r| r.level == level && (r.col..r.col + r.span).contains(&col))
    }

    /// Whether a lower cell's shaft would cut through a widened upper room.
    fn blocked(&self, level: usize, col: usize) -> bool {
        level > 0
            && self
                .rooms
                .iter()
                .any(|r| r.level == 0 && r.col <= col && col + 1 < r.col + r.span)
    }

    /// The first free cell: column by column from the left, upper level first.
    fn free_cell(&self, grid: Grid) -> Option<(usize, usize)> {
        (0..grid.cols)
            .flat_map(|col| (0..grid.levels).map(move |level| (level, col)))
            .find(|&(level, col)| self.occupant(level, col).is_none() && !self.blocked(level, col))
    }

    /// Fits the same grid to a new view size or surface band: the biggest scale (or, when only
    /// the band changed, the current or a smaller one) at which every room still holds its
    /// ants. Whether one was found.
    fn refit(&mut self, demand: &Demand, size: Vec2, grid: Grid, resized: bool) -> bool {
        let scales = SCALES.into_iter().filter(|px| resized || *px <= grid.px);
        for px in scales {
            let grid = Grid { px, ..grid };
            let ground = self.ground(size, grid, demand.envoy);
            if !ground.fits {
                continue;
            }
            let max_rows = ground.max_rows();
            let rows: Option<Vec<usize>> = self
                .rooms
                .iter()
                .map(|r| {
                    let footprints = demand.footprints(&r.key).unwrap_or_default();
                    let per_row = ground.per_row(r.level, r.span);
                    (rows_for(footprints, 0, per_row) <= max_rows)
                        .then(|| target_rows(footprints, per_row, max_rows))
                })
                .collect();
            let Some(rows) = rows else {
                continue;
            };
            // Rooms keep the rows they were dug for where the band still has the height.
            for (room, rows) in self.rooms.iter_mut().zip(rows) {
                room.rows = rows.max(room.rows.min(max_rows));
            }
            self.grid = Some(grid);
            return true;
        }
        false
    }

    /// Lays the grid out afresh: the demanded rooms in their order, then rooms still in their
    /// grace period, with spare cells, the scale picked for them and the band shrunk to its
    /// crowds.
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
        let fullest = keys
            .iter()
            .map(|(_, f, _)| f.iter().sum::<usize>())
            .max()
            .unwrap_or(0);
        // Spare cells and rows, as long as they cost at most a step of scale and never
        // shrink the ants below scale 2.
        let scale = |cells: usize, slots: usize| {
            let crowd = Crowd {
                tunnels: cells,
                chamber: fullest + slots,
                ..self.crowd(demand.envoy)
            };
            (pixel_scale(size, crowd), cells)
        };
        let (rooms, spare) = (keys.len().max(1), keys.len() + spare_cells(keys.len()));
        let tight = scale(rooms, 0);
        let floor = (tight.0.0 - 1.0).max(tight.0.0.min(2.0));
        let ((px, levels), cells) = [
            scale(spare, SPARE_SLOTS),
            scale(rooms, SPARE_SLOTS),
            scale(spare, 0),
        ]
        .into_iter()
        .find(|((px, _), _)| *px >= floor)
        .unwrap_or(tight);
        let grid = Grid {
            px,
            levels,
            cols: cells.div_ceil(levels),
        };
        let ground = self.ground(size, grid, demand.envoy);
        let max_rows = ground.max_rows();
        self.rooms = keys
            .into_iter()
            .enumerate()
            .map(|(i, (key, footprints, empty_since_ms))| {
                let (level, col) = (i % levels, i / levels);
                Room {
                    key,
                    level,
                    col,
                    span: 1,
                    rows: target_rows(footprints, ground.per_row(level, 1), max_rows),
                    empty_since_ms,
                }
            })
            .collect();
        self.grid = Some(grid);
        self.fit = Fit {
            size: [size.x, size.y],
            landing: self.landing,
            storage: self.storage,
            envoy: demand.envoy,
        };
        Change::Rearranged
    }
}

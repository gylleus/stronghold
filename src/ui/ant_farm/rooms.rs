//! The room plan: which cell of the grid under the gallery each tunnel's chamber holds and
//! how many rows of ants it is dug for, carried from frame to frame (and across launches) so
//! an ant joining a room moves nothing else.
//!
//! The ground under the gallery is a grid of columns on `levels` levels, a room to a cell.
//! The plan is fitted to the view: the biggest scale (up to [`MAX_PX`], in half steps) at
//! which every room is dug for its ants and [`SPARE_SLOTS`] more, columns as wide as their
//! rooms need and stretched to fill the width. Each frame [`RoomPlan::update`] changes as
//! little as it can:
//!
//! - an ant that fits its room's rows moves nothing;
//! - a full room digs deeper while its level has the height, nothing else moving;
//! - an emptied room stays for [`GRACE_MS`] in case its ants come back;
//! - a new room, a room filled in, a room that can grow no deeper, a resize, or a surface
//!   band that needs more height lays the plan out afresh (the view glides to it).
//!
//! The plan is a function of the history of demands, view sizes and times it was given.

use eframe::egui::Vec2;
use serde::{Deserialize, Serialize};

use super::layout::{
    CELL, Crowd, SHAFT_GAP, SLOT, TunnelKey, band, chamber_per_row, pack, rows_height,
};

/// Ant slots a room is dug for beyond its ants, when the view has the room.
pub const SPARE_SLOTS: usize = 2;
/// How long a room stands empty before it is filled in.
pub const GRACE_MS: i64 = 120_000;
/// Biggest points per texel: a near-empty farm draws no giant ants.
pub const MAX_PX: f32 = 4.5;
/// Smallest points per texel, which a farm too big for the view still gets.
const MIN_PX: f32 = 1.0;
/// Steps between scales: halves are whole pixels on a Retina screen.
const PX_STEP: f32 = 0.5;
/// Most levels of rooms stacked under the gallery.
const MAX_LEVELS: usize = 3;
/// Height over each level of rooms for their labels and progress bars, in cells.
pub const LABEL_GAP: f32 = 1.6;

/// One room in the grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Room {
    pub key: TunnelKey,
    pub level: usize,
    pub col: usize,
    /// Rows of ants it is dug for.
    pub rows: usize,
    /// Since when it has had no ants (ms since the epoch).
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
    /// A room dug deeper or emptied; the others kept their cells.
    Grew,
    /// The grid was laid out afresh.
    Rearranged,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoomPlan {
    /// Named apart from the earlier plans' `grid`, so a plan saved by them is laid out afresh.
    fitted: Option<Grid>,
    pub rooms: Vec<Room>,
    /// Ants the surface band is sized for: it grows with them and shrinks on a rearrange.
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
    pub levels: usize,
    /// Left edge of each column, from the view's left.
    lefts: Vec<f32>,
    widths: Vec<f32>,
    depths: Vec<usize>,
    /// Height of each level's band under the gallery.
    pub band_h: f32,
    /// From the view's top to the top of the first level's band.
    pub below_top: f32,
}

impl Ground {
    pub fn new(size: Vec2, grid: &Grid, crowd: Crowd) -> Self {
        let a = CELL * grid.px;
        let below_top = band(size, a, crowd).gallery_top(a) + (1.2 + LABEL_GAP) * a;
        let band_h = band_height(size.y, below_top, a, grid.levels);
        let lefts = grid
            .widths
            .iter()
            .scan(0.0, |x, w| {
                let left = *x;
                *x += w;
                Some(left)
            })
            .collect();
        Self {
            a,
            px: grid.px,
            levels: grid.levels,
            lefts,
            widths: grid.widths.clone(),
            depths: grid.depths.clone(),
            band_h,
            below_top,
        }
    }

    /// Shafts to deeper rooms passing on the right of the room at `(level, col)`.
    fn gaps(&self, level: usize, col: usize) -> usize {
        self.depths
            .get(col)
            .map_or(0, |d| d.saturating_sub(level + 1))
    }

    /// Left edge of the chambers in column `col`, from the view's left.
    pub fn chamber_left(&self, col: usize) -> f32 {
        self.lefts.get(col).copied().unwrap_or_default() + 0.3 * self.a
    }

    /// Width of the chamber at `(level, col)`: its column less a margin and room on its right
    /// for the shafts down to the chambers under it.
    pub fn chamber_w(&self, level: usize, col: usize) -> f32 {
        let width = self.widths.get(col).copied().unwrap_or_default();
        let gaps = self.gaps(level, col) as f32 * SHAFT_GAP * self.a;
        (width - 0.6 * self.a - gaps).max(SLOT * self.a)
    }

    /// Where the shaft down to the chamber at `(level, col)` runs, from the view's left: an
    /// upper chamber's middle, else beside the chambers over it.
    pub fn shaft_x(&self, level: usize, col: usize) -> f32 {
        let left = self.chamber_left(col);
        let width = self.chamber_w(level, col);
        if level == 0 {
            left + width / 2.0
        } else {
            left + width - SHAFT_GAP * self.a / 2.0
        }
    }

    pub fn per_row(&self, level: usize, col: usize) -> usize {
        chamber_per_row(self.chamber_w(level, col), self.a)
    }

    /// The most rows a level's band has the height for.
    pub fn max_rows(&self) -> usize {
        max_rows(self.band_h, self.a)
    }
}

/// Height of each of `levels` bands of rooms between `below_top` and the view's bottom.
fn band_height(height: f32, below_top: f32, a: f32, levels: usize) -> f32 {
    let below = (height - 0.3 * a) - below_top;
    (below - (levels - 1) as f32 * LABEL_GAP * a) / levels as f32
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

/// Width of a chamber holding `per_row` ants side by side.
fn chamber_width(per_row: usize, a: f32) -> f32 {
    ((per_row - 1) as f32 * SLOT + 2.0).max(SLOT) * a
}

/// The grid fitted to `size` at `px` for `rooms` (each room's footprints), if every room
/// holds its ants and `spare` more in at most as many rows as its level has the height for,
/// and when `packed` side by side (rows outnumbering the ants to a row by one at most);
/// fewest levels first.
/// Columns are as wide as their widest room needs, the width left over shared out in
/// proportion.
fn fit(
    size: Vec2,
    crowd: Crowd,
    rooms: &[&[usize]],
    (px, spare, packed): (f32, usize, bool),
) -> Option<Grid> {
    let a = CELL * px;
    if crowd.envoy && px > (size.x / (18.0 * CELL) / PX_STEP).floor().max(1.0) * PX_STEP {
        return None;
    }
    let below_top = band(size, a, crowd).gallery_top(a) + (1.2 + LABEL_GAP) * a;
    let n = rooms.len().max(1);
    (1..=MAX_LEVELS.min(n)).find_map(|levels| {
        let max_rows = max_rows(band_height(size.y, below_top, a, levels), a);
        if max_rows == 0 {
            return None;
        }
        let cols = n.div_ceil(levels);
        let depths: Vec<usize> = (0..cols).map(|c| (n - c * levels).min(levels)).collect();
        let mut widths = vec![0.0f32; cols];
        for (i, footprints) in rooms.iter().enumerate() {
            let (level, col) = (i % levels, i / levels);
            let slots = (footprints.iter().sum::<usize>() + spare).max(1);
            let per_row = (1..=slots).find(|&p| {
                let rows = rows_for(footprints, spare, p);
                rows <= max_rows && (!packed || rows <= p + 1)
            })?;
            let gaps = depths[col] - level - 1;
            let width = chamber_width(per_row, a) + 0.6 * a + gaps as f32 * SHAFT_GAP * a;
            widths[col] = widths[col].max(width);
        }
        let total: f32 = widths.iter().sum();
        (total <= size.x).then(|| Grid {
            px,
            levels,
            widths: widths.iter().map(|w| w * size.x / total).collect(),
            depths,
        })
    })
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
        let consistent = grid.widths.len() == grid.depths.len()
            && self.rooms.iter().all(|r| r.col < grid.widths.len());
        if filled_in || new_room || resized || !consistent {
            return self.rearrange(demand, size);
        }
        // More ants on the landing or in storage: fine while the band keeps its height.
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
        let max_rows = ground.max_rows();
        for room in &mut self.rooms {
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
        // The biggest scale with a spare slot at least, chambers packed side by side if it
        // costs no scale, a second spare slot if that costs nothing either.
        let tries = |spares: &'static [usize]| {
            scales().flat_map(move |px| {
                [true, false]
                    .into_iter()
                    .flat_map(move |packed| spares.iter().map(move |&spare| (px, spare, packed)))
            })
        };
        let grid = tries(&[2, 1])
            .chain(tries(&[0]))
            .find_map(|t| fit(size, crowd, &footprints, t))
            .unwrap_or_else(|| {
                // Too many rooms for the view: the smallest scale, rooms squeezed in.
                let levels = MAX_LEVELS.min(keys.len().max(1));
                let cols = keys.len().max(1).div_ceil(levels);
                Grid {
                    px: MIN_PX,
                    levels,
                    widths: vec![size.x / cols as f32; cols],
                    depths: (0..cols)
                        .map(|c| (keys.len().max(1) - c * levels).min(levels))
                        .collect(),
                }
            });
        let ground = Ground::new(size, &grid, crowd);
        let max_rows = ground.max_rows();
        let levels = grid.levels;
        self.rooms = keys
            .into_iter()
            .enumerate()
            .map(|(i, (key, footprints, empty_since_ms))| {
                let (level, col) = (i % levels, i / levels);
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

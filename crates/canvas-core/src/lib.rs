//! Pure-logic core for the infinite canvas.
//!
//! No GPU, no windowing, no I/O, no clock. Everything here is deterministic
//! and unit-testable, which is deliberate: viewport culling, LOD selection and
//! eviction bookkeeping are where the real correctness risk lives, and none of
//! it needs a graphics device to verify.

use std::collections::HashMap;
use std::ops::{Add, Sub};

/// Base edge length in texels for a generated thumbnail pyramid.
pub const BASE_MIP: u32 = 256;
/// 256, 128, 64, 32, 16, 8, 4
pub const MIP_LEVELS: u32 = 7;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };

    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

impl Add for Vec2 {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self::new(self.x + rhs.x, self.y + rhs.y)
    }
}

impl Sub for Vec2 {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self::new(self.x - rhs.x, self.y - rhs.y)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub min: Vec2,
    pub max: Vec2,
}

impl Rect {
    pub const fn new(min: Vec2, max: Vec2) -> Self {
        Self { min, max }
    }

    pub fn from_center_half(center: Vec2, half: Vec2) -> Self {
        Self::new(
            Vec2::new(center.x - half.x, center.y - half.y),
            Vec2::new(center.x + half.x, center.y + half.y),
        )
    }

    pub fn center(&self) -> Vec2 {
        Vec2::new(
            (self.min.x + self.max.x) * 0.5,
            (self.min.y + self.max.y) * 0.5,
        )
    }

    pub fn intersects(&self, other: &Self) -> bool {
        self.min.x < other.max.x
            && other.min.x < self.max.x
            && self.min.y < other.max.y
            && other.min.y < self.max.y
    }

    pub fn union(&self, other: &Self) -> Self {
        Self::new(
            Vec2::new(self.min.x.min(other.min.x), self.min.y.min(other.min.y)),
            Vec2::new(self.max.x.max(other.max.x), self.max.y.max(other.max.y)),
        )
    }

    pub fn expand(&self, margin: f32) -> Self {
        Self::new(
            Vec2::new(self.min.x - margin, self.min.y - margin),
            Vec2::new(self.max.x + margin, self.max.y + margin),
        )
    }
}

/// Camera state. `scale` is device pixels per world unit.
#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    pub center: Vec2,
    pub scale: f32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            center: Vec2::ZERO,
            scale: 1.0,
        }
    }
}

impl Viewport {
    /// World-space rectangle visible through a viewport `size` device pixels wide.
    pub fn visible_rect(&self, size: Vec2) -> Rect {
        let half = Vec2::new(size.x * 0.5 / self.scale, size.y * 0.5 / self.scale);
        Rect::from_center_half(self.center, half)
    }

    pub fn world_to_screen(&self, p: Vec2, size: Vec2) -> Vec2 {
        Vec2::new(
            (p.x - self.center.x) * self.scale + size.x * 0.5,
            (p.y - self.center.y) * self.scale + size.y * 0.5,
        )
    }

    pub fn screen_to_world(&self, p: Vec2, size: Vec2) -> Vec2 {
        Vec2::new(
            (p.x - size.x * 0.5) / self.scale + self.center.x,
            (p.y - size.y * 0.5) / self.scale + self.center.y,
        )
    }
}

/// One reference on the canvas, in world units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Item {
    pub id: u32,
    /// World position of the top-left corner.
    pub pos: Vec2,
    /// World size.
    pub size: Vec2,
    /// Atlas texture key, shared by items that came from the same source image.
    pub tex: u32,
    pub z: i32,
}

impl Item {
    pub fn bounds(&self) -> Rect {
        Rect::new(
            self.pos,
            Vec2::new(self.pos.x + self.size.x, self.pos.y + self.size.y),
        )
    }
}

/// An item that survived culling, resolved to device pixels and a mip level.
#[derive(Clone, Copy, Debug)]
pub struct VisibleItem {
    pub id: u32,
    pub tex: u32,
    pub z: i32,
    /// Top-left in device pixels, relative to the canvas viewport origin.
    pub screen_pos: Vec2,
    /// Size in device pixels.
    pub screen_size: Vec2,
    /// Which level of the thumbnail pyramid to sample.
    pub mip: u32,
}

/// Pick the coarsest mip that still resolves to roughly 1:1 in device pixels.
pub fn mip_for_screen_size(screen_px: f32) -> u32 {
    if screen_px <= 0.0 {
        return MIP_LEVELS - 1;
    }
    let level = (BASE_MIP as f32 / screen_px.max(1.0)).log2().floor();
    (level.max(0.0) as u32).min(MIP_LEVELS - 1)
}

/// Uniform spatial hash over world space.
///
/// A dense grid is wrong here: an infinite canvas has no bounds to size it to,
/// so cells are hashed instead. Query dedup uses a generation-stamped scratch
/// buffer rather than a HashSet, because the same item is reachable from every
/// cell it overlaps and we do this every frame.
pub struct SpatialGrid {
    cell: f32,
    cells: HashMap<(i32, i32), Vec<u32>>,
    items: Vec<Item>,
    slot_of: HashMap<u32, usize>,
    seen: Vec<u32>,
    gen: u32,
}

impl SpatialGrid {
    pub fn new(cell_size: f32) -> Self {
        assert!(cell_size > 0.0, "cell size must be positive");
        Self {
            cell: cell_size,
            cells: HashMap::new(),
            items: Vec::new(),
            slot_of: HashMap::new(),
            seen: Vec::new(),
            gen: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn clear(&mut self) {
        self.cells.clear();
        self.items.clear();
        self.slot_of.clear();
        self.seen.clear();
    }

    pub fn item(&self, id: u32) -> Option<&Item> {
        self.slot_of.get(&id).map(|&slot| &self.items[slot])
    }

    pub fn insert(&mut self, item: Item) {
        let slot = match self.slot_of.get(&item.id) {
            Some(&slot) => slot,
            None => {
                let slot = self.items.len();
                self.items.push(item);
                self.slot_of.insert(item.id, slot);
                self.seen.push(0);
                slot
            }
        };
        // Remove the item's previous cell membership before re-inserting.
        if self.items[slot] != item {
            self.unindex_slot(slot);
            self.items[slot] = item;
        }
        let r = self.items[slot].bounds();
        let (x0, y0) = self.cell_of(r.min);
        let (x1, y1) = self.cell_of(r.max);
        for cy in y0..=y1 {
            for cx in x0..=x1 {
                self.cells.entry((cx, cy)).or_default().push(slot as u32);
            }
        }
    }

    fn unindex_slot(&mut self, slot: usize) {
        let r = self.items[slot].bounds();
        let (x0, y0) = self.cell_of(r.min);
        let (x1, y1) = self.cell_of(r.max);
        for cy in y0..=y1 {
            for cx in x0..=x1 {
                if let Some(bucket) = self.cells.get_mut(&(cx, cy)) {
                    bucket.retain(|&s| s != slot as u32);
                }
            }
        }
    }

    fn cell_of(&self, p: Vec2) -> (i32, i32) {
        (
            (p.x / self.cell).floor() as i32,
            (p.y / self.cell).floor() as i32,
        )
    }

    /// Items whose bounds intersect `query`, in stable insertion order.
    pub fn query(&mut self, query: &Rect) -> Vec<&Item> {
        self.gen = self.gen.wrapping_add(1);
        if self.gen == 0 {
            self.seen.iter_mut().for_each(|s| *s = 0);
            self.gen = 1;
        }
        let gen = self.gen;
        let (x0, y0) = self.cell_of(query.min);
        let (x1, y1) = self.cell_of(query.max);

        let mut hits = Vec::new();
        for cy in y0..=y1 {
            for cx in x0..=x1 {
                let Some(bucket) = self.cells.get(&(cx, cy)) else {
                    continue;
                };
                for &slot in bucket {
                    let slot = slot as usize;
                    if self.seen[slot] == gen {
                        continue;
                    }
                    self.seen[slot] = gen;
                    let item = &self.items[slot];
                    if item.bounds().intersects(query) {
                        hits.push(item);
                    }
                }
            }
        }
        hits.sort_unstable_by_key(|i| i.id);
        hits
    }
}

/// Cull to the viewport and resolve each survivor to device pixels plus a mip.
///
/// `margin_px` keeps items slightly outside the viewport resident so that a small
/// pan does not immediately trigger an upload for something that is about to
/// scroll in.
pub fn cull(
    grid: &mut SpatialGrid,
    viewport: &Viewport,
    size: Vec2,
    margin_px: f32,
) -> Vec<VisibleItem> {
    let world_rect = viewport
        .visible_rect(size)
        .expand(margin_px / viewport.scale);
    let mut out = Vec::new();
    for item in grid.query(&world_rect) {
        let top_left = viewport.world_to_screen(item.pos, size);
        let screen_size = Vec2::new(item.size.x * viewport.scale, item.size.y * viewport.scale);
        out.push(VisibleItem {
            id: item.id,
            tex: item.tex,
            z: item.z,
            screen_pos: top_left,
            screen_size,
            mip: mip_for_screen_size(screen_size.x.max(screen_size.y)),
        });
    }
    out.sort_unstable_by_key(|v| (v.z, v.id));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: u32, x: f32, y: f32) -> Item {
        Item {
            id,
            pos: Vec2::new(x, y),
            size: Vec2::new(100.0, 100.0),
            tex: id % 8,
            z: 0,
        }
    }

    #[test]
    fn viewport_roundtrips_through_screen_space() {
        let view = Viewport {
            center: Vec2::new(500.0, -250.0),
            scale: 2.5,
        };
        let size = Vec2::new(1600.0, 900.0);
        let world = Vec2::new(123.0, 456.0);
        let screen = view.world_to_screen(world, size);
        let back = view.screen_to_world(screen, size);
        assert!((back.x - world.x).abs() < 1e-3, "x drift");
        assert!((back.y - world.y).abs() < 1e-3, "y drift");
    }

    #[test]
    fn visible_rect_matches_screen_extents() {
        let view = Viewport {
            center: Vec2::ZERO,
            scale: 2.0,
        };
        let size = Vec2::new(800.0, 600.0);
        let r = view.visible_rect(size);
        // 800px at 2 px/unit is 400 world units, half of that is 200.
        assert!((r.max.x - 200.0).abs() < 1e-4);
        assert!((r.max.y - 150.0).abs() < 1e-4);
    }

    #[test]
    fn grid_finds_every_item_once_despite_cell_overlap() {
        let mut grid = SpatialGrid::new(64.0);
        for i in 0..500 {
            // Spans far wider than one cell, so every item lands in many cells.
            grid.insert(Item {
                id: i,
                pos: Vec2::new(i as f32 * 37.0, i as f32 * 11.0),
                size: Vec2::new(200.0, 150.0),
                tex: i % 8,
                z: 0,
            });
        }
        let hits = grid.query(&Rect::new(Vec2::ZERO, Vec2::new(100_000.0, 100_000.0)));
        assert_eq!(hits.len(), 500, "every item should appear exactly once");
    }

    #[test]
    fn query_excludes_items_outside_the_rect() {
        let mut grid = SpatialGrid::new(64.0);
        grid.insert(item(1, 0.0, 0.0));
        grid.insert(item(2, 5_000.0, 5_000.0));
        let hits = grid.query(&Rect::new(Vec2::ZERO, Vec2::new(200.0, 200.0)));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, 1);
    }

    #[test]
    fn touching_edges_do_not_count_as_intersecting() {
        // Half-open intervals, so an item flush against the right edge of the
        // viewport is not visible until it actually crosses it.
        let a = Rect::new(Vec2::ZERO, Vec2::new(10.0, 10.0));
        let b = Rect::new(Vec2::new(10.0, 0.0), Vec2::new(20.0, 10.0));
        assert!(!a.intersects(&b));
        let c = Rect::new(Vec2::new(9.99, 0.0), Vec2::new(20.0, 10.0));
        assert!(a.intersects(&c));
    }

    #[test]
    fn reinsert_updates_position_without_duplicating() {
        let mut grid = SpatialGrid::new(64.0);
        grid.insert(item(1, 0.0, 0.0));
        grid.insert(item(1, 9_000.0, 9_000.0));
        assert_eq!(grid.len(), 1);
        let hits = grid.query(&Rect::new(
            Vec2::new(8_900.0, 8_900.0),
            Vec2::new(9_500.0, 9_500.0),
        ));
        assert_eq!(
            hits.len(),
            1,
            "item should only be findable at its new position"
        );
        let old = grid.query(&Rect::new(
            Vec2::new(-200.0, -200.0),
            Vec2::new(200.0, 200.0),
        ));
        assert_eq!(old.len(), 0, "stale cell membership must be removed");
    }

    #[test]
    fn mip_gets_coarser_as_items_shrink() {
        assert_eq!(mip_for_screen_size(256.0), 0, "1:1 samples the base level");
        assert_eq!(mip_for_screen_size(128.0), 1);
        assert_eq!(mip_for_screen_size(4.0), 6, "clamps to the smallest level");
        assert_eq!(
            mip_for_screen_size(100_000.0),
            0,
            "clamps to the largest level"
        );
        assert_eq!(
            mip_for_screen_size(0.0),
            MIP_LEVELS - 1,
            "degenerate size is safe"
        );
    }

    #[test]
    fn cull_drops_items_outside_the_viewport_and_keeps_the_rest() {
        let mut grid = SpatialGrid::new(64.0);
        for i in 0..200 {
            grid.insert(Item {
                id: i,
                pos: Vec2::new(i as f32 * 120.0, 0.0),
                size: Vec2::new(100.0, 100.0),
                tex: i % 8,
                z: 0,
            });
        }
        let view = Viewport {
            center: Vec2::new(1_000.0, 0.0),
            scale: 1.0,
        };
        let visible = cull(&mut grid, &view, Vec2::new(800.0, 600.0), 0.0);
        assert!(!visible.is_empty());
        // Far-right items must be culled, and nothing outside the view may appear.
        let world_rect = view.visible_rect(Vec2::new(800.0, 600.0));
        for v in &visible {
            let i = grid.item(v.id).expect("culled item must exist in the grid");
            assert!(
                i.bounds().intersects(&world_rect),
                "item {} escaped culling",
                v.id
            );
        }
    }

    #[test]
    fn cull_output_is_ordered_by_z_then_id() {
        let mut grid = SpatialGrid::new(1_000.0);
        grid.insert(Item {
            z: 5,
            ..item(1, 0.0, 0.0)
        });
        grid.insert(Item {
            z: 1,
            ..item(2, 10.0, 0.0)
        });
        grid.insert(Item {
            z: 1,
            ..item(3, 20.0, 0.0)
        });
        let view = Viewport::default();
        let visible = cull(&mut grid, &view, Vec2::new(800.0, 600.0), 0.0);
        let order: Vec<u32> = visible.iter().map(|v| v.id).collect();
        assert_eq!(order, vec![2, 3, 1], "z first, then id for stability");
    }

    #[test]
    fn margin_keeps_nearby_items_resident() {
        let mut grid = SpatialGrid::new(64.0);
        // An 800px-wide viewport at scale 1.0 spans world x -400..400, so an
        // item starting at 450 sits just past the right edge.
        grid.insert(item(1, 450.0, 0.0));
        let view = Viewport::default();
        let tight = cull(&mut grid, &view, Vec2::new(800.0, 600.0), 0.0);
        let loose = cull(&mut grid, &view, Vec2::new(800.0, 600.0), 128.0);
        assert!(tight.is_empty(), "outside the viewport with no margin");
        assert_eq!(loose.len(), 1, "margin should keep it resident");
    }
}

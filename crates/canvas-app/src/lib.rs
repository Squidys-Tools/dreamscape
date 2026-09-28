//! Shared application layer for the canvas spike.
//!
//! All three UI hosts link this, so the only thing that differs between them is
//! how the chrome is drawn. Scene, textures, camera, selection, search and the
//! per-frame pipeline are identical, which is what makes their frame times
//! comparable.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use canvas_core::{Item, SpatialGrid, Vec2, Viewport};
use canvas_gpu::GpuCanvas;

/// A reproducible little PRNG, so a run is repeatable without a dependency.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(6364136223846793005).wrapping_add(1))
    }
    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.0 as u32;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x as u64;
        x
    }
    pub fn f32(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / (1 << 24) as f32
    }
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + self.f32() * (hi - lo)
    }
}

/// One generated source image and its mip pyramid.
pub struct SourceTexture {
    /// `levels[0]` is full resolution, then successively halved.
    pub levels: Vec<Vec<u8>>,
}

/// A distinct, deterministic test image with enough high-frequency detail that
/// mip levels visibly do work when sampled.
pub fn make_texture(seed: u32) -> SourceTexture {
    let mut rng = Rng::new(seed as u64 * 0x9E37_79B9_7F4A_7C15);
    let base = canvas_core::BASE_MIP;
    let blobs: [(f32, f32, f32, [u8; 3]); 5] = std::array::from_fn(|_| {
        (
            rng.range(0.15, 0.85),
            rng.range(0.15, 0.85),
            rng.range(0.08, 0.30),
            [
                (rng.range(0.2, 1.0) * 255.0) as u8,
                (rng.range(0.2, 1.0) * 255.0) as u8,
                (rng.range(0.2, 1.0) * 255.0) as u8,
            ],
        )
    });

    let mut levels = Vec::new();
    let mut side = base;
    let mut src = vec![0u8; (base * base * 4) as usize];
    for y in 0..base {
        for x in 0..base {
            let (fx, fy) = (x as f32 / base as f32, y as f32 / base as f32);
            let (mut r, mut g, mut b) = (18.0f32, 18.0f32, 22.0f32);
            for (cx, cy, r0, col) in blobs {
                let d = (((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt() / r0).min(1.0);
                let w = (1.0 - d) * (1.0 - d);
                r += col[0] as f32 * w;
                g += col[1] as f32 * w;
                b += col[2] as f32 * w;
            }
            // Fine grain so downsampling does real filtering work.
            let grain = ((x * 7 + y * 13 + seed.wrapping_mul(31)) % 17) as f32 * 1.4;
            let i = ((y * base + x) * 4) as usize;
            src[i] = (r + grain).clamp(0.0, 255.0) as u8;
            src[i + 1] = (g + grain).clamp(0.0, 255.0) as u8;
            src[i + 2] = (b + grain).clamp(0.0, 255.0) as u8;
            src[i + 3] = 255;
        }
    }
    levels.push(src);

    // Box-filter downsample. Production generates these on the GPU at ingest and
    // reads them back from the thumbnail cache; doing it here keeps the harness
    // asset-free and it happens once, outside any measured frame.
    while side > 1 {
        let next = ((side / 2) as usize).max(1);
        let src_side = side as usize;
        let mut dst = vec![0u8; next * next * 4];
        let prev = levels.last().unwrap();
        for y in 0..next {
            for x in 0..next {
                for c in 0..4usize {
                    let s = (y * 2) * src_side + x * 2;
                    let a = prev[s * 4 + c] as u32;
                    let b = prev[(s + 1) * 4 + c] as u32;
                    let d = prev[(s + src_side) * 4 + c] as u32;
                    let e = prev[(s + src_side + 1) * 4 + c] as u32;
                    dst[(y * next + x) * 4 + c] = ((a + b + d + e) / 4) as u8;
                }
            }
        }
        levels.push(dst);
        side = (next as u32).max(1);
    }
    SourceTexture { levels }
}

/// Camera motion applied each frame while the animation is running.
#[derive(Clone, Copy, Debug)]
pub struct Motion {
    /// World units per frame on x.
    pub pan_x: f32,
    /// World units per frame on y.
    pub pan_y: f32,
    /// Scale multiplier applied across the whole run.
    pub zoom_sweep: f32,
}

impl Default for Motion {
    fn default() -> Self {
        Self {
            pan_x: 18.0,
            pan_y: 6.3,
            zoom_sweep: 0.7,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub items: u32,
    pub textures: u32,
    pub atlas: canvas_gpu::AtlasConfig,
    /// Half-extent of the world area items are scattered across, in world units.
    ///
    /// This is the knob that decides how dense a board is, and it is the one
    /// that was badly wrong before. At 12,000 the items spread over a
    /// 24,000-square area while a 2560x1440 viewport at `start_scale` covers
    /// about 7,300 of it, so only 5% of them were ever on screen and the
    /// benchmark was measuring a nearly empty board while reporting healthy
    /// frame times.
    ///
    /// At 1,000 every scenario is a full board, so the sweep answers the
    /// question that matters: at what item count does a full board break?
    /// Anything larger spreads the items out and quietly returns to measuring
    /// an empty one.
    pub extent: f32,
    pub seed: u64,
    pub start_scale: f32,
    pub motion: Motion,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            items: 2_000,
            textures: 2_000,
            atlas: canvas_gpu::AtlasConfig::default(),
            extent: 1_000.0,
            seed: 0x5EED,
            start_scale: 0.35,
            motion: Motion::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FrameStats {
    /// CPU work: cull, residency, encode, submit.
    pub cpu: Duration,
    /// Visible items surviving culling.
    pub visible: u32,
    /// Items actually drawn.
    pub drawn: u32,
    /// Items drawn as a placeholder because no level could be placed.
    pub placeholder: u32,
    /// Texture level uploads this frame.
    pub uploads: u32,
    pub evictions: u64,
}

#[derive(Default)]
pub struct Metrics {
    pub samples: Vec<FrameStats>,
    pub peak_visible: u32,
}

impl Metrics {
    pub fn push(&mut self, s: FrameStats) {
        self.peak_visible = self.peak_visible.max(s.visible);
        self.samples.push(s);
    }
    pub fn clear(&mut self) {
        self.samples.clear();
    }
    pub fn len(&self) -> usize {
        self.samples.len()
    }
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn percentiles(&self, budget_ms: f64) -> (f64, f64, f64, f64, usize) {
        if self.samples.is_empty() {
            return (0.0, 0.0, 0.0, 0.0, 0);
        }
        let mut v: Vec<f64> = self
            .samples
            .iter()
            .map(|s| s.cpu.as_secs_f64() * 1000.0)
            .collect();
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let at = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
        let over = v.iter().filter(|&&x| x > budget_ms).count();
        (mean, at(0.5), at(0.99), *v.last().unwrap(), over)
    }

    pub fn total_placeholder(&self) -> u32 {
        self.samples.iter().map(|s| s.placeholder).sum()
    }

    pub fn total_visible(&self) -> u32 {
        self.samples.iter().map(|s| s.visible).sum()
    }
}

/// The whole simulated application. Hosts own this and drive it per frame.
pub struct AppState {
    pub grid: SpatialGrid,
    pub viewport: Viewport,
    pub sources: Vec<SourceTexture>,
    pub cfg: Config,
    pub selected: HashSet<u32>,
    pub search: String,
    pub motion: Motion,
    pub frame: u64,
    pub start_scale: f32,
    /// When false the camera does not advance, so a host can measure a still frame.
    pub animate: bool,
    /// Scroll offset in rows, used to exercise the sidebar's virtual list.
    pub list_scroll: u32,
}

impl AppState {
    pub fn new(cfg: Config) -> Self {
        let mut rng = Rng::new(cfg.seed);
        let mut grid = SpatialGrid::new(900.0);
        let clusters = ((cfg.items / 50).max(1)) as usize;

        for i in 0..cfg.items {
            let cluster = (i as usize) % clusters;
            let cx = (cluster as f32 / clusters as f32 - 0.5) * 2.0 * cfg.extent;
            let cy = (((cluster as f32) * 0.618034) % 1.0 - 0.5) * 2.0 * cfg.extent;
            let size = rng.range(140.0, 420.0);
            grid.insert(Item {
                id: i,
                pos: Vec2::new(cx + rng.range(-900.0, 900.0), cy + rng.range(-900.0, 900.0)),
                size: Vec2::new(size, size * rng.range(0.7, 1.4)),
                tex: i % cfg.textures.max(1),
                z: (i % 7) as i32,
            });
        }

        let sources = (0..cfg.textures.max(1))
            .map(|i| make_texture(i + 1))
            .collect();

        Self {
            grid,
            viewport: Viewport {
                center: Vec2::ZERO,
                scale: cfg.start_scale,
            },
            sources,
            start_scale: cfg.start_scale,
            cfg,
            selected: HashSet::new(),
            search: String::new(),
            motion: Motion::default(),
            frame: 0,
            animate: true,
            list_scroll: 0,
        }
    }

    /// Items whose id or texture matches the search query.
    pub fn search_hits(&self) -> Vec<u32> {
        if self.search.trim().is_empty() {
            return Vec::new();
        }
        let q = self.search.to_lowercase();
        (0..self.cfg.items)
            .filter(|&i| format!("ref-{:05}", i).contains(&q))
            .take(500)
            .collect()
    }

    fn advance(&mut self) {
        if !self.animate {
            return;
        }
        self.viewport.center.x += self.motion.pan_x;
        self.viewport.center.y += self.motion.pan_y;
    }

    /// Cull, ensure residency, and draw into `target`.
    ///
    /// This is the whole per-frame pipeline and it is identical for every host,
    /// so differences in frame time come from the host, not from here.
    pub fn draw_frame(
        &mut self,
        canvas: &mut GpuCanvas,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        size: Vec2,
    ) -> FrameStats {
        let t0 = Instant::now();
        self.advance();
        self.frame += 1;
        let t = (self.frame % 600) as f32 / 600.0;
        if self.animate {
            self.viewport.scale = self.start_scale * (1.0 - t * self.motion.zoom_sweep);
        }

        canvas.begin_frame();
        let visible: Vec<canvas_core::VisibleItem> =
            canvas_core::cull(&mut self.grid, &self.viewport, size, 192.0);

        canvas.atlas_mut().pin(&GpuCanvas::visible_keys(&visible));

        // Request a level only when the texture has no usable one yet. Re-requesting
        // a level that can never be placed re-uploads it every frame, which costs far
        // more than the upload itself.
        let mut wanted: Vec<(u32, u32)> = visible.iter().map(|v| (v.tex, v.mip)).collect();
        wanted.sort_unstable();
        wanted.dedup();
        for (key, level) in wanted {
            if canvas.atlas().resolve_level(key, level).is_some() {
                continue;
            }
            let src = &self.sources[key as usize % self.sources.len()];
            canvas
                .atlas_mut()
                .ensure_view(device, queue, key, level, &src.levels);
        }

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("app-frame"),
        });
        let drawn = canvas.draw(device, queue, &mut enc, target, &visible);
        queue.submit(std::iter::once(enc.finish()));

        let st = canvas.atlas().stats();
        FrameStats {
            cpu: t0.elapsed(),
            visible: visible.len() as u32,
            drawn,
            placeholder: canvas.last_frame_dropped,
            uploads: st.uploads_this_frame,
            evictions: st.evictions_total,
        }
    }
}

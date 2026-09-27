//! Test scaffolding for the canvas spike: procedural content, a deterministic
//! scene, and frame-time measurement.
//!
//! No asset files. Every texture is generated so the benchmark is reproducible
//! on any machine and the working set is known exactly.

use std::time::Duration;

use canvas_core::{Item, SpatialGrid, Vec2, Viewport};
use canvas_gpu::{AtlasConfig, GpuCanvas};

/// A reproducible little PRNG, so a run is repeatable without a dep.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(6364136223846793005).wrapping_add(1))
    }
    pub fn next_u32(&mut self) -> u32 {
        // xorshift32
        let mut x = self.0 as u32;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x as u64;
        x
    }
    /// Uniform in [0, 1).
    pub fn f32(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / (1 << 24) as f32
    }
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + self.f32() * (hi - lo)
    }
}

/// One generated source image and its mip pyramid.
pub struct SourceTexture {
    /// `levels[0]` is the full-resolution image, then successively halved.
    pub levels: Vec<Vec<u8>>,
}

/// A distinct, deterministic test image with enough high-frequency detail that
/// mip levels are visibly doing work.
pub fn make_texture(seed: u32) -> SourceTexture {
    let mut rng = Rng::new(seed as u64 * 0x9E37_79B9_7F4A_7C15);
    let base = canvas_core::BASE_MIP;
    // A handful of saturated blobs on a dark ground, plus fine grain.
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
            let mut r = 18.0f32;
            let mut g = 18.0f32;
            let mut b = 22.0f32;
            for (cx, cy, r0, col) in blobs {
                let d = (((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt() / r0).min(1.0);
                let w = (1.0 - d) * (1.0 - d);
                r += col[0] as f32 * w;
                g += col[1] as f32 * w;
                b += col[2] as f32 * w;
            }
            // Fine grain so downsampling is doing real filtering.
            let grain = ((x * 7 + y * 13 + seed * 31) % 17) as f32 * 1.4;
            let i = ((y * base + x) * 4) as usize;
            src[i] = (r + grain).clamp(0.0, 255.0) as u8;
            src[i + 1] = (g + grain).clamp(0.0, 255.0) as u8;
            src[i + 2] = (b + grain).clamp(0.0, 255.0) as u8;
            src[i + 3] = 255;
        }
    }
    levels.push(src);

    // Box-filter downsample. Production generates these on the GPU at ingest;
    // CPU generation here keeps the harness dependency-free and happens once.
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

/// A board laid out in loose clusters, the way a real moodboard is.
pub struct Scene {
    pub grid: SpatialGrid,
    pub viewport: Viewport,
}

pub fn make_scene(items: u32, textures: u32, cell: f32, extent: f32, seed: u64) -> Scene {
    let mut rng = Rng::new(seed);
    let mut grid = SpatialGrid::new(cell);
    let clusters = ((items / 50).max(1)) as usize;

    for i in 0..items {
        let cluster = (i as usize) % clusters;
        let cx = (cluster as f32 / clusters as f32 - 0.5) * 2.0 * extent;
        let cy = (((cluster as f32) * 0.618034) % 1.0 - 0.5) * 2.0 * extent;
        let size = rng.range(140.0, 420.0);
        let pos = Vec2::new(
            cx + rng.range(-900.0, 900.0),
            cy + rng.range(-900.0, 900.0),
        );
        grid.insert(Item {
            id: i,
            pos,
            size: Vec2::new(size, size * rng.range(0.7, 1.4)),
            tex: i % textures,
            z: (i % 7) as i32,
        });
    }
    Scene {
        grid,
        viewport: Viewport {
            center: Vec2::ZERO,
            scale: 0.35,
        },
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FrameSample {
    /// CPU work: cull, residency, encode, submit.
    pub submit: Duration,
    /// CPU work plus a hard GPU wait. Pessimistic against a pipelined app.
    pub serial: Duration,
    pub drawn: u32,
    pub visible: u32,
    /// Visible items drawn as a placeholder because their atlas slot was unavailable.
    pub placeholder: u32,
    pub uploads: u32,
    pub evictions: u64,
}

#[derive(Default)]
pub struct Metrics {
    samples: Vec<FrameSample>,
}

impl Metrics {
    pub fn push(&mut self, s: FrameSample) {
        self.samples.push(s);
    }
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    fn percentile_ms(values: &[f64], p: f64) -> f64 {
        if values.is_empty() {
            return 0.0;
        }
        let mut v = values.to_vec();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let idx = ((v.len() - 1) as f64 * p).round() as usize;
        v[idx]
    }

    pub fn report(&self, budget_ms: f64) -> String {
        let submit: Vec<f64> = self
            .samples
            .iter()
            .map(|s| s.submit.as_secs_f64() * 1000.0)
            .collect();
        let serial: Vec<f64> = self
            .samples
            .iter()
            .map(|s| s.serial.as_secs_f64() * 1000.0)
            .collect();

        let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;
        let over = |v: &[f64]| v.iter().filter(|&&x| x > budget_ms).count();
        let pct = |v: &[f64], p: f64| Self::percentile_ms(v, p);
        let last = self.samples.last();

        let mut out = String::new();
        out.push_str(&format!("frames            {}\n", self.samples.len()));
        out.push_str(&format!(
            "submit ms         mean {:6.2}  p50 {:6.2}  p90 {:6.2}  p99 {:6.2}  max {:6.2}\n",
            mean(&submit),
            pct(&submit, 0.50),
            pct(&submit, 0.90),
            pct(&submit, 0.99),
            pct(&submit, 1.0),
        ));
        out.push_str(&format!(
            "serial ms          mean {:6.2}  p50 {:6.2}  p90 {:6.2}  p99 {:6.2}  max {:6.2}\n",
            mean(&serial),
            pct(&serial, 0.50),
            pct(&serial, 0.90),
            pct(&serial, 0.99),
            pct(&serial, 1.0),
        ));
        out.push_str(&format!(
            "over budget        {:>5} / {}  ({:.1}%)\n",
            over(&serial),
            self.samples.len(),
            100.0 * over(&serial) as f64 / self.samples.len().max(1) as f64
        ));
        if let Some(s) = last {
            out.push_str(&format!(
                "last frame         visible {}  drawn {}  placeholder {}  uploads {}  evictions {}\n",
                s.visible, s.drawn, s.placeholder, s.uploads, s.evictions
            ));
        }
        let ph: u32 = self.samples.iter().map(|s| s.placeholder).sum();
        let vis: u32 = self.samples.iter().map(|s| s.visible).sum();
        out.push_str(&format!(
            "placeholder frames {ph} of {vis} item-frames ({:.2}%)\n",
            100.0 * ph as f64 / vis.max(1) as f64
        ));
        out.push_str(
            "  evictions must never remove a pinned texture; a board that shows\n  \
             grey squares instead of a texture means the atlas is undersized for\n  \
             the working set, not that eviction is broken.\n",
        );
        out
    }
}

pub struct RunConfig {
    pub items: u32,
    pub textures: u32,
    pub atlas: AtlasConfig,
    pub width: u32,
    pub height: u32,
    pub warmup: u32,
    pub frames: u32,
    /// Pan velocity in world units per frame.
    pub pan_per_frame: f32,
    /// Zoom applied across the run, as a multiplier.
    pub zoom_sweep: f32,
    pub seed: u64,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            items: 2_000,
            textures: 64,
            atlas: AtlasConfig::default(),
            width: 1600,
            height: 900,
            warmup: 120,
            frames: 400,
            pan_per_frame: 26.0,
            zoom_sweep: 0.6,
            seed: 0x5EED,
        }
    }
}

pub struct RunReport {
    pub metrics: Metrics,
    pub adapter: wgpu::AdapterInfo,
    pub final_viewport: Viewport,
    pub peak_visible: u32,
}

/// Drive the renderer for `warmup + frames` frames and measure.
///
/// Records two numbers per frame. `submit` is CPU work only. `serial` adds a
/// hard `device.poll(Wait)`, which serialises CPU and GPU and is therefore
/// pessimistic against a real app that runs ahead. If `submit` is comfortably
/// inside budget while `serial` is not, the GPU is the bottleneck and the fix
/// is fewer pixels or smaller mips, not less culling work.
pub fn run(cfg: &RunConfig) -> RunReport {
    let (device, queue, format, adapter) = canvas_gpu::headless_device();

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bench-target"),
        size: wgpu::Extent3d {
            width: cfg.width,
            height: cfg.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

    // Deliberately smaller than the resident working set, so eviction and
    // re-upload are exercised rather than measured once and forgotten.
    let mut canvas = GpuCanvas::new(&device, &queue, format, cfg.atlas, cfg.items + 64);
    let mut scene = make_scene(cfg.items, cfg.textures, 900.0, 12_000.0, cfg.seed);

    let sources: Vec<SourceTexture> = (0..cfg.textures)
        .map(|i| make_texture(i as u32 + 1))
        .collect();

    let size = Vec2::new(cfg.width as f32, cfg.height as f32);
    let total = cfg.warmup + cfg.frames;
    let mut metrics = Metrics::default();
    let mut peak_visible = 0u32;
    let start_scale = scene.viewport.scale;

    for frame in 0..total {
        let t = frame as f32 / total as f32;
        // Pan steadily and zoom across the run, so LOD changes and mip
        // residency churn are both in the measurement.
        scene.viewport.center.x += cfg.pan_per_frame;
        scene.viewport.center.y += cfg.pan_per_frame * 0.35;
        scene.viewport.scale = start_scale * (1.0 - t * cfg.zoom_sweep);

        let t0 = std::time::Instant::now();

        let visible = canvas_core::cull(&mut scene.grid, &scene.viewport, size, 192.0);
        let keys = GpuCanvas::missing_textures(&visible);

        // Pin before ingesting, not after. Otherwise ingesting one newly visible
        // texture can evict another that is already on screen this frame but has
        // not been visited by the loop yet, which shows up as a hole in the board.
        canvas.atlas_mut().pin(&keys);

        for key in &keys {
            canvas.atlas_mut().ensure_resident(
                &device,
                &queue,
                *key,
                &sources[*key as usize % sources.len()].levels,
            );
        }

        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        let drawn = canvas.draw(
            &device,
            &queue,
            &mut enc,
            &target_view,
            &visible,
        );
        queue.submit(std::iter::once(enc.finish()));
        let submit = t0.elapsed();

        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        let serial = t0.elapsed();

        peak_visible = peak_visible.max(visible.len() as u32);
        if frame >= cfg.warmup {
            let st = canvas.atlas().stats();
            metrics.push(FrameSample {
                submit,
                serial,
                drawn,
                visible: visible.len() as u32,
                placeholder: canvas.last_frame_dropped,
                uploads: st.uploads_this_frame,
                evictions: st.evictions_total,
            });
        }
    }

    RunReport {
        metrics,
        adapter,
        final_viewport: scene.viewport,
        peak_visible,
    }
}

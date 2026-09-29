//! Headless benchmark driver.
//!
//! Runs the shared `AppState` with no UI attached, so the canvas baseline is
//! known independently of whichever host it is later embedded in.

use std::time::Instant;

use canvas_app::{AppState, Config, Metrics};
use canvas_gpu::GpuCanvas;

pub use canvas_app::{make_texture, Motion, Rng, SourceTexture};

pub struct RunConfig {
    pub app: Config,
    pub width: u32,
    pub height: u32,
    pub warmup: u32,
    pub frames: u32,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            app: Config::default(),
            width: 2560,
            height: 1440,
            warmup: 120,
            frames: 200,
        }
    }
}

pub struct RunReport {
    pub metrics: Metrics,
    pub adapter: wgpu::AdapterInfo,
}

/// Drive the renderer and measure.
///
/// The measured frame is the `draw_frame` call **plus** a hard
/// `device.poll(Wait)`. That serialises CPU and GPU, so the number is a ceiling
/// rather than a typical frame, which is the honest direction to be wrong in.
///
/// It used to time only the inside of `draw_frame`, which is CPU submit. The
/// documented meaning of the metric is "CPU submit plus a hard GPU wait", and
/// CPU submit on its own is not frame time, so the two have to be made to
/// agree before any cross-target number means anything.
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
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

    let mut canvas = GpuCanvas::new(&device, &queue, format, cfg.app.atlas, cfg.app.items + 64);
    let mut app = AppState::new(cfg.app.clone());
    let size = canvas_core::Vec2::new(cfg.width as f32, cfg.height as f32);
    let mut metrics = Metrics::default();

    for frame in 0..(cfg.warmup + cfg.frames) {
        let t0 = Instant::now();
        let stats = app.draw_frame(&mut canvas, &device, &queue, &target_view, size);
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        let cpu = t0.elapsed();
        if frame >= cfg.warmup {
            metrics.push(cpu, stats);
        }
    }

    RunReport { metrics, adapter }
}

/// Text report for reading.
pub fn report(m: &Metrics, adapter: &wgpu::AdapterInfo, budget_ms: f64) -> String {
    let (mean, p50, p99, max, over) = m.percentiles(budget_ms);
    let ph = m.total_placeholder();
    let deg = m.total_degraded();
    let vis = m.total_visible();
    let mut out = String::new();
    out.push_str(&format!(
        "adapter           {} / {:?}\n",
        adapter.name, adapter.backend
    ));
    out.push_str(&format!("frames            {}\n", m.len()));
    out.push_str(&format!(
        "cpu ms            mean {mean:6.2}  p50 {p50:6.2}  p99 {p99:6.2}  max {max:6.2}\n"
    ));
    out.push_str(&format!("over budget       {over:>5} / {}\n", m.len()));
    out.push_str(&format!("peak visible      {}\n", m.peak_visible));
    out.push_str(&format!(
        "placeholder       {ph} of {vis} item-frames ({:.2}%)\n",
        100.0 * ph as f64 / vis.max(1) as f64
    ));
    out.push_str(&format!(
        "degraded          {deg} of {vis} item-frames ({:.2}%)\n",
        100.0 * deg as f64 / vis.max(1) as f64
    ));
    out
}

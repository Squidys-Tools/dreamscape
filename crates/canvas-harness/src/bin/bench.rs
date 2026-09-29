//! Headless benchmark: the canvas with no UI attached.
//!
//! This is the floor. The three UI hosts add chrome on top of exactly this
//! pipeline, so their numbers are only meaningful relative to it.

use canvas_app::{Config, Motion, BUDGET_MS};
use canvas_gpu::AtlasConfig;
use canvas_harness::{report, run, RunConfig};

fn arg(name: &str, default: &str) -> String {
    std::env::args()
        .skip(1)
        .find(|a| a.starts_with(&format!("--{name}=")))
        .map(|a| {
            a.split_once('=')
                .map(|(_, v)| v.to_string())
                .unwrap_or_default()
        })
        .unwrap_or_else(|| default.to_string())
}

fn main() {
    let cfg = RunConfig {
        app: Config {
            items: arg("items", "2000").parse().unwrap(),
            textures: arg("textures", "2000").parse().unwrap(),
            atlas: AtlasConfig {
                size: arg("atlas", "2048").parse().unwrap(),
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                debug_slots: false,
            },
            extent: arg("extent", "1000").parse().unwrap(),
            motion: Motion {
                pan_x: arg("pan", "18.0").parse().unwrap(),
                pan_y: arg("pan", "18.0").parse::<f32>().unwrap() * 0.35,
                zoom_sweep: arg("zoom", "0.7").parse().unwrap(),
            },
            ..Default::default()
        },
        width: arg("width", "2560").parse().unwrap(),
        height: arg("height", "1440").parse().unwrap(),
        warmup: arg("warmup", "120").parse().unwrap(),
        frames: arg("frames", "200").parse().unwrap(),
    };

    println!("dreamscape canvas spike - headless");
    println!(
        "  scene      {} items, {} distinct images, {}x{}, atlas {}",
        cfg.app.items, cfg.app.textures, cfg.width, cfg.height, cfg.app.atlas.size
    );

    let r = run(&cfg);
    print!("{}", report(&r.metrics, &r.adapter, BUDGET_MS));
    println!("{}", r.metrics.result_line(BUDGET_MS));
}

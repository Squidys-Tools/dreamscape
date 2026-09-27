//! Headless benchmark: pans and zooms a 2,000-item board and reports frame times.
//!
//! This is the SQU-60 acceptance test, minus the UI library. Run it before
//! wiring up a host, so the canvas baseline is known independently.

use canvas_gpu::AtlasConfig;
use canvas_harness::{run, RunConfig};

fn arg(name: &str, default: &str) -> String {
    std::env::args()
        .skip(1)
        .find(|a| a.starts_with(&format!("--{name}=")))
        .map(|a| a.split_once('=').map(|(_, v)| v.to_string()).unwrap_or_default())
        .unwrap_or_else(|| default.to_string())
}

fn main() {
    let cfg = RunConfig {
        items: arg("items", "2000").parse().unwrap(),
        textures: arg("textures", "64").parse().unwrap(),
        atlas: AtlasConfig {
            size: arg("atlas", "2048").parse().unwrap(),
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            debug_slots: false,
        },
        width: arg("width", "1600").parse().unwrap(),
        height: arg("height", "900").parse().unwrap(),
        warmup: arg("warmup", "120").parse().unwrap(),
        frames: arg("frames", "400").parse().unwrap(),
        pan_per_frame: arg("pan", "26.0").parse().unwrap(),
        zoom_sweep: arg("zoom", "0.6").parse().unwrap(),
        seed: 0x5EED,
    };

    println!("dreamscape canvas spike — headless");
    println!(
        "  scene      {} items, {} textures, {}x{}, atlas {}",
        cfg.items, cfg.textures, cfg.width, cfg.height, cfg.atlas.size
    );
    println!(
        "  motion     pan {:.0}/frame, zoom x{:.2} -> x{:.2}",
        cfg.pan_per_frame,
        1.0,
        1.0 - cfg.zoom_sweep
    );

    let report = run(&cfg);
    println!(
        "  adapter    {} / {:?} / {}\n",
        report.adapter.name, report.adapter.backend, report.adapter.driver
    );
    print!("{}", report.metrics.report(16.67));
    println!("peak visible items  {}", report.peak_visible);
    println!(
        "final viewport      center ({:.0}, {:.0}) scale {:.3}",
        report.final_viewport.center.x,
        report.final_viewport.center.y,
        report.final_viewport.scale
    );
    // Stable machine-readable summary. Scripts parse this, not the text above.
    println!("{}", report.metrics.machine_line(report.peak_visible));
}

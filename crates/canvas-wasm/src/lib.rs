//! Browser host for the canvas renderer.
//!
//! This crate is the whole reason it exists as its own crate: `wasm-bindgen` and
//! `web-sys` belong at the adapter boundary and nowhere else. Putting them in
//! `canvas-app` would drag the JS boundary into the layer every native host
//! links, which is the first step toward the two-renderer split the desktop and
//! the hosted tier are supposed to share instead.
//!
//! The seam it implements is defined in `canvas-app` and `canvas-core`. What is
//! left here is the three things only a browser can do: get a GPU context out
//! of a `<canvas>`, own a clock, and be paced by the compositor. Everything
//! else is the same `AppState` the headless harness drives.
//!
//! Two measurements come out of it, and they are defined to match the harness
//! rather than to be convenient:
//!
//! * The frame window starts at the `draw_frame` call and ends when the queue
//!   reports all submitted work done. That is the browser's equivalent of the
//!   harness's `device.poll(Wait)`, so the two serialise CPU and GPU the same
//!   way and the numbers can be read next to each other.
//! * The RESULT line is [`canvas_app::Metrics::result_line`], formatted once, so
//!   the browser cannot report a field the native runner spells differently.
//!
//! What is *not* the same: the native hosts render into an offscreen texture and
//! blit it, because their toolkit owns the surface. Here the canvas context is
//! the target, so the browser figure excludes one fullscreen blit that the
//! desktop pays. Read the browser number as a lower bound on the desktop host.

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::time::Duration;

use canvas_app::{AppState, Config, FrameStats, Metrics, Motion, BUDGET_MS};
use canvas_core::{PointerEvent, PointerPhase, Vec2, WheelEvent};
use canvas_gpu::{AtlasConfig, GpuCanvas};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{HtmlCanvasElement, Performance};

fn window() -> web_sys::Window {
    web_sys::window().expect("the browser host needs a window")
}

fn err(what: &str, e: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&format!("{what}: {e}"))
}

/// Everything the adapter is willing to say about itself, on one line.
///
/// A browser is allowed to report an empty adapter name, and this one does. That
/// is a conditions problem rather than a cosmetic one: a figure with no machine
/// attached to it cannot be reproduced or disbelieved later, so the parts that
/// *are` available are printed and the missing ones are named.
fn describe(info: &wgpu::AdapterInfo) -> String {
    let hex = |v: u32| (v != 0).then(|| format!("{v:#06x}"));
    let mut parts = Vec::new();
    if !info.name.is_empty() {
        parts.push(info.name.clone());
    }
    if let Some(v) = hex(info.vendor) {
        parts.push(format!("vendor {v}"));
    }
    if let Some(v) = hex(info.device) {
        parts.push(format!("device {v}"));
    }
    if !info.driver.is_empty() {
        parts.push(format!("driver {}", info.driver));
    }
    if !info.driver_info.is_empty() {
        parts.push(info.driver_info.clone());
    }
    parts.push(format!("{:?}", info.backend));
    if parts.len() == 1 {
        format!("{} / no adapter identity reported", parts[0])
    } else {
        parts.join(" / ")
    }
}

/// A canvas drawing into a `<canvas>`, plus the metrics its frames produced.
///
/// The `Vec2` positions the host forwards are in device pixels relative to the
/// canvas, so a page with a CSS-scaled canvas multiplies by `devicePixelRatio`
/// exactly once, here in JavaScript.
/// Everything a frame can change.
///
/// Split out and behind a [`RefCell`] because of one rule from `wasm-bindgen`:
/// an exported `&mut self` method that awaits holds its borrow until the future
/// resolves. Any other call into the same object during that window is a
/// re-entrant mutable borrow, and `wasm-bindgen` panics with "recursive use of
/// an object detected which would lead to unsafe aliasing in rust".
///
/// That is not a theoretical hazard. The frame loop awaits the GPU on every
/// frame, so a pointer event arriving mid-frame lands inside the borrow window
/// and throws. With `&self` exports and the mutable state in here, a borrow is
/// taken for a few statements and dropped before anything awaits, and the
/// browser can call in whenever it likes.
struct Inner {
    config: wgpu::SurfaceConfiguration,
    canvas: GpuCanvas,
    app: AppState,
    metrics: Metrics,
    size: Vec2,
    /// Downsampled RGB of the last readback, for [`CanvasHost::verify_map`].
    map: Vec<u8>,
    map_w: u32,
    map_h: u32,
}

#[wasm_bindgen]
pub struct CanvasHost {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    perf: Performance,
    adapter: String,
    inner: RefCell<Inner>,
}

#[wasm_bindgen]
impl CanvasHost {
    /// Build a device from a canvas element and draw one frame of `items`
    /// distinct images.
    ///
    /// `backend` is `"auto"`, `"webgpu"` or `"webgl"`. Forcing a backend is how
    /// the fallback gets tested: the same seam, the same renderer, one different
    /// field.
    ///
    /// `extent`, `pan` and `zoom` are named and interpreted exactly as the
    /// matching `bench` flags are, so the same scene can be described to both
    /// runners. That is the point: a browser figure is only comparable to a
    /// native figure if the scene underneath is the same one.
    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        canvas: HtmlCanvasElement,
        items: u32,
        textures: u32,
        atlas: u32,
        extent: f32,
        pan: f32,
        zoom: f32,
        animate: bool,
        backend: &str,
    ) -> Result<CanvasHost, JsValue> {
        let backends = match backend {
            "webgpu" => wgpu::Backends::BROWSER_WEBGPU,
            "webgl" => {
                if cfg!(not(feature = "webgl")) {
                    return Err(JsValue::from_str(
                        "the webgl backend is not compiled in: build with --features webgl",
                    ));
                }
                wgpu::Backends::GL
            }
            _ => wgpu::Backends::all(),
        };

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends,
            ..Default::default()
        });

        // The surface is offered to adapter selection on purpose, and the reason is
        // recorded here because it looks removable and is not.
        //
        // wgpu-hal enumerates GL adapters *from the canvas's own WebGL2 context*
        // (wgpu-hal 27 `gles/web.rs`, `enumerate_adapters`): with no surface hint
        // it returns an empty list, so the request fails with "gl found no
        // adapters" no matter how capable the browser is. Dropping the hint trades
        // a correct-but-hanging path for a fast misleading one.
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
            .map_err(|e| err("create_surface", e))?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
            })
            .await
            .map_err(|e| err("request_adapter", e))?;

        // Deliberately the same limits the headless harness asks for. Requesting
        // more on one target and less on the other would make every cross-target
        // number a comparison of two devices rather than of two hosts.
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("browser-device"),
                required_limits: wgpu::Limits::downlevel_webgl2_defaults()
                    .using_resolution(adapter.limits()),
                memory_hints: wgpu::MemoryHints::Performance,
                ..Default::default()
            })
            .await
            .map_err(|e| err("request_device", e))?;

        let caps = surface.get_capabilities(&adapter);
        // sRGB, because the renderer samples an sRGB atlas and therefore emits
        // linear values. Handing it a plain UNORM surface stores those linear
        // numbers as if they were already encoded, which reads on screen as a
        // board rendered about a gamma too dark. The native hosts avoid this by
        // passing `Rgba8UnormSrgb` explicitly, so the browser host has to
        // choose rather than take the first format it is offered.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| {
                matches!(
                    f,
                    wgpu::TextureFormat::Rgba8UnormSrgb | wgpu::TextureFormat::Bgra8UnormSrgb
                )
            })
            .or_else(|| caps.formats.first().copied())
            .ok_or_else(|| JsValue::from_str("the surface reports no formats"))?;

        let (w, h) = (canvas.width().max(1), canvas.height().max(1));
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: w,
            height: h,
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
            desired_maximum_frame_latency: 1,
        };
        surface.configure(&device, &config);

        let cfg = Config {
            items,
            textures,
            atlas: AtlasConfig {
                size: atlas,
                ..Default::default()
            },
            extent,
            motion: Motion {
                pan_x: pan,
                // Matches `bench --pan`, which derives y from x the same way.
                pan_y: pan * 0.35,
                zoom_sweep: zoom,
            },
            ..Default::default()
        };

        let mut app = AppState::new(cfg.clone());
        app.animate = animate;
        let canvas = GpuCanvas::new(
            &device,
            &queue,
            format,
            cfg.atlas,
            cfg.items.saturating_add(64),
        );
        let info = adapter.get_info();
        let adapter = describe(&info);

        Ok(Self {
            device,
            queue,
            surface,
            perf: window()
                .performance()
                .ok_or_else(|| JsValue::from_str("window.performance is unavailable"))?,
            adapter,
            inner: RefCell::new(Inner {
                config,
                canvas,
                app,
                metrics: Metrics::default(),
                size: Vec2::new(w as f32, h as f32),
                map: Vec::new(),
                map_w: 0,
                map_h: 0,
            }),
        })
    }

    /// Resize the drawing buffer. The page owns the element, so it passes the
    /// device-pixel size it has already set on it.
    pub fn resize(&self, width: u32, height: u32) {
        let config = {
            let mut inner = self.inner.borrow_mut();
            inner.config.width = width.max(1);
            inner.config.height = height.max(1);
            inner.size = Vec2::new(inner.config.width as f32, inner.config.height as f32);
            inner.config.clone()
        };
        self.surface.configure(&self.device, &config);
    }

    /// Stop or resume the scripted camera motion, so a page can measure the
    /// same scene the harness does and then let the pointer drive it.
    pub fn set_animate(&self, animate: bool) {
        self.inner.borrow_mut().app.animate = animate;
    }

    pub fn pointer_down(&self, x: f32, y: f32) {
        self.pointer(x, y, PointerPhase::Down);
    }

    pub fn pointer_move(&self, x: f32, y: f32) {
        self.pointer(x, y, PointerPhase::Move);
    }

    pub fn pointer_up(&self, x: f32, y: f32) {
        self.pointer(x, y, PointerPhase::Up);
    }

    /// `delta_y` positive zooms out, matching a wheel scrolled away from the
    /// user.
    pub fn wheel(&self, x: f32, y: f32, delta_y: f32) {
        let size = self.inner.borrow().size;
        self.inner.borrow_mut().app.wheel(
            WheelEvent {
                pos: Vec2::new(x, y),
                delta: Vec2::new(0.0, delta_y),
            },
            size,
        );
    }

    /// Whether a drag is in progress, so the page can switch the cursor.
    pub fn dragging(&self) -> bool {
        self.inner.borrow().app.dragging()
    }

    /// One frame, timed the way the harness times one, in milliseconds.
    ///
    /// Measures `draw_frame` plus the queue reporting all submitted work done.
    /// The GPU wait is inside the window deliberately: CPU submit on its own is
    /// not frame time, which is the mistake the harness was corrected for.
    pub async fn tick(&self) -> Result<f64, JsValue> {
        let (ms, stats) = self.frame().await?;
        self.inner
            .borrow_mut()
            .metrics
            .push(Duration::from_secs_f64(ms / 1000.0), stats);
        Ok(ms)
    }

    /// Run `warmup` unmeasured frames and then `frames` measured ones, and
    /// return the RESULT line.
    ///
    /// The same warmup-then-measure shape as the harness, on the same scene, so
    /// the two lines are the same measurement rather than two similar ones.
    pub async fn run(&self, warmup: u32, frames: u32) -> Result<String, JsValue> {
        self.inner.borrow_mut().metrics.clear();
        for i in 0..(warmup + frames) {
            let (ms, stats) = self.frame().await?;
            if i >= warmup {
                self.inner
                    .borrow_mut()
                    .metrics
                    .push(Duration::from_secs_f64(ms / 1000.0), stats);
            }
        }
        Ok(self.inner.borrow().metrics.result_line(BUDGET_MS))
    }

    /// The RESULT line for whatever has been measured so far. The interactive
    /// page reads this so its live numbers cannot drift from the bench's.
    pub fn result_line(&self) -> String {
        self.inner.borrow().metrics.result_line(BUDGET_MS)
    }

    /// Adapter name and backend, for the conditions a figure has to carry.
    pub fn adapter(&self) -> String {
        self.adapter.clone()
    }

    /// The drawing-buffer size actually being rendered, in device pixels.
    pub fn size(&self) -> String {
        let inner = self.inner.borrow();
        format!("{}x{}", inner.config.width, inner.config.height)
    }

    /// Current camera as `scale x y`.
    ///
    /// On an infinite board, "how far in am I and where am I" is the first
    /// question, and a person dragging one deserves an answer. It is also the
    /// only way to see that the input path did anything at all, since the board
    /// itself is a canvas that a screenshot cannot read.
    pub fn camera(&self) -> String {
        let v = &self.inner.borrow().app.viewport;
        format!("{:.3}x  x {:.0}  y {:.0}", v.scale, v.center.x, v.center.y)
    }

    /// An ASCII luminance map of the last [`CanvasHost::verify_pixels`] frame.
    ///
    /// A count of distinct colours cannot tell you whether a board was drawn. It
    /// can tell you that *something* was drawn, which is the difference between a
    /// renderer and a flat fill, but a picture settles it in one look. A ramp
    /// needs no image encoder and no round trip through the DOM, which is what
    /// makes it usable at all in a headless browser.
    pub fn verify_map(&self) -> String {
        const RAMP: &[u8] = b" .:-=+*#%@";
        let inner = self.inner.borrow();
        let (mw, mh) = (inner.map_w as usize, inner.map_h as usize);
        if mw == 0 {
            return "(no readback yet)".into();
        }
        let mut out = String::with_capacity((mw + 1) * mh);
        for y in 0..mh {
            for x in 0..mw {
                let i = (y * mw + x) * 3;
                // Luma over the stored bytes, which is what the ramp is tuned for.
                let lum = (inner.map[i] as u32 * 30
                    + inner.map[i + 1] as u32 * 59
                    + inner.map[i + 2] as u32 * 11)
                    / 100;
                out.push(RAMP[(lum as usize * (RAMP.len() - 1) / 255).min(RAMP.len() - 1)] as char);
            }
            out.push('\n');
        }
        out
    }

    /// Draw one frame into a readable texture and summarise the pixels that came
    /// out.
    ///
    /// This exists because the obvious ways to look at a WebGPU canvas both lie
    /// in a headless browser. A page screenshot comes back blank for a canvas
    /// layer the compositor never drew, and `createImageBitmap` on the canvas
    /// reads the same missing layer. A frame that was submitted and never
    /// painted, and a frame that was painted correctly but never composited, look
    /// identical from the outside, and they mean opposite things: one is a
    /// renderer bug, the other is a presentation limitation nobody would have
    /// found by reading the diff.
    ///
    /// So this bypasses the canvas entirely and reads the render target back off
    /// the GPU. It answers "did the renderer draw a board", which is the
    /// question worth answering here. Whether the browser then *presents* that
    /// board is a separate claim, and the honest way to make it is a screenshot
    /// of a headed browser.
    ///
    /// Advances the camera, because it draws a real frame through the real
    /// pipeline rather than a special path.
    pub async fn verify_pixels(&self) -> Result<String, JsValue> {
        let (w, h, format) = {
            let inner = self.inner.borrow();
            (inner.config.width, inner.config.height, inner.config.format)
        };
        let target = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("verify-target"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());

        // One borrow, destructured. Two `borrow_mut` calls on the same cell, or
        // a borrow held while the other is taken, would panic at runtime rather
        // than fail to compile.
        let stats = {
            let mut inner = self.inner.borrow_mut();
            let Inner {
                canvas, app, size, ..
            } = &mut *inner;
            app.draw_frame(canvas, &self.device, &self.queue, &view, *size)
        };
        self.gpu_idle().await?;

        // `copy_texture_to_buffer` requires a 256-byte row alignment, so the
        // staging rows are padded and the padding skipped when reading.
        let unpadded = w as usize * 4;
        let stride = unpadded.div_ceil(256) * 256;
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("verify-readback"),
            size: (stride * h as usize) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("verify-copy"),
            });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride as u32),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(std::iter::once(enc.finish()));
        self.gpu_idle().await?;

        let mapped = map_read(&staging, 0..(stride * h as usize) as u64).await?;
        let data = &mapped[..];

        // Sampled on a prime stride rather than every pixel: a 2560x1440 readback
        // is 14MB and a full histogram of it buys nothing over a hundred thousand
        // samples of a board made of flat swatches.
        const STRIDE_SAMPLE: usize = 37;
        let mut counts: std::collections::HashMap<[u8; 3], usize> =
            std::collections::HashMap::new();
        for y in 0..h as usize {
            let row = y * stride;
            for x in (0..unpadded).step_by(4 * STRIDE_SAMPLE) {
                let px = &data[row + x..row + x + 4];
                *counts.entry([px[0], px[1], px[2]]).or_default() += 1;
            }
        }
        let mut top: Vec<([u8; 3], usize)> = counts.into_iter().collect();
        top.sort_unstable_by(|a, b| b.1.cmp(&a.1));
        let sampled: usize = top.iter().map(|(_, n)| n).sum();
        let hex = |c: [u8; 3]| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);

        // Max-pooled downsample for the map, from the same readback. A mean over
        // a block that is mostly background reports a bright thumbnail as
        // background, so "is there real content, and how much contrast does it
        // have" is a question about the brightest pixel in each block rather than
        // the average one.
        const MAP_W: u32 = 120;
        let map_h = ((MAP_W as u64 * h as u64) / w as u64).max(1) as u32;
        let mut map = vec![0u8; (MAP_W * map_h * 3) as usize];
        let mut peak = 0u8;
        let mut luma_hist = [0u32; 8];
        for my in 0..map_h {
            let y0 = my as u64 * h as u64 / map_h as u64;
            let y1 = (((my + 1) as u64 * h as u64) / map_h as u64)
                .max(y0 + 1)
                .min(h as u64);
            for mx in 0..MAP_W {
                let x0 = mx as u64 * w as u64 / MAP_W as u64;
                let x1 = (((mx + 1) as u64 * w as u64) / MAP_W as u64)
                    .max(x0 + 1)
                    .min(w as u64);
                let mut best = [0u32; 3];
                for y in y0..y1 {
                    let row = y as usize * stride;
                    for x in x0..x1 {
                        let o = row + x as usize * 4;
                        for k in 0..3 {
                            best[k] = best[k].max(data[o + k] as u32);
                        }
                    }
                }
                let lum = (best[0] * 30 + best[1] * 59 + best[2] * 11) / 100;
                peak = peak.max(lum as u8);
                luma_hist[(lum as usize * 8 / 256).min(7)] += 1;
                let o = ((my * MAP_W + mx) * 3) as usize;
                for k in 0..3 {
                    map[o + k] = best[k] as u8;
                }
            }
        }
        // What the pipeline's clear should land as in this format. The render
        // target here is whatever the surface prefers, which is not sRGB, so the
        // clear is stored as written and comparing it through an sRGB encode
        // would report a frame of pure clear as full of content.
        let clear = self.inner.borrow().canvas.background;
        {
            let mut inner = self.inner.borrow_mut();
            inner.map = map;
            inner.map_w = MAP_W;
            inner.map_h = map_h;
        }
        let byte_of = |v: f32| (v.clamp(0.0, 1.0) * 255.0) as u8;
        let expect = [byte_of(clear[0]), byte_of(clear[1]), byte_of(clear[2])];

        Ok(format!(
            "verify {w}x{h} format={format:?} drawn={} visible={} sampled={sampled} \
             distinct={} expect_clear={} peak_luma={peak} luma_hist={:?} top=[{}]",
            stats.drawn,
            stats.visible,
            top.len(),
            hex(expect),
            luma_hist,
            top.iter()
                .take(4)
                .map(|(c, n)| format!("{}x{}", hex(*c), n))
                .collect::<Vec<_>>()
                .join(" "),
        ))
    }
}

/// Resolve once `buffer` is mapped for reading.
async fn map_read(buffer: &wgpu::Buffer, range: std::ops::Range<u64>) -> Result<Vec<u8>, JsValue> {
    let slice = buffer.slice(range);
    let done = js_sys::Promise::new(&mut |resolve, reject| {
        slice.map_async(wgpu::MapMode::Read, move |r| match r {
            Ok(()) => {
                let _ = resolve.call0(&JsValue::NULL);
            }
            Err(e) => {
                let _ = reject.call1(&JsValue::NULL, &JsValue::from_str(&e.to_string()));
            }
        });
    });
    JsFuture::from(done)
        .await
        .map_err(|_| JsValue::from_str("the readback buffer could not be mapped"))?;

    let out = slice.get_mapped_range().to_vec();
    buffer.unmap();
    Ok(out)
}

impl CanvasHost {
    fn pointer(&self, x: f32, y: f32, phase: PointerPhase) {
        let mut inner = self.inner.borrow_mut();
        let size = inner.size;
        inner.app.pointer(
            PointerEvent {
                pos: Vec2::new(x, y),
                phase,
            },
            size,
        );
    }

    /// One frame, submitted and presented, then waited on.
    ///
    /// The mutable borrow is released before the await. That is the whole reason
    /// this method takes `&self`: holding a `&mut self` across the GPU wait would
    /// make every pointer event that arrived during a frame a re-entrant borrow,
    /// and `wasm-bindgen` turns that into a thrown panic.
    async fn frame(&self) -> Result<(f64, FrameStats), JsValue> {
        let frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                // The element changed size under us. Reconfigure and report a
                // skipped frame rather than counting a reconfigure as a fast one.
                let config = self.inner.borrow().config.clone();
                self.surface.configure(&self.device, &config);
                return Err(JsValue::from_str("surface reconfigured"));
            }
            Err(e) => return Err(err("get_current_texture", e)),
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let start = self.perf.now();
        let stats = {
            let mut inner = self.inner.borrow_mut();
            let Inner {
                canvas, app, size, ..
            } = &mut *inner;
            app.draw_frame(canvas, &self.device, &self.queue, &view, *size)
        };
        frame.present();

        self.gpu_idle().await?;
        Ok((self.perf.now() - start, stats))
    }

    /// Resolve once the GPU has finished everything submitted so far.
    ///
    /// This is the browser's `device.poll(Wait)`, and it is what makes the frame
    /// numbers comparable with the harness's. wgpu 27 hands this over as a
    /// callback rather than a future on this target, so the callback is bridged
    /// onto a promise: awaiting it blocks the frame until the GPU is idle, which
    /// is the whole point.
    async fn gpu_idle(&self) -> Result<(), JsValue> {
        let done = js_sys::Promise::new(&mut |resolve, _reject| {
            self.queue.on_submitted_work_done(move || {
                let _ = resolve.call0(&JsValue::NULL);
            });
        });
        JsFuture::from(done)
            .await
            .map_err(|_| JsValue::from_str("the gpu-idle promise was rejected"))?;
        Ok(())
    }
}

//! Throwaway probe: can wgpu 27 present to a winit 0.30 window on this machine?
//!
//! iced 0.13 and 0.14 both open a window and never draw a frame, on Vulkan and
//! DX12 alike, while headless wgpu renders correctly. This binary strips the
//! question down to raw winit + raw wgpu with no UI toolkit in the path, so the
//! answer applies to every wgpu-based host we might build.
//!
//! Draws a solid magenta clear: if the window goes magenta, presentation works
//! and the fault is upstream in iced. If it stays grey, presentation is broken
//! at the wgpu/winit/driver level and no wgpu host can be measured here.

use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

#[derive(Default)]
struct Probe {
    window: Option<Arc<Window>>,
    surface: Option<wgpu::Surface<'static>>,
    device: Option<wgpu::Device>,
    queue: Option<wgpu::Queue>,
    pipeline: Option<wgpu::RenderPipeline>,
    format: Option<wgpu::TextureFormat>,
    frames: u64,
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let window = match event_loop
            .create_window(Window::default_attributes().with_title("Probe - raw wgpu surface"))
        {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("FATAL create_window: {e}");
                event_loop.exit();
                return;
            }
        };
        eprintln!("window created");

        let size = window.inner_size();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let surface = match instance.create_surface(Arc::clone(&window)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("FATAL create_surface: {e}");
                event_loop.exit();
                return;
            }
        };
        eprintln!("surface created");

        let adapter =
            match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::None,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("FATAL request_adapter: {e}");
                    event_loop.exit();
                    return;
                }
            };
        eprintln!("adapter: {:?}", adapter.get_info());

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("probe-device"),
            required_features: wgpu::Features::empty(),
            // `using_resolution` matters here, and the omission of it is why this
            // probe stopped working rather than starting to fail differently.
            // `downlevel_webgl2_defaults()` caps `max_texture_dimension_2d` at
            // 2048, and the surface is then configured at the raw window size, so
            // a monitor wider than 2048 pixels made `Surface::configure` fail
            // validation and the process panic before it drew a single frame.
            // The harness and the browser host resolve the resolution-dependent
            // limits against the adapter, which is what lets them render at
            // 2560x1440. Same line, same reason.
            required_limits: wgpu::Limits::downlevel_webgl2_defaults()
                .using_resolution(adapter.limits()),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        }))
        .expect("request_device");

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        eprintln!("surface format: {format:?}");

        surface.configure(
            &device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format,
                width: size.width.max(1),
                height: size.height.max(1),
                present_mode: wgpu::PresentMode::Fifo,
                desired_maximum_frame_latency: 2,
                alpha_mode: caps.alpha_modes[0],
                view_formats: vec![],
            },
        );
        eprintln!("surface configured");

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit"),
            source: wgpu::ShaderSource::Wgsl(
                r#"
                @vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
                    let x = f32(i & 1u) * 4.0 - 1.0;
                    let y = f32(i >> 1u) * 4.0 - 1.0;
                    return vec4<f32>(x, -y, 0.0, 1.0);
                }
                @fragment fn fs() -> @location(0) vec4<f32> {
                    return vec4<f32>(1.0, 0.0, 1.0, 1.0);
                }
                "#
                .into(),
            ),
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(format.into())],
                compilation_options: Default::default(),
            }),
            multiview: None,
            cache: None,
        });

        self.window = Some(window);
        self.surface = Some(surface);
        self.device = Some(device);
        self.queue = Some(queue);
        self.pipeline = Some(pipeline);
        self.format = Some(format);
        eprintln!("pipeline ready; entering redraw loop");
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                eprintln!("Resized -> {}x{}", size.width, size.height);
                if let (Some(surface), Some(device), Some(format)) =
                    (self.surface.as_ref(), self.device.as_ref(), self.format)
                {
                    surface.configure(
                        device,
                        &wgpu::SurfaceConfiguration {
                            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                            format,
                            width: size.width.max(1),
                            height: size.height.max(1),
                            present_mode: wgpu::PresentMode::Fifo,
                            desired_maximum_frame_latency: 2,
                            alpha_mode: wgpu::CompositeAlphaMode::Auto,
                            view_formats: vec![],
                        },
                    );
                }
            }
            WindowEvent::RedrawRequested => {
                let (Some(surface), Some(device), Some(queue), Some(pipeline)) = (
                    self.surface.as_ref(),
                    self.device.as_ref(),
                    self.queue.as_ref(),
                    self.pipeline.as_ref(),
                ) else {
                    return;
                };

                match surface.get_current_texture() {
                    Ok(frame) => {
                        let view = frame.texture.create_view(&Default::default());
                        let mut enc =
                            device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("frame"),
                            });
                        {
                            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                                label: Some("frame"),
                                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                    view: &view,
                                    depth_slice: None,
                                    resolve_target: None,
                                    ops: wgpu::Operations {
                                        load: wgpu::LoadOp::Clear(wgpu::Color {
                                            r: 1.0,
                                            g: 0.0,
                                            b: 1.0,
                                            a: 1.0,
                                        }),
                                        store: wgpu::StoreOp::Store,
                                    },
                                })],
                                depth_stencil_attachment: None,
                                timestamp_writes: None,
                                occlusion_query_set: None,
                            });
                            pass.set_pipeline(pipeline);
                            pass.draw(0..3, 0..1);
                        }
                        queue.submit([enc.finish()]);
                        frame.present();
                        self.frames += 1;
                        if self.frames <= 3 || self.frames.is_multiple_of(60) {
                            eprintln!("presented frame {}", self.frames);
                        }
                    }
                    Err(e) => eprintln!("get_current_texture failed at frame {}: {e}", self.frames),
                }
            }
            _ => {}
        }
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

fn main() {
    env_logger::init();
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let _ = event_loop.run_app(&mut Probe::default());
}

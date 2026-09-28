//! Bridges [`canvas_app::AppState`] into iced as a custom `Primitive`.
//!
//! The seam is the whole point of this host. iced 0.14 is on wgpu 27, the same
//! version as the canvas renderer, so `prepare` receives iced's device and queue
//! and the canvas runs on the same device as the chrome. Nothing is copied
//! through the CPU: the canvas renders into its own texture and a single
//! fullscreen triangle samples it inside iced's existing render pass.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use canvas_app::{AppState, Metrics};
use canvas_gpu::GpuCanvas;
use iced::advanced::graphics::core::Rectangle;
use iced::advanced::graphics::Viewport;
use iced::advanced::mouse;
use iced::widget::shader;
use iced_wgpu::primitive::{Pipeline, Primitive};

/// Owned by the app and shared with the shader widget.
pub struct Shared {
    pub app: AppState,
    /// Created on the first `prepare`, once iced's device is available.
    pub canvas: Option<GpuCanvas>,
    pub metrics: Metrics,
    pub last_size: (u32, u32),
    pub last_uploads: u32,
    pub last_evictions: u64,
}

impl Shared {
    pub fn new(app: AppState) -> Self {
        Self {
            app,
            canvas: None,
            metrics: Metrics::default(),
            last_size: (1, 1),
            last_uploads: 0,
            last_evictions: 0,
        }
    }
}

pub type SharedHandle = Arc<Mutex<Shared>>;

const BLIT_SHADER: &str = r#"
@group(0) @binding(0) var src : texture_2d<f32>;
@group(0) @binding(1) var samp : sampler;

struct VSOut {
    @builtin(position) pos : vec4<f32>,
    @location(0) uv : vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi : u32) -> VSOut {
    // Oversized triangle rather than a quad: three vertices, no diagonal seam.
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 3.0,  1.0),
    );
    var out : VSOut;
    out.pos = vec4<f32>(p[vi], 0.0, 1.0);
    // Texture y grows downward, clip y grows upward.
    out.uv = vec2<f32>((p[vi].x + 1.0) * 0.5, (1.0 - p[vi].y) * 0.5);
    return out;
}

@fragment
fn fs(in : VSOut) -> @location(0) vec4<f32> {
    return textureSample(src, samp, in.uv);
}
"#;

/// The canvas renders sRGB and the blit converts into iced's surface format.
const CANVAS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// GPU-side state iced creates once and shares across all canvas instances:
/// the offscreen texture the canvas draws into, plus the blit that presents it.
pub struct CanvasPipeline {
    format: wgpu::TextureFormat,
    sampler: wgpu::Sampler,
    layout: wgpu::BindGroupLayout,
    blit: wgpu::RenderPipeline,
    /// Recreated only when the canvas widget's size changes.
    target: Option<wgpu::Texture>,
    view: Option<wgpu::TextureView>,
    bind_group: Option<wgpu::BindGroup>,
}

impl Pipeline for CanvasPipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit-shader"),
            source: wgpu::ShaderSource::Wgsl(BLIT_SHADER.into()),
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit-pl"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let blit = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(format.into())],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        Self {
            format,
            sampler,
            layout,
            blit,
            target: None,
            view: None,
            bind_group: None,
        }
    }
}

pub struct CanvasProgram {
    shared: SharedHandle,
}

impl CanvasProgram {
    pub fn new(shared: SharedHandle) -> Self {
        Self { shared }
    }
}

impl<M> shader::Program<M> for CanvasProgram {
    type State = ();
    type Primitive = CanvasPrimitive;

    fn draw(
        &self,
        _state: &Self::State,
        _cursor: mouse::Cursor,
        _bounds: Rectangle,
    ) -> Self::Primitive {
        CanvasPrimitive {
            shared: self.shared.clone(),
        }
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        _bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        mouse::Interaction::Grab
    }
}

pub struct CanvasPrimitive {
    shared: SharedHandle,
}

impl std::fmt::Debug for CanvasPrimitive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CanvasPrimitive")
    }
}

impl Primitive for CanvasPrimitive {
    type Pipeline = CanvasPipeline;

    /// Runs a whole canvas frame on iced's device and queue.
    fn prepare(
        &self,
        pipeline: &mut Self::Pipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        viewport: &Viewport,
    ) {
        let start = Instant::now();
        let mut guard = self.shared.lock().expect("canvas state poisoned");
        // Split the guard so the app and the canvas can be borrowed at once.
        let sh: &mut Shared = &mut guard;

        let w = ((bounds.width * viewport.scale_factor()).round() as u32).max(1);
        let h = ((bounds.height * viewport.scale_factor()).round() as u32).max(1);
        sh.last_size = (w, h);

        let needs_target = match &pipeline.target {
            Some(t) => t.width() != w || t.height() != h,
            None => true,
        };
        if needs_target {
            pipeline.target = Some(device.create_texture(&wgpu::TextureDescriptor {
                label: Some("canvas-target"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: CANVAS_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            }));
            pipeline.view = None;
            pipeline.bind_group = None;
        }
        if pipeline.view.is_none() {
            pipeline.view = Some(
                pipeline
                    .target
                    .as_ref()
                    .expect("target created above")
                    .create_view(&wgpu::TextureViewDescriptor::default()),
            );
        }
        if pipeline.bind_group.is_none() {
            pipeline.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("blit-bg"),
                layout: &pipeline.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(
                            pipeline.view.as_ref().expect("view created above"),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&pipeline.sampler),
                    },
                ],
            }));
        }

        if sh.canvas.is_none() {
            sh.canvas = Some(GpuCanvas::new(
                device,
                queue,
                CANVAS_FORMAT,
                sh.app.cfg.atlas,
                sh.app.cfg.items + 64,
            ));
        }

        let view = pipeline.view.as_ref().expect("view created above").clone();
        let stats = sh.app.draw_frame(
            sh.canvas.as_mut().expect("canvas created above"),
            device,
            queue,
            &view,
            canvas_core::Vec2::new(w as f32, h as f32),
        );

        sh.last_uploads = stats.uploads;
        sh.last_evictions = stats.evictions;
        // The host owns the clock. `start` was taken at the top of `update`,
        // so this is the whole host frame, canvas work included.
        sh.metrics.push(start.elapsed(), stats);
        if sh.metrics.len() > 3_000 {
            sh.metrics.clear();
        }
        // Measured so the harness can compare host time against canvas time.
        let _ = start;
    }

    /// Blit inside iced's existing render pass, GPU-side. No CPU copy.
    fn draw(&self, pipeline: &Self::Pipeline, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        let Some(bind_group) = &pipeline.bind_group else {
            return true;
        };
        let _ = pipeline.format;
        render_pass.set_pipeline(&pipeline.blit);
        render_pass.set_bind_group(0, bind_group, &[]);
        render_pass.draw(0..3, 0..1);
        true
    }
}

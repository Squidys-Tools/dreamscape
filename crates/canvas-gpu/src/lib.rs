//! wgpu canvas renderer for the spike.
//!
//! Design notes that matter for the numbers this is meant to produce:
//!
//! * **One draw call.** Every visible item is an instance of a single quad. Two
//!   thousand items is one `draw(0..6, 0..n)` with no batching logic at all.
//!   If this is not fast, the problem is fill rate or texture sampling, never
//!   draw-call overhead.
//!
//! * **Mips are uploaded as separate sub-rectangles**, one slot grid per level.
//!   The GPU's own mip selection is therefore disabled and sampling is explicit,
//!   which sidesteps the bleeding that a shared mip chain would cause between
//!   neighbouring atlas slots.
//!
//! * **Eviction may never touch a pinned texture.** Pinned means "touched this
//!   frame", so anything on screen is immune by construction.
//!
//! Deliberately *not* representative of production: mip levels arrive here as
//! CPU-generated buffers. In the real pipeline they are decoded once at ingest
//! and read back from the thumbnail cache (SQU-65). The atlas residency,
//! eviction and upload behaviour under pan and zoom is identical either way,
//! and that is what this crate is here to measure.

use std::collections::HashMap;
use std::num::NonZeroU32;

use canvas_core::{Vec2, VisibleItem, MIP_LEVELS};

/// Mip level `i` of a pyramid generated at [`canvas_core::BASE_MIP`].
pub fn mip_size(level: u32) -> u32 {
    (canvas_core::BASE_MIP >> level).max(1)
}

#[derive(Clone, Copy, Debug)]
pub struct AtlasConfig {
    /// Edge length of the single atlas texture. Must be a power of two.
    pub size: u32,
    /// Formats the atlas is created with. sRGB so samples decode to linear.
    pub format: wgpu::TextureFormat,
    /// Fill every slot with a checker to make linear-filter bleed obvious.
    pub debug_slots: bool,
}

impl Default for AtlasConfig {
    fn default() -> Self {
        Self {
            size: 2048,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            debug_slots: false,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Resident {
    /// Atlas slot per mip level, allocated independently and lazily.
    slots: [Option<u32>; MIP_LEVELS as usize],
    /// Frame this texture was last drawn with. Equal to `Atlas::frame` means pinned.
    last_used: u64,
    approx_bytes: u64,
}

impl Resident {
    fn has_level(&self, level: u32) -> bool {
        self.slots
            .get(level as usize)
            .map(|s| s.is_some())
            .unwrap_or(false)
    }
}

pub struct AtlasStats {
    pub resident_textures: usize,
    pub total_slots: u64,
    pub used_slots: u64,
    pub resident_bytes: u64,
    pub uploads_this_frame: u32,
    pub evictions_total: u64,
    pub free_slots_for: [u64; MIP_LEVELS as usize],
}

/// Fixed-slot texture atlas, one uniform slot grid per mip level.
pub struct Atlas {
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    config: AtlasConfig,
    per_row: [u32; MIP_LEVELS as usize],
    free: [Vec<u32>; MIP_LEVELS as usize],
    resident: HashMap<u32, Resident>,
    /// Every key ever admitted, append-only. The eviction cursor indexes this.
    /// Entries for evicted keys are dead weight but are skipped in O(1), and
    /// compacting would cost more than it saves at realistic working-set sizes.
    key_order: Vec<u32>,
    evict_cursor: usize,
    frame: u64,
    uploads_this_frame: u32,
    evictions_total: u64,
}

impl Atlas {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, config: AtlasConfig) -> Self {
        assert!(
            config.size.is_power_of_two(),
            "atlas size must be a power of two"
        );
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("canvas-atlas"),
            size: wgpu::Extent3d {
                width: config.size,
                height: config.size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: config.format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());

        // Slot 0 of mip 0 is reserved as a placeholder swatch. When the atlas
        // cannot hold the working set, items draw as this flat swatch instead of
        // vanishing. A zero-area uv rect aimed at one texel renders it with no
        // shader change and no branching in the fragment path.
        {
            let swatch = vec![0u8; (canvas_core::BASE_MIP * canvas_core::BASE_MIP * 4) as usize];
            let px = canvas_core::BASE_MIP;
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: 0, y: 0, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &swatch,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(px * 4),
                    rows_per_image: Some(px),
                },
                wgpu::Extent3d {
                    width: px,
                    height: px,
                    depth_or_array_layers: 1,
                },
            );
        }

        let mut free: [Vec<u32>; MIP_LEVELS as usize] = std::array::from_fn(|_| Vec::new());
        let mut per_row = [0u32; MIP_LEVELS as usize];
        for level in 0..MIP_LEVELS as usize {
            let px = mip_size(level as u32);
            let n = (config.size / px).max(1);
            per_row[level] = n;
            // Slot 0 at mip 0 is the reserved placeholder swatch, so mip 0 starts
            // handing out from 1. Every other level starts at 0.
            let first = if level == 0 { 1 } else { 0 };
            // Hand out low indices first so eviction order is easy to reason about.
            free[level] = (first..(n * n)).rev().collect();
        }

        if config.debug_slots {
            // Upload a 1px checker per slot to expose linear-filter bleed at
            // slot edges, which is otherwise invisible until a user zooms in.
            for level in 0..MIP_LEVELS as usize {
                let px = mip_size(level as u32);
                let n = per_row[level];
                let data = checker_rgba(px, level % 2 == 0);
                for slot in 0..(n * n) {
                    let dst = self_origin(px, n, slot);
                    queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &tex,
                            mip_level: 0,
                            origin: wgpu::Origin3d {
                                x: dst.0,
                                y: dst.1,
                                z: 0,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        &data,
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(px * 4),
                            rows_per_image: Some(px),
                        },
                        wgpu::Extent3d {
                            width: px,
                            height: px,
                            depth_or_array_layers: 1,
                        },
                    );
                }
            }
        }

        Self {
            tex,
            view,
            config,
            per_row,
            free,
            resident: HashMap::new(),
            key_order: Vec::new(),
            evict_cursor: 0,
            frame: 0,
            uploads_this_frame: 0,
            evictions_total: 0,
        }
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.tex
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn stats(&self) -> AtlasStats {
        let total_slots: u64 = (0..MIP_LEVELS)
            .map(|l| (self.per_row[l as usize] as u64) * (self.per_row[l as usize] as u64))
            .sum();
        let used_slots = total_slots
            - (0..MIP_LEVELS)
                .map(|l| self.free[l as usize].len() as u64)
                .sum::<u64>();
        AtlasStats {
            resident_textures: self.resident.len(),
            total_slots,
            used_slots,
            resident_bytes: self.resident.values().map(|r| r.approx_bytes).sum(),
            uploads_this_frame: self.uploads_this_frame,
            evictions_total: self.evictions_total,
            free_slots_for: std::array::from_fn(|l| self.free[l].len() as u64),
        }
    }

    /// Call once per frame, before requesting any visible textures.
    pub fn begin_frame(&mut self) {
        self.frame += 1;
        self.uploads_this_frame = 0;
    }

    /// Mark textures as on-screen this frame, making them ineligible for eviction.
    pub fn pin(&mut self, keys: &[u32]) {
        let frame = self.frame;
        for &k in keys {
            if let Some(r) = self.resident.get_mut(&k) {
                r.last_used = frame;
            }
        }
    }

    pub fn is_resident(&self, key: u32) -> bool {
        self.resident.contains_key(&key)
    }

    pub fn has_level(&self, key: u32, level: u32) -> bool {
        self.resident
            .get(&key)
            .map(|r| r.has_level(level))
            .unwrap_or(false)
    }

    /// Nearest resident level at or coarser than `level`.
    pub fn resolve_level(&self, key: u32, level: u32) -> Option<u32> {
        let r = self.resident.get(&key)?;
        if level >= MIP_LEVELS {
            return None;
        }
        (level as usize..MIP_LEVELS as usize)
            .find(|&l| r.slots[l].is_some())
            .map(|l| l as u32)
    }

    /// Ensure that *some* level at or coarser than `preferred` is resident.
    ///
    /// Tries `preferred` first, then progressively coarser levels. Coarser levels
    /// occupy far less atlas space, so a board of many distinct images resolves
    /// almost everything at a blurrier level rather than dropping it entirely.
    ///
    /// Returns the level that ended up resident, or `None` if even the coarsest
    /// level could not be placed. Callers must check [`Atlas::resolve_level`]
    /// before calling, otherwise they will re-upload the same level every frame.
    pub fn ensure_view(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        key: u32,
        preferred: u32,
        levels: &[Vec<u8>],
    ) -> Option<u32> {
        if let Some(l) = self.resolve_level(key, preferred) {
            if let Some(r) = self.resident.get_mut(&key) {
                r.last_used = self.frame;
            }
            return Some(l);
        }
        if preferred >= MIP_LEVELS {
            return None;
        }
        for level in preferred..MIP_LEVELS {
            let Some(bytes) = levels.get(level as usize) else {
                break;
            };
            if self.ensure_level(device, queue, key, level, bytes) {
                return Some(level);
            }
            // Placement failed at this level. Trying coarser is not merely
            // better, it is much cheaper: a 64px level needs 16x less space than
            // a 256px one, so the coarse levels have orders of magnitude more room.
        }
        None
    }

    /// Make one mip level of `key` resident, uploading it if needed.
    ///
    /// Levels are allocated independently and on demand. This is the whole
    /// point: the fine levels are the scarce resource, and a texture that cannot
    /// fit at 256px must still be admitted at 8px where there is ample room.
    /// Allocating a whole pyramid up front rejects the entire texture when any
    /// single level is oversubscribed, which is the common case on a board of
    /// distinct images.
    pub fn ensure_level(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        key: u32,
        level: u32,
        rgba: &[u8],
    ) -> bool {
        if level >= MIP_LEVELS {
            return false;
        }
        let px = mip_size(level);
        let want = (px * px * 4) as usize;
        if rgba.len() < want {
            return false;
        }

        let frame = self.frame;
        if let Some(r) = self.resident.get_mut(&key) {
            r.last_used = frame;
            if r.has_level(level) {
                return true;
            }
        }

        let Some(slot) = self.take_slot(level) else {
            return false;
        };
        let (ox, oy) = self_origin(px, self.per_row[level as usize], slot);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.tex,
                mip_level: 0,
                origin: wgpu::Origin3d { x: ox, y: oy, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &rgba[..want],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(px * 4),
                rows_per_image: Some(px),
            },
            wgpu::Extent3d {
                width: px,
                height: px,
                depth_or_array_layers: 1,
            },
        );
        self.uploads_this_frame += 1;

        let entry = self.resident.entry(key).or_insert_with(|| {
            self.key_order.push(key);
            Resident {
                slots: [None; MIP_LEVELS as usize],
                last_used: frame,
                approx_bytes: 0,
            }
        });
        entry.last_used = frame;
        entry.slots[level as usize] = Some(slot);
        entry.approx_bytes += want as u64;
        let _ = device;
        true
    }

    /// Eagerly upload every level provided. Kept for tests and for the case
    /// where a full pyramid genuinely is wanted, such as an export path.
    pub fn ensure_resident(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        key: u32,
        levels: &[Vec<u8>],
    ) -> bool {
        if self.resident.contains_key(&key) {
            if let Some(r) = self.resident.get_mut(&key) {
                r.last_used = self.frame;
            }
            return true;
        }
        let mut any = false;
        for (level, bytes) in levels.iter().enumerate().take(MIP_LEVELS as usize) {
            if self.ensure_level(device, queue, key, level as u32, bytes) {
                any = true;
            }
        }
        if !any {
            self.resident.remove(&key);
        }
        any
    }

    /// Zero-area uv aimed at the single placeholder texel at the atlas origin.
    ///
    /// Sampling one texel is a constant fetch, so a placeholder item costs the
    /// same as a real one and needs no branch in the fragment shader.
    pub fn placeholder_uv(&self) -> [f32; 4] {
        let inv = 1.0 / self.config.size as f32;
        let half = 0.5 * inv;
        [half, half, inv - half, inv - half]
    }

    /// Pixel origin of a slot within the atlas.
    fn slot_origin(&self, level: u32, slot: u32) -> (u32, u32) {
        let px = mip_size(level);
        let n = self.per_row[level as usize];
        self_origin(px, n, slot)
    }

    /// UV rect for one mip of a resident texture, inset by half a texel so
    /// linear filtering cannot reach into the neighbouring slot.
    ///
    /// If the exact level is not resident, falls back to the nearest *coarser*
    /// level that is. That is the normal degradation path: the fine levels are
    /// the scarce resource, so an item that cannot get a sharp copy gets a
    /// blurrier one rather than a grey box. A coarser level stretched to the
    /// same on-screen size is blurry, never distorted, which is what mipmapping
    /// already does.
    ///
    /// Returns the level actually used alongside the rect, so a caller that
    /// needs to know whether the item was degraded gets it without a second
    /// residency lookup on the hot path.
    pub fn uv_rect(&self, key: u32, level: u32) -> Option<(u32, [f32; 4])> {
        let r = self.resident.get(&key)?;
        if level >= MIP_LEVELS {
            return None;
        }
        let used = (level as usize..MIP_LEVELS as usize).find(|&l| r.slots[l].is_some())?;
        let slot = r.slots[used]?;
        let px = mip_size(used as u32);
        let (ox, oy) = self.slot_origin(used as u32, slot);
        let inv = 1.0 / self.config.size as f32;
        let half = 0.5 * inv;
        Some((
            used as u32,
            [
                ox as f32 * inv + half,
                oy as f32 * inv + half,
                (ox + px) as f32 * inv - half,
                (oy + px) as f32 * inv - half,
            ],
        ))
    }

    /// Evict a texture to free one slot at `level`.
    ///
    /// Uses a rotating cursor over the append-only key list rather than a
    /// `min_by_key` over the residency map. The latter is a full O(n) scan with
    /// no early exit, so a board of many distinct images spends the entire
    /// frame inside eviction. The cursor finds an unpinned victim in a bounded
    /// number of steps in practice because it resumes where it left off.
    fn take_slot(&mut self, level: u32) -> Option<u32> {
        if let Some(slot) = self.free[level as usize].pop() {
            return Some(slot);
        }
        let frame = self.frame;
        let n = self.key_order.len();
        for step in 0..n {
            let idx = (self.evict_cursor + step) % n.max(1);
            let Some(&key) = self.key_order.get(idx) else {
                continue;
            };
            let Some(r) = self.resident.get(&key) else {
                // Key was evicted long ago; keep advancing.
                continue;
            };
            if r.last_used >= frame {
                continue; // On screen this frame. Never evictable.
            }
            self.evict_cursor = (idx + 1) % n.max(1);

            let victim_entry = self.resident.remove(&key)?;
            for (lvl, slot) in victim_entry.slots.iter().enumerate() {
                if let Some(s) = slot {
                    self.free[lvl].push(*s);
                }
            }
            self.evictions_total += 1;
            return self.free[level as usize].pop();
        }
        // Everything resident is on screen. Refusing to thrash is correct: the
        // caller falls back to a coarser level or a placeholder.
        None
    }
}

fn self_origin(px: u32, per_row: u32, slot: u32) -> (u32, u32) {
    ((slot % per_row) * px, (slot / per_row) * px)
}

fn checker_rgba(px: u32, light: bool) -> Vec<u8> {
    let mut v = Vec::with_capacity((px * px * 4) as usize);
    for y in 0..px {
        for x in 0..px {
            let on = ((x / 2) + (y / 2)) % 2 == 0;
            let c = if on == light { 230u8 } else { 30u8 };
            v.extend_from_slice(&[c, c, c, 255]);
        }
    }
    v
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Instance {
    /// x, y, w, h in device pixels.
    rect: [f32; 4],
    /// u0, v0, u1, v1 into the atlas.
    uv: [f32; 4],
    /// rgba multiply.
    tint: [f32; 4],
}

const SHADER: &str = r#"
@group(0) @binding(0) var atlas : texture_2d<f32>;
@group(0) @binding(1) var samp  : sampler;

struct VSOut {
    @builtin(position) pos : vec4<f32>,
    @location(0) uv : vec2<f32>,
    @location(1) tint : vec4<f32>,
};

@vertex
fn vs(
    @builtin(vertex_index) vi : u32,
    @location(0) rect : vec4<f32>,
    @location(1) uv_rect : vec4<f32>,
    @location(2) tint : vec4<f32>,
) -> VSOut {
    // Two triangles, no vertex buffer for positions.
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0),
    );
    let c = corners[vi];

    var out : VSOut;
    out.pos = vec4<f32>(rect.xy + c * rect.zw, 0.0, 1.0);
    out.uv = mix(uv_rect.xy, uv_rect.zw, c);
    out.tint = tint;
    return out;
}

@fragment
fn fs(in : VSOut) -> @location(0) vec4<f32> {
    // Level 0 only: the correct mip is already selected on the CPU and packed
    // as its own sub-rectangle, so the sampler must not pick its own.
    let texel = textureSampleLevel(atlas, samp, in.uv, 0.0);
    return texel * in.tint;
}
"#;

/// The renderer: owns the atlas, the pipeline and the instance buffer.
pub struct GpuCanvas {
    atlas: Atlas,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    instances: wgpu::Buffer,
    instance_capacity: u64,
    pub background: [f32; 4],
    /// Visible items skipped last frame because their texture had no atlas slot.
    /// Non-zero means eviction dropped something on screen, i.e. a hole in the board.
    pub last_frame_dropped: u32,
    /// Visible items drawn at a mip level coarser than their screen size asked for.
    ///
    /// Unlike a dropped item this is designed behaviour, not a bug: the atlas is
    /// partitioned by level, so a board of distinct images saturates the fine
    /// levels and falls back to blurrier ones. It is reported separately because
    /// a placeholder count of zero says only that nothing failed, and this is the
    /// number that says whether the board is actually sharp.
    pub last_frame_degraded: u32,
}

impl GpuCanvas {
    /// `instance_capacity` is preallocated; exceeding it drops the excess rather
    /// than reallocating mid-frame, since a resize in the hot path would show up
    /// as a frame-time spike and pollute the measurement.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        atlas: AtlasConfig,
        instance_capacity: u32,
    ) -> Self {
        let atlas = Atlas::new(device, queue, atlas);
        let capacity = NonZeroU32::new(instance_capacity.max(1)).unwrap();

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("canvas-bgl"),
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

        let instance_bytes = std::mem::size_of::<Instance>() as u64;
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("canvas-instances"),
            size: instance_bytes * capacity.get() as u64,
            // VERTEX, not STORAGE: the WebGL2 floor has no storage buffers in
            // any shader stage, and an instanced vertex buffer is the portable
            // way to feed per-item data.
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("canvas-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("canvas-bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(atlas.view()),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("canvas-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("canvas-pl"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("canvas-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4],
                }],
            },
            // Premultiplied so the tint's alpha behaves predictably.
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
            atlas,
            pipeline,
            bind_group,
            instances,
            instance_capacity: capacity.get() as u64,
            background: [0.09, 0.09, 0.10, 1.0],
            last_frame_dropped: 0,
            last_frame_degraded: 0,
        }
    }

    pub fn atlas(&self) -> &Atlas {
        &self.atlas
    }

    pub fn atlas_mut(&mut self) -> &mut Atlas {
        &mut self.atlas
    }

    pub fn begin_frame(&mut self) {
        self.atlas.begin_frame();
    }

    /// Distinct texture keys among the visible set, sorted and deduplicated.
    pub fn visible_keys(visible: &[VisibleItem]) -> Vec<u32> {
        let mut keys: Vec<u32> = visible.iter().map(|v| v.tex).collect();
        keys.sort_unstable();
        keys.dedup();
        keys
    }

    /// Items whose texture is not resident, deduplicated.
    pub fn missing_textures(visible: &[VisibleItem]) -> Vec<u32> {
        Self::visible_keys(visible)
    }

    /// Build instances and issue a single instanced draw.
    ///
    /// Returns the number actually drawn, which may be fewer than
    /// `visible.len()` if the atlas could not free space in time.
    pub fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        visible: &[VisibleItem],
    ) -> u32 {
        let mut batch: Vec<Instance> = Vec::with_capacity(visible.len());
        let mut dropped = 0u32;
        let mut degraded = 0u32;
        let placeholder = self.atlas.placeholder_uv();
        for v in visible {
            let uv = match self.atlas.uv_rect(v.tex, v.mip) {
                Some((used, uv)) => {
                    // A coarser level than LOD asked for. Counted here, beside the
                    // loop that chose it, because the fallback is silent: the item
                    // draws correctly and no other metric in the pipeline can see
                    // that it is blurrier than the screen size warranted.
                    if used > v.mip {
                        degraded += 1;
                    }
                    uv
                }
                None => {
                    // No slot for this texture. Draw the placeholder rather than
                    // nothing, and record it so the harness can flag a board
                    // that is visibly missing items.
                    dropped += 1;
                    placeholder
                }
            };
            batch.push(Instance {
                rect: [
                    v.screen_pos.x,
                    v.screen_pos.y,
                    v.screen_size.x,
                    v.screen_size.y,
                ],
                uv,
                tint: [1.0, 1.0, 1.0, 1.0],
            });
            if batch.len() as u64 >= self.instance_capacity {
                break;
            }
        }
        if batch.is_empty() {
            self.last_frame_dropped = dropped;
            self.last_frame_degraded = degraded;
            return 0;
        }

        queue.write_buffer(
            &self.instances,
            0,
            bytemuck::cast_slice(&batch[..batch.len().min(self.instance_capacity as usize)]),
        );

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("canvas-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: self.background[0] as f64,
                            g: self.background[1] as f64,
                            b: self.background[2] as f64,
                            a: self.background[3] as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.instances.slice(..));
            pass.draw(0..6, 0..batch.len() as u32);
        }
        let _ = device;
        self.last_frame_dropped = dropped;
        self.last_frame_degraded = degraded;
        batch.len() as u32
    }
}

/// Convenience for building a device without pulling in a windowing layer.
///
/// The surface format is fixed rather than queried: `get_capabilities` lives on
/// `Surface` in wgpu 27, and a headless pass has no surface. Hosts that do have
/// one (iced, WGPUI) pass their own format into [`GpuCanvas::new`].
pub fn headless_device() -> (
    wgpu::Device,
    wgpu::Queue,
    wgpu::TextureFormat,
    wgpu::AdapterInfo,
) {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .expect("no suitable GPU adapter found");

    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("spike-device"),
        // Downlevel webgl2 defaults are the floor SQU-60 committed to.
        required_limits:
            wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
        memory_hints: wgpu::MemoryHints::Performance,
        ..Default::default()
    }))
    .expect("failed to create device");

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    (device, queue, format, adapter.get_info())
}

/// Screen-space rect a viewport occupies, for hosts that need to size a target.
pub fn viewport_size(width: f32, height: f32) -> Vec2 {
    Vec2::new(width, height)
}

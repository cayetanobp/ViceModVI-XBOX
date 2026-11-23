//! Orbiting mesh preview rendered with wgpu into an egui texture.

use eframe::{
    egui::{self, Rect, Sense},
    egui_wgpu::{self, wgpu, wgpu::util::DeviceExt},
};
use glam::{Mat4, Vec3};
use vicemodvi_core::Mesh;

const SAMPLES: u32 = 4;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const BACKDROP: [f64; 3] = [0xD2 as f64 / 255.0, 0xDD as f64 / 255.0, 0xF0 as f64 / 255.0];
/// Untextured surface color of the WPF viewer.
const PLAIN: [f32; 4] = [123.0 / 255.0, 144.0 / 255.0, 152.0 / 255.0, 1.0];

const SHADER: &str = r#"
struct Uniforms {
    view_proj: mat4x4<f32>,
    base_color: vec4<f32>,
    light_dir: vec4<f32>,
    mode: vec4<u32>,
};
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var surface: texture_2d<f32>;
@group(0) @binding(2) var surface_sampler: sampler;

struct VertexIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
};
struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
};

@vertex
fn vs_main(v: VertexIn) -> VertexOut {
    var out: VertexOut;
    out.clip = u.view_proj * vec4<f32>(v.position, 1.0);
    out.normal = v.normal;
    out.uv = v.uv;
    out.color = v.color;
    return out;
}

@fragment
fn fs_main(v: VertexOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let sampled = textureSample(surface, surface_sampler, v.uv);
    var base = u.base_color;
    if (u.mode.x == 1u) {
        base = v.color;
    } else if (u.mode.x == 2u) {
        base = sampled;
        if (base.a < 0.5) {
            discard;
        }
    }
    var n = normalize(v.normal);
    if (!front) {
        n = -n;
    }
    // Ambient 185/255 plus a 140/255 directional light, like the WPF scene.
    let diffuse = max(dot(n, -u.light_dir.xyz), 0.0);
    let light = min(vec3<f32>(0.7255) + vec3<f32>(0.549) * diffuse, vec3<f32>(1.0));
    return vec4<f32>(base.rgb * light, 1.0);
}
"#;

#[repr(C)]
#[derive(Clone, Copy)]
struct Uniforms {
    view_proj: [[f32; 4]; 4],
    base_color: [f32; 4],
    light_dir: [f32; 4],
    mode: [u32; 4],
}

impl Uniforms {
    fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(112);
        for column in self.view_proj {
            for v in column {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        for v in self.base_color.iter().chain(&self.light_dir) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in self.mode {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }
}

/// How the surface of the mesh is colored.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Surface {
    Plain,
    VertexColors,
    Texture,
}

pub struct Camera {
    pub center: Vec3,
    pub distance: f32,
    pub yaw: f32,
    pub pitch: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self { center: Vec3::ZERO, distance: 1.0, yaw: -1.05, pitch: 0.34 }
    }
}

impl Camera {
    /// Frames the mesh bounds from the default viewing angle.
    pub fn fit(&mut self, mesh: &Mesh) {
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for p in &mesh.positions {
            let p = Vec3::from_array(*p);
            min = min.min(p);
            max = max.max(p);
        }
        if !min.is_finite() || !max.is_finite() {
            return;
        }
        self.center = (min + max) / 2.0;
        self.distance = ((max - min).length() * 2.0).max(0.0001);
        self.yaw = -1.05;
        self.pitch = 0.34;
    }

    fn eye_offset(&self) -> Vec3 {
        Vec3::new(self.yaw.cos() * self.pitch.cos(), self.yaw.sin() * self.pitch.cos(), self.pitch.sin()) * self.distance
    }

    /// WPF's 40° field of view is horizontal.
    fn view_proj(&self, aspect: f32) -> Mat4 {
        let eye = self.center + self.eye_offset();
        let horizontal = 40f32.to_radians();
        let vertical = 2.0 * ((horizontal / 2.0).tan() / aspect.max(0.01)).atan();
        let projection = Mat4::perspective_rh(vertical, aspect.max(0.01), self.distance / 300.0, self.distance * 50.0);
        projection * Mat4::look_at_rh(eye, self.center, Vec3::Z)
    }

    pub fn orbit(&mut self, delta: egui::Vec2) {
        self.yaw -= delta.x * 0.01;
        self.pitch = (self.pitch + delta.y * 0.01).clamp(-1.5, 1.5);
    }

    pub fn zoom(&mut self, notches: f32) {
        self.distance = (self.distance * 1.18f32.powf(-notches)).clamp(0.000_001, 1e9);
    }
}

struct Target {
    size: [u32; 2],
    color_view: wgpu::TextureView,
    msaa_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    texture_id: egui::TextureId,
}

struct GpuMesh {
    key: String,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
}

struct Material {
    key: String,
    bind_group: wgpu::BindGroup,
}

pub struct Viewer {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    uniforms: wgpu::Buffer,
    sampler: wgpu::Sampler,
    plain: wgpu::BindGroup,
    target: Option<Target>,
    mesh: Option<GpuMesh>,
    material: Option<Material>,
    pub max_texture_side: u32,
}

/// Material image: BGRA pixels with a cache key.
pub struct SurfaceImage<'a> {
    pub key: &'a str,
    pub width: u32,
    pub height: u32,
    pub bgra: &'a [u8],
}

impl Viewer {
    pub fn new(state: &egui_wgpu::RenderState) -> Self {
        let device = &state.device;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mesh preview"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mesh preview"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mesh preview"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let attributes = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Unorm8x4];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mesh preview"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout { array_stride: 36, step_mode: wgpu::VertexStepMode::Vertex, attributes: &attributes })],
            },
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState { count: SAMPLES, ..Default::default() },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: COLOR_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mesh preview uniforms"),
            size: 112,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("mesh preview"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let white = Self::upload_texture(state, 1, 1, &[255, 255, 255, 255]);
        let plain = Self::bind(device, &layout, &uniforms, &white, &sampler);
        Self {
            pipeline,
            layout,
            uniforms,
            sampler,
            plain,
            target: None,
            mesh: None,
            material: None,
            max_texture_side: device.limits().max_texture_dimension_2d,
        }
    }

    fn bind(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, uniforms: &wgpu::Buffer, view: &wgpu::TextureView, sampler: &wgpu::Sampler) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mesh preview"),
            layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(sampler) },
            ],
        })
    }

    /// Uploads RGBA pixels with a box-filtered mip chain.
    fn upload_texture(state: &egui_wgpu::RenderState, width: u32, height: u32, rgba: &[u8]) -> wgpu::TextureView {
        let levels = 32 - width.max(height).leading_zeros();
        let texture = state.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mesh preview surface"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let (mut w, mut h, mut level) = (width, height, rgba.to_vec());
        for mip in 0..levels {
            state.queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: mip, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                &level,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * w), rows_per_image: Some(h) },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
            if mip + 1 < levels {
                let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
                level = halve(&level, w as usize, h as usize, nw as usize, nh as usize);
                (w, h) = (nw, nh);
            }
        }
        texture.create_view(&Default::default())
    }

    pub fn set_mesh(&mut self, state: &egui_wgpu::RenderState, key: &str, mesh: &Mesh, colors: bool) {
        let key = format!("{key}|{colors}");
        if self.mesh.as_ref().is_some_and(|m| m.key == key) {
            return;
        }
        let normals = vertex_normals(mesh);
        let uv = mesh.tex_coords.as_ref().filter(|uv| uv.len() == mesh.positions.len());
        let vertex_colors = mesh.vertex_colors.as_ref().filter(|c| colors && c.len() == mesh.positions.len());
        let mut bytes = Vec::with_capacity(mesh.positions.len() * 36);
        for (i, p) in mesh.positions.iter().enumerate() {
            for v in p.iter().chain(&normals[i]) {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            let t = uv.map_or([0.0, 0.0], |uv| uv[i]);
            bytes.extend_from_slice(&t[0].to_le_bytes());
            bytes.extend_from_slice(&t[1].to_le_bytes());
            // Vertex colors keep red in the low byte.
            let c = vertex_colors.map_or(u32::MAX, |c| c[i] | 0xFF00_0000);
            bytes.extend_from_slice(&c.to_le_bytes());
        }
        let indices: Vec<u8> = mesh.indices.iter().flat_map(|&i| (i as u32).to_le_bytes()).collect();
        let device = &state.device;
        self.mesh = Some(GpuMesh {
            key,
            vertices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mesh preview vertices"),
                contents: &bytes,
                usage: wgpu::BufferUsages::VERTEX,
            }),
            indices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mesh preview indices"),
                contents: &indices,
                usage: wgpu::BufferUsages::INDEX,
            }),
            count: mesh.indices.len() as u32,
        });
    }

    pub fn set_surface(&mut self, state: &egui_wgpu::RenderState, image: Option<SurfaceImage>) {
        let Some(image) = image else {
            self.material = None;
            return;
        };
        if self.material.as_ref().is_some_and(|m| m.key == image.key) {
            return;
        }
        let mut rgba = image.bgra.to_vec();
        for p in rgba.as_chunks_mut::<4>().0 {
            p.swap(0, 2);
        }
        let view = Self::upload_texture(state, image.width, image.height, &rgba);
        let bind_group = Self::bind(&state.device, &self.layout, &self.uniforms, &view, &self.sampler);
        self.material = Some(Material { key: image.key.to_string(), bind_group });
    }

    fn ensure_target(&mut self, state: &egui_wgpu::RenderState, size: [u32; 2]) -> egui::TextureId {
        if let Some(target) = &self.target {
            if target.size == size {
                return target.texture_id;
            }
        }
        let device = &state.device;
        let make = |format, samples, usage, label| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: samples,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let color_view = make(COLOR_FORMAT, 1, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING, "mesh preview color");
        let msaa_view = make(COLOR_FORMAT, SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT, "mesh preview msaa");
        let depth_view = make(DEPTH_FORMAT, SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT, "mesh preview depth");
        let mut renderer = state.renderer.write();
        let texture_id = match self.target.take() {
            Some(old) => {
                renderer.update_egui_texture_from_wgpu_texture(device, &color_view, wgpu::FilterMode::Linear, old.texture_id);
                old.texture_id
            }
            None => renderer.register_native_texture(device, &color_view, wgpu::FilterMode::Linear),
        };
        self.target = Some(Target { size, color_view, msaa_view, depth_view, texture_id });
        texture_id
    }

    /// Draws the current mesh into `rect` and handles orbit and zoom input.
    pub fn show(&mut self, ui: &mut egui::Ui, state: &egui_wgpu::RenderState, rect: Rect, camera: &mut Camera, surface: Surface) {
        let response = ui.interact(rect, ui.make_persistent_id("mesh preview"), Sense::drag());
        if response.dragged_by(egui::PointerButton::Primary) {
            camera.orbit(response.drag_delta());
        }
        if response.hovered() {
            let scroll = ui.ctx().input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                camera.zoom(scroll / 50.0);
            }
        }
        let scale = ui.ctx().pixels_per_point();
        let limit = self.max_texture_side.max(1);
        let size = [((rect.width() * scale) as u32).clamp(1, limit), ((rect.height() * scale) as u32).clamp(1, limit)];
        let texture_id = self.ensure_target(state, size);
        let uniforms = Uniforms {
            view_proj: camera.view_proj(size[0] as f32 / size[1] as f32).to_cols_array_2d(),
            base_color: PLAIN,
            light_dir: Vec3::new(-1.0, -2.0, -3.0).normalize().extend(0.0).to_array(),
            mode: [
                match (surface, self.material.is_some()) {
                    (Surface::VertexColors, _) => 1,
                    (Surface::Texture, true) => 2,
                    _ => 0,
                },
                0,
                0,
                0,
            ],
        };
        state.queue.write_buffer(&self.uniforms, 0, &uniforms.bytes());
        let (Some(target), Some(mesh)) = (&self.target, &self.mesh) else { return };
        let bind_group = match (surface, &self.material) {
            (Surface::Texture, Some(material)) => &material.bind_group,
            _ => &self.plain,
        };
        let mut encoder = state.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("mesh preview") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("mesh preview"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.msaa_view,
                    depth_slice: None,
                    resolve_target: Some(&target.color_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: BACKDROP[0], g: BACKDROP[1], b: BACKDROP[2], a: 1.0 }),
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &target.depth_view,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.set_vertex_buffer(0, mesh.vertices.slice(..));
            pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..mesh.count, 0, 0..1);
        }
        state.queue.submit([encoder.finish()]);
        ui.painter().image(texture_id, rect, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
    }
}

/// Smooth normals: area-weighted face normals summed per vertex.
fn vertex_normals(mesh: &Mesh) -> Vec<[f32; 3]> {
    let mut sums = vec![Vec3::ZERO; mesh.positions.len()];
    for face in mesh.indices.as_chunks::<3>().0 {
        let [a, b, c] = [face[0] as usize, face[1] as usize, face[2] as usize];
        let (pa, pb, pc) = (Vec3::from_array(mesh.positions[a]), Vec3::from_array(mesh.positions[b]), Vec3::from_array(mesh.positions[c]));
        let normal = (pb - pa).cross(pc - pa);
        for i in [a, b, c] {
            sums[i] += normal;
        }
    }
    sums.into_iter().map(|n| n.try_normalize().unwrap_or(Vec3::Z).to_array()).collect()
}

/// Halves an RGBA image with a box filter.
fn halve(rgba: &[u8], w: usize, h: usize, nw: usize, nh: usize) -> Vec<u8> {
    let mut out = vec![0u8; nw * nh * 4];
    for y in 0..nh {
        for x in 0..nw {
            let mut sum = [0u32; 4];
            let mut count = 0;
            for sy in (y * 2)..((y * 2 + 2).min(h)) {
                for sx in (x * 2)..((x * 2 + 2).min(w)) {
                    let p = &rgba[(sy * w + sx) * 4..(sy * w + sx) * 4 + 4];
                    for i in 0..4 {
                        sum[i] += p[i] as u32;
                    }
                    count += 1;
                }
            }
            for i in 0..4 {
                out[(y * nw + x) * 4 + i] = ((sum[i] + count / 2) / count.max(1)) as u8;
            }
        }
    }
    out
}

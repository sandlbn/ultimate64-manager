//! GPU video path — a custom iced `shader` widget that renders the native VIC
//! frame through a persistent wgpu texture.
//!
//! The stream thread publishes native 384×272 RGBA frames; this widget uploads
//! the latest frame to a **persistent** GPU texture (only when it changed) and
//! draws a single integer-fit quad, sampling nearest. Scaling is pixel-perfect
//! (largest whole multiple that fits, centered, letterboxed) and the Scale2x /
//! Scanlines / CRT effects run in the fragment shader — so no per-frame CPU
//! scaling, buffer clones, or texture re-allocation happen at all.
//!
//! Requires iced's wgpu backend (enabled by default). On the tiny-skia fallback
//! this widget can't render; the streaming views fall back to the compatibility
//! image path instead (see `VideoStreaming::use_gpu_shader`).

use std::sync::Arc;

use iced::widget::shader::{Pipeline, Primitive, Program, Viewport};
use iced::{Element, Length, Rectangle};

use crate::streaming::{ScaleMode, StreamingMessage};

/// Native VIC frame size — must match `streaming::VIC_WIDTH/VIC_HEIGHT`.
const TEX_W: u32 = crate::streaming::VIC_WIDTH;
const TEX_H: u32 = crate::streaming::VIC_HEIGHT;

/// The video display element backed by the wgpu shader widget.
pub fn vic_video<'a>(
    frame: Arc<Vec<u8>>,
    version: u64,
    mode: ScaleMode,
) -> Element<'a, StreamingMessage> {
    iced::widget::shader(VicProgram {
        frame,
        version,
        effect: mode.to_u8() as u32,
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

// ─── Program ────────────────────────────────────────────────────────────────

struct VicProgram {
    frame: Arc<Vec<u8>>,
    version: u64,
    effect: u32,
}

impl Program<StreamingMessage> for VicProgram {
    type State = ();
    type Primitive = VicPrimitive;

    fn draw(
        &self,
        _state: &Self::State,
        _cursor: iced::mouse::Cursor,
        _bounds: Rectangle,
    ) -> Self::Primitive {
        VicPrimitive {
            frame: self.frame.clone(),
            version: self.version,
            effect: self.effect,
        }
    }
}

// ─── Primitive ──────────────────────────────────────────────────────────────

#[derive(Debug)]
struct VicPrimitive {
    frame: Arc<Vec<u8>>,
    version: u64,
    effect: u32,
}

impl Primitive for VicPrimitive {
    type Pipeline = VicPipeline;

    fn prepare(
        &self,
        pipeline: &mut Self::Pipeline,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        viewport: &Viewport,
    ) {
        // Upload the frame only when it actually changed.
        if self.version != pipeline.last_version {
            pipeline.last_version = self.version;
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &pipeline.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &self.frame,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(TEX_W * 4),
                    rows_per_image: Some(TEX_H),
                },
                wgpu::Extent3d {
                    width: TEX_W,
                    height: TEX_H,
                    depth_or_array_layers: 1,
                },
            );
        }

        // Compute the pixel-perfect integer-fit quad in NDC. iced sets the render
        // pass viewport to this widget's bounds for the `draw()` path, so NDC
        // [-1,1] maps to the bounds — the quad is just a centered box whose half-
        // extents are the integer-scaled size over the bounds size. Integer scale
        // is computed in *physical* pixels so each source texel is a whole number
        // of device pixels (crisp).
        let sf = viewport.scale_factor() as f32;
        let bw = bounds.width * sf;
        let bh = bounds.height * sf;

        let scale = (bw / TEX_W as f32)
            .floor()
            .min((bh / TEX_H as f32).floor())
            .max(1.0);
        let dw = TEX_W as f32 * scale;
        let dh = TEX_H as f32 * scale;

        let half_x = (dw / bw).min(1.0);
        let half_y = (dh / bh).min(1.0);
        // rect = (x0, y0_top, x1, y1_bottom)
        let uniforms = Uniforms {
            rect: [-half_x, half_y, half_x, -half_y],
            out_size: [dw, dh],
            effect: self.effect,
            _pad: 0,
        };
        queue.write_buffer(&pipeline.uniform_buf, 0, bytemuck::bytes_of(&uniforms));
    }

    fn draw(&self, pipeline: &Self::Pipeline, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        // Draw inside iced's existing render pass (viewport/scissor already set to
        // our bounds). Reusing the pass avoids opening a second one, which on
        // tile-based GPUs (Apple Silicon) would reload+store the whole framebuffer
        // every frame — the main render cost at large window sizes.
        render_pass.set_pipeline(&pipeline.pipeline);
        render_pass.set_bind_group(0, &pipeline.bind_group, &[]);
        render_pass.draw(0..6, 0..1);
        true
    }
}

// ─── Pipeline (persistent GPU state, created once) ──────────────────────────

struct VicPipeline {
    pipeline: wgpu::RenderPipeline,
    texture: wgpu::Texture,
    uniform_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    last_version: u64,
}

impl Pipeline for VicPipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        // Match the texture's sRGB-ness to the render target so colors round-trip
        // unchanged. iced picks a *linear* (non-sRGB) surface format when gamma
        // correction is on (its default) and encodes gamma in its own shaders. If
        // we used an sRGB texture against a linear target, sampling would
        // linearize the values but the target would never re-encode them, making
        // the whole picture much darker. So: sRGB target → sRGB texture; linear
        // target → linear (Unorm) texture, which passes the raw C64 palette bytes
        // straight through (and makes the effects match the CPU kernels, which
        // also operate in gamma space).
        let tex_format = if format.is_srgb() {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vic_shader texture"),
            size: wgpu::Extent3d {
                width: TEX_W,
                height: TEX_H,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: tex_format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("vic_shader sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vic_shader uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vic_shader bgl"),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vic_shader bg"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform_buf.as_entire_binding(),
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vic_shader wgsl"),
            source: wgpu::ShaderSource::Wgsl(SHADER_WGSL.into()),
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vic_shader layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vic_shader pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });

        Self {
            pipeline,
            texture,
            uniform_buf,
            bind_group,
            last_version: u64::MAX, // force first upload
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    /// Quad in NDC: (x0, y0_top, x1, y1_bottom).
    rect: [f32; 4],
    /// Displayed quad size in physical pixels — lets the CRT shadow mask key off
    /// the true output resolution.
    out_size: [f32; 2],
    /// 0 = pixel-perfect, 1 = Scale2x, 2 = Scanlines, 3 = CRT.
    effect: u32,
    _pad: u32,
}

const SHADER_WGSL: &str = r#"
struct Uniforms {
    rect: vec4<f32>,
    out_size: vec2<f32>,
    effect: u32,
    _pad: u32,
};

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> u: Uniforms;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // Two triangles over the rect. uv has y=0 at the top (matches frame row 0).
    var corner = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0),
    );
    let c = corner[vi];
    let ndc_x = mix(u.rect.x, u.rect.z, c.x);
    let ndc_y = mix(u.rect.y, u.rect.w, c.y);
    var out: VsOut;
    out.pos = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = c;
    return out;
}

fn eq(a: vec3<f32>, b: vec3<f32>) -> bool {
    let d = abs(a - b);
    return d.x < 0.02 && d.y < 0.02 && d.z < 0.02;
}

const TEX_SIZE: vec2<f32> = vec2<f32>(384.0, 272.0);

// Scale2x / EPX, mirroring video_scaling::scale2x.
fn epx(uv: vec2<f32>) -> vec3<f32> {
    let texel = 1.0 / TEX_SIZE;
    let p = textureSample(tex, samp, uv).rgb;
    let a = textureSample(tex, samp, uv + vec2<f32>(0.0, -texel.y)).rgb; // top
    let b = textureSample(tex, samp, uv + vec2<f32>(texel.x, 0.0)).rgb;  // right
    let c = textureSample(tex, samp, uv + vec2<f32>(-texel.x, 0.0)).rgb; // left
    let d = textureSample(tex, samp, uv + vec2<f32>(0.0, texel.y)).rgb;  // bottom
    let f = fract(uv * TEX_SIZE);
    var res = p;
    if (f.x < 0.5 && f.y < 0.5) {
        if (eq(a, c) && !eq(a, b) && !eq(c, d)) { res = a; }
    } else if (f.x >= 0.5 && f.y < 0.5) {
        if (eq(a, b) && !eq(a, c) && !eq(b, d)) { res = b; }
    } else if (f.x < 0.5 && f.y >= 0.5) {
        if (eq(c, d) && !eq(a, c) && !eq(b, d)) { res = c; }
    } else {
        if (eq(b, d) && !eq(a, b) && !eq(c, d)) { res = d; }
    }
    return res;
}

// Barrel distortion: bend x by y^2 and y by x^2 around the center.
fn curve(uv0: vec2<f32>) -> vec2<f32> {
    var c = uv0 * 2.0 - 1.0;
    c = c + c * vec2<f32>(c.y * c.y, c.x * c.x) * vec2<f32>(0.03, 0.045);
    return c * 0.5 + 0.5;
}

// The "tube" shading shared by CRT and Glow: per-scanline shading, an RGB shadow
// mask keyed to the real output resolution, a soft vignette, and a brightness
// boost to compensate for the darkening.
fn tube(color_in: vec3<f32>, uv: vec2<f32>) -> vec3<f32> {
    var color = color_in;

    let line = fract(uv.y * TEX_SIZE.y);
    color = color * mix(0.6, 1.0, sin(line * 3.14159));

    let m = i32(floor(uv.x * u.out_size.x)) % 3;
    var mask = vec3<f32>(0.8, 0.8, 0.8);
    if (m == 0) { mask = vec3<f32>(1.0, 0.8, 0.8); }
    else if (m == 1) { mask = vec3<f32>(0.8, 1.0, 0.8); }
    else { mask = vec3<f32>(0.8, 0.8, 1.0); }
    color = color * mask;

    let vc = uv - 0.5;
    color = color * mix(0.55, 1.0, clamp(1.0 - dot(vc, vc) * 0.9, 0.0, 1.0));

    return min(color * 1.4, vec3<f32>(1.0, 1.0, 1.0));
}

// A proper CRT: curvature + tube shading, one texture tap.
fn crt(uv0: vec2<f32>) -> vec4<f32> {
    let uv = curve(uv0);
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0); // black bezel
    }
    let color = textureSample(tex, samp, uv).rgb;
    return vec4<f32>(tube(color, uv), 1.0);
}

// Bright portion of a colour (soft threshold) — the part that "glows".
fn bright(c: vec3<f32>) -> vec3<f32> {
    return max(c - 0.30, vec3<f32>(0.0, 0.0, 0.0));
}

// Glow: the CRT tube plus a strong phosphor bloom — bright pixels bleed light
// like a luminous CRT. Gathers bright neighbours at a few radii (~25 taps).
fn fx_glow(uv0: vec2<f32>) -> vec4<f32> {
    let uv = curve(uv0);
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    let texel = 1.0 / TEX_SIZE;
    var color = textureSample(tex, samp, uv).rgb;

    var dirs = array<vec2<f32>, 8>(
        vec2<f32>(1.0, 0.0), vec2<f32>(-1.0, 0.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(0.0, -1.0),
        vec2<f32>(0.707, 0.707), vec2<f32>(-0.707, 0.707),
        vec2<f32>(0.707, -0.707), vec2<f32>(-0.707, -0.707),
    );
    var glow = vec3<f32>(0.0, 0.0, 0.0);
    for (var i: i32 = 0; i < 8; i = i + 1) {
        let d = dirs[i] * texel;
        glow = glow + bright(textureSample(tex, samp, uv + d * 1.5).rgb) * 0.65;
        glow = glow + bright(textureSample(tex, samp, uv + d * 3.5).rgb) * 0.40;
        glow = glow + bright(textureSample(tex, samp, uv + d * 7.0).rgb) * 0.25;
    }
    color = color + (glow / 8.0) * 2.2;

    return vec4<f32>(tube(color, uv), 1.0);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if (u.effect == 3u) {
        return crt(in.uv);
    }
    if (u.effect == 4u) {
        return fx_glow(in.uv);
    }

    let src = in.uv * TEX_SIZE;
    var color = textureSample(tex, samp, in.uv).rgb;

    if (u.effect == 1u) {
        color = epx(in.uv);
    } else if (u.effect == 2u) {
        // Smooth scanlines: bright at each source line's center, dark in the gaps,
        // with brightness compensation so the picture doesn't dim overall.
        let line = fract(src.y);
        color = min(color * mix(0.5, 1.0, sin(line * 3.14159)) * 1.2, vec3<f32>(1.0, 1.0, 1.0));
    }

    return vec4<f32>(color, 1.0);
}
"#;

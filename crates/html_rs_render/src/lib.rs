//! # html_rs_render
//!
//! The custom GPU renderer. No webview, no DOM engine — just wgpu:
//!
//! - **Layout**: taffy (flexbox) drives element sizes/positions.
//! - **Boxes**: one instanced SDF-rounded-rect pipeline (borders included).
//! - **Text**: glyphon (cosmic-text) shaping + atlas rendering.

mod painter;

pub use painter::{paint, DrawList, HitTarget, Quad, TextItem};

use std::sync::Arc;
use wgpu::util::DeviceExt;
use winit::window::Window;

const CORNERS: [[f32; 2]; 6] = [
    [0.0, 0.0],
    [1.0, 0.0],
    [0.0, 1.0],
    [1.0, 0.0],
    [1.0, 1.0],
    [0.0, 1.0],
];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct QuadGpu {
    // [pos.xy, size.xy]
    pos_size: [f32; 4],
    color: [f32; 4],
    // [radius, border_width, 0, 0]
    params: [f32; 4],
    border_color: [f32; 4],
}

/// Owns the GPU state. Created once per window.
pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    quad_pipeline: wgpu::RenderPipeline,
    corner_buf: wgpu::Buffer,
    instance_buf: wgpu::Buffer,
    instance_capacity: usize,
    camera_buf: wgpu::Buffer,
    camera_bind: wgpu::BindGroup,
    // Text stack.
    pub font_system: glyphon::FontSystem,
    swash_cache: glyphon::SwashCache,
    _cache: glyphon::Cache,
    atlas: glyphon::TextAtlas,
    text_renderer: glyphon::TextRenderer,
    viewport: glyphon::Viewport,
    /// Window clear color (linear RGBA).
    pub clear: [f32; 4],
    width: f32,
    height: f32,
}

impl Renderer {
    /// Creates the renderer for a window. Native callers wrap in `pollster::block_on`;
    /// on wasm this must be awaited inside `spawn_local`.
    pub async fn new(window: Arc<Window>) -> Self {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance
            .create_surface(window)
            .expect("surface creation");
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .expect("no suitable GPU adapter");
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("html.rs device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
            })
            .await
            .expect("device creation");

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&device, &config);

        // --- Quad pipeline -------------------------------------------------
        let camera_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("html.rs camera"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("html.rs camera bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(16),
                },
                count: None,
            }],
        });
        let camera_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("html.rs camera bind"),
            layout: &camera_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buf.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("html.rs quad layout"),
            bind_group_layouts: &[Some(&camera_bgl)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("html.rs quad shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let quad_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("html.rs quad pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[
                    // Corner positions
                    Some(wgpu::VertexBufferLayout {
                        array_stride: 8,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[wgpu::VertexAttribute {
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                        }],
                    }),
                    // Quad instances
                    Some(wgpu::VertexBufferLayout {
                        array_stride: 64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &[
                            wgpu::VertexAttribute { shader_location: 1, format: wgpu::VertexFormat::Float32x4, offset: 0 },
                            wgpu::VertexAttribute { shader_location: 2, format: wgpu::VertexFormat::Float32x4, offset: 16 },
                            wgpu::VertexAttribute { shader_location: 3, format: wgpu::VertexFormat::Float32x4, offset: 32 },
                            wgpu::VertexAttribute { shader_location: 4, format: wgpu::VertexFormat::Float32x4, offset: 48 },
                        ],
                    }),
                ],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let corner_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("html.rs corners"),
            contents: bytemuck::cast_slice(&CORNERS),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let instance_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("html.rs quads"),
            size: 64 * 256,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // --- Text (glyphon) -------------------------------------------------
        let cache = glyphon::Cache::new(&device);
        let font_system = glyphon::FontSystem::new();
        let swash_cache = glyphon::SwashCache::new();
        let mut atlas = glyphon::TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer = glyphon::TextRenderer::new(
            &mut atlas,
            &device,
            wgpu::MultisampleState::default(),
            None,
        );
        let viewport = glyphon::Viewport::new(&device, &cache);

        Self {
            device,
            queue,
            surface,
            config,
            quad_pipeline,
            corner_buf,
            instance_buf,
            instance_capacity: 256,
            camera_buf,
            camera_bind,
            font_system,
            swash_cache,
            _cache: cache,
            atlas,
            text_renderer,
            viewport,
            clear: html_rs_core::DEFAULT_BG,
            width: size.width as f32,
            height: size.height as f32,
        }
    }

    /// Window clear color, in sRGB floats.
    pub fn set_clear_color(&mut self, srgb: [f32; 4]) {
        self.clear = html_rs_core::srgb_to_linear(srgb);
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.width = self.config.width as f32;
        self.height = self.config.height as f32;
        self.surface.configure(&self.device, &self.config);
    }

    /// Lays out + paints the tree (CPU side). Returns the draw list, which the
    /// runtime keeps around for hit-testing.
    pub fn paint(
        &mut self,
        root: &html_rs_core::Element,
        width: f32,
        height: f32,
        scale: f32,
        focus_path: Option<&[u32]>,
        hovered_path: Option<&[u32]>,
    ) -> DrawList {
        painter::paint(
            root,
            width.max(1.0),
            height.max(1.0),
            scale,
            &mut self.font_system,
            focus_path,
            hovered_path,
        )
    }

    /// Renders a draw list to the screen.
    pub fn render(&mut self, list: &DrawList) {
        // Upload camera.
        let camera: [f32; 4] = [self.width, self.height, 0.0, 0.0];
        self.queue
            .write_buffer(&self.camera_buf, 0, bytemuck::cast_slice(&camera));

        // Upload quads (grow buffer if needed).
        let quads: Vec<QuadGpu> = list
            .quads
            .iter()
            .map(|q| QuadGpu {
                pos_size: [q.x, q.y, q.w, q.h],
                color: q.color,
                params: [q.radius, q.border_width, 0.0, 0.0],
                border_color: q.border_color,
            })
            .collect();
        if quads.len() > self.instance_capacity {
            self.instance_buf.destroy();
            self.instance_capacity = quads.len().next_power_of_two();
            self.instance_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("html.rs quads"),
                size: (self.instance_capacity * 64) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !quads.is_empty() {
            self.queue
                .write_buffer(&self.instance_buf, 0, bytemuck::cast_slice(&quads));
        }

        // Prepare text.
        self.viewport.update(
            &self.queue,
            glyphon::Resolution {
                width: self.config.width,
                height: self.config.height,
            },
        );
        let mut areas = Vec::with_capacity(list.texts.len());
        for t in &list.texts {
            let color = t.color;
            let a = (color[3] * 255.0) as u8;
            areas.push(glyphon::TextArea {
                buffer: &t.buffer,
                left: t.x,
                top: t.y,
                scale: 1.0,
                bounds: glyphon::TextBounds {
                    left: t.bounds[0].floor() as i32,
                    top: t.bounds[1].floor() as i32,
                    right: (t.bounds[0] + t.bounds[2]).ceil() as i32,
                    bottom: (t.bounds[1] + t.bounds[3]).ceil() as i32,
                },
                default_color: glyphon::Color::rgba(
                    (color[0] * 255.0) as u8,
                    (color[1] * 255.0) as u8,
                    (color[2] * 255.0) as u8,
                    a,
                ),
                custom_glyphs: &[],
            });
        }
        if let Err(e) = self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            areas,
            &mut self.swash_cache,
        ) {
            eprintln!("html.rs: text prepare failed: {e:?}");
        }

        // Draw.
        use wgpu::CurrentSurfaceTexture;
        let frame = match self.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(frame) | CurrentSurfaceTexture::Suboptimal(frame) => frame,
            CurrentSurfaceTexture::Outdated | CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            other => {
                eprintln!("html.rs: skipping frame: {other:?}");
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("html.rs frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("html.rs pass"),
                multiview_mask: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: self.clear[0] as f64,
                            g: self.clear[1] as f64,
                            b: self.clear[2] as f64,
                            a: self.clear[3] as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if !quads.is_empty() {
                pass.set_pipeline(&self.quad_pipeline);
                pass.set_bind_group(0, &self.camera_bind, &[]);
                pass.set_vertex_buffer(0, self.corner_buf.slice(..));
                pass.set_vertex_buffer(1, self.instance_buf.slice(..));
                pass.draw(0..6, 0..quads.len() as u32);
            }
            if let Err(e) = self.text_renderer.render(&self.atlas, &self.viewport, &mut pass) {
                eprintln!("html.rs: text render failed: {e:?}");
            }
        }
        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
    }
}
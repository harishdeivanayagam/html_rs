//! # html_rs_render
//!
//! The custom GPU renderer. No webview, no DOM engine — just wgpu:
//!
//! - **Layout**: taffy (flexbox) drives element sizes/positions.
//! - **Boxes**: one instanced SDF-rounded-rect pipeline (borders included).
//! - **Text**: glyphon (cosmic-text) shaping + atlas rendering.

mod painter;

pub use painter::{paint, range_value_at, DrawList, HitTarget, ImageItem, ImageSizes, InputState, PaintState, Quad, Scrollable, SelectState, TextItem};

use std::collections::HashMap;
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

/// Corner positions + UVs for the image pipeline ([x, y, u, v] per vertex).
const CORNERS_UV: [[f32; 4]; 6] = [
    [0.0, 0.0, 0.0, 0.0],
    [1.0, 0.0, 1.0, 0.0],
    [0.0, 1.0, 0.0, 1.0],
    [1.0, 0.0, 1.0, 0.0],
    [1.0, 1.0, 1.0, 1.0],
    [0.0, 1.0, 0.0, 1.0],
];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ImageInstanceGpu {
    pos_size: [f32; 4],
    params: [f32; 4],
}

struct LoadedImage {
    #[allow(dead_code)] // must stay alive: bind group keeps a view, not ownership
    texture: wgpu::Texture,
    bind: wgpu::BindGroup,
    w: f32,
    h: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
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
    /// Corner+UV vertices for the image pipeline.
    corner_uv_buf: wgpu::Buffer,
    instance_buf: wgpu::Buffer,
    instance_capacity: usize,
    /// Last frame's uploaded instances — used to write only changed ranges.
    prev_instances: Vec<QuadGpu>,
    camera_buf: wgpu::Buffer,
    camera_bind: wgpu::BindGroup,
    // Text stack.
    pub font_system: glyphon::FontSystem,
    swash_cache: glyphon::SwashCache,
    _cache: glyphon::Cache,
    atlas: glyphon::TextAtlas,
    text_renderer: glyphon::TextRenderer,
    viewport: glyphon::Viewport,
    /// Persistent layout + paint state (taffy tree, previous items, shaped
    /// text cache) — enables incremental, surgical re-layout.
    paint_state: painter::PaintState,
    /// Image pipeline state: decoded textures keyed by `src`.
    image_pipeline: wgpu::RenderPipeline,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))] // no filesystem → no textures on web
    image_bind: wgpu::BindGroupLayout,
    /// Linear filtering sampler shared by all image bind groups.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    linear_sampler: wgpu::Sampler,
    image_instance_buf: wgpu::Buffer,
    image_instance_capacity: usize,
    image_cache: HashMap<String, Option<LoadedImage>>,
    image_sizes: ImageSizes,
    /// Runtime-managed `<select>` state (open menu + internal selections).
    selects: painter::SelectState,
    /// Runtime-managed `<input>`/`<textarea>` state (uncontrolled typed values).
    inputs: painter::InputState,
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

        // --- Image pipeline -------------------------------------------------
        let image_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("html.rs image bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
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
            ],
        });
        let image_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("html.rs image layout"),
            bind_group_layouts: &[Some(&camera_bgl), Some(&image_bgl)],
            immediate_size: 0,
        });
        let image_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("html.rs image shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("image.wgsl").into()),
        });
        let image_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("html.rs image pipeline"),
            layout: Some(&image_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &image_shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[
                    Some(wgpu::VertexBufferLayout {
                        array_stride: 16,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[
                            wgpu::VertexAttribute { shader_location: 0, format: wgpu::VertexFormat::Float32x2, offset: 0 },
                            wgpu::VertexAttribute { shader_location: 1, format: wgpu::VertexFormat::Float32x2, offset: 8 },
                        ],
                    }),                    Some(wgpu::VertexBufferLayout {
                        array_stride: 32,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &[
                            wgpu::VertexAttribute { shader_location: 2, format: wgpu::VertexFormat::Float32x4, offset: 0 },
                            wgpu::VertexAttribute { shader_location: 3, format: wgpu::VertexFormat::Float32x4, offset: 16 },
                        ],
                    }),
                ],
            },
            fragment: Some(wgpu::FragmentState {
                module: &image_shader,
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
        let corner_uv_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("html.rs image corners"),
            contents: bytemuck::cast_slice(&CORNERS_UV),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let image_instance_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("html.rs image instances"),
            size: 32 * 64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let linear_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("html.rs image sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        Self {
            device,
            queue,
            surface,
            config,
            quad_pipeline,
            corner_buf,
            corner_uv_buf,
            instance_buf,
            instance_capacity: 256,
            prev_instances: Vec::new(),
            camera_buf,
            camera_bind,
            font_system,
            swash_cache,
            _cache: cache,
            atlas,
            text_renderer,
            viewport,
            paint_state: painter::PaintState::new(),
            image_pipeline,
            image_bind: image_bgl,
            linear_sampler,
            image_instance_buf,
            image_instance_capacity: 64,
            image_cache: HashMap::new(),
            image_sizes: HashMap::new(),
            selects: painter::SelectState::default(),
            inputs: painter::InputState::default(),
            clear: html_rs_core::srgb_to_linear(html_rs_core::current_theme().background),
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

    /// Lays out + paints the tree (CPU side). Layout is incremental: only
    /// subtrees whose elements changed are re-laid-out; unchanged text is not
    /// re-shaped. Returns the draw list, which the runtime keeps for
    /// hit-testing.
    pub fn paint(
        &mut self,
        root: &html_rs_core::Element,
        width: f32,
        height: f32,
        scale: f32,
        focus_path: Option<&[u32]>,
        hovered_path: Option<&[u32]>,
    ) -> DrawList {
        self.collect_images(root);
        painter::paint(
            &mut self.paint_state,
            root,
            width.max(1.0),
            height.max(1.0),
            scale,
            &mut self.font_system,
            &self.image_sizes,
            &mut self.selects,
            &mut self.inputs,
            focus_path,
            hovered_path,
        )
    }

    /// Mutable access to the runtime-managed `<select>` state (open menu +
    /// internal selections). The runtime updates this on clicks.
    pub fn selects_mut(&mut self) -> &mut painter::SelectState {
        &mut self.selects
    }

    /// Mutable access to the runtime-managed `<input>`/`<textarea>` state
    /// (uncontrolled typed values). The runtime updates this on key events.
    pub fn inputs_mut(&mut self) -> &mut painter::InputState {
        &mut self.inputs
    }

    /// Read-only access to the input state.
    pub fn inputs(&self) -> &painter::InputState {
        &self.inputs
    }

    /// Read-only access to the select state (open menu, selections).
    pub fn selects(&self) -> &painter::SelectState {
        &self.selects
    }

    /// Scrolls the container at `path` by `dy` physical px (positive =
    /// content moves up, like a wheel-down). Clamped at the next paint.
    pub fn scroll_by(&mut self, path: &[u32], dy: f32) {
        self.paint_state.scroll_by(path, dy);
    }

    /// Walks the tree, decoding any newly seen `<img src>` files and filling
    /// `image_sizes` with their intrinsic dimensions (used for measurement).
    fn collect_images(&mut self, root: &html_rs_core::Element) {
        fn walk(el: &html_rs_core::Element, r: &mut Renderer) {
            if el.tag == "img" {
                let src = el.get_attr("src").unwrap_or_default().to_string();
                if !src.is_empty() && !r.image_cache.contains_key(&src) {
                    r.image_cache.insert(src.clone(), None);
                    if let Some(img) = r.load_image(&src) {
                        r.image_sizes.insert(src.clone(), Some((img.w, img.h)));
                        r.image_cache.insert(src, Some(img));
                    }
                }
            }
            for child in &el.children {
                if let html_rs_core::Node::Element(e) = child {
                    walk(e, r);
                }
            }
        }
        walk(root, self);
    }

    /// Decodes an image file from disk into a GPU texture. Returns `None` for
    /// remote/data URLs, on wasm32 (no filesystem access), or if the file
    /// cannot be read/decoded.
    fn load_image(&self, src: &str) -> Option<LoadedImage> {
        if src.starts_with("http://")
            || src.starts_with("https://")
            || src.starts_with("data:")
        {
            return None;
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = src;
            None
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let bytes = std::fs::read(src).ok()?;
            let img = image::load_from_memory(&bytes).ok()?;
            let (iw, ih) = (img.width(), img.height());
            if iw == 0 || ih == 0 {
                return None;
            }
            let rgba = img.to_rgba8();
            let size = wgpu::Extent3d {
                width: iw,
                height: ih,
                depth_or_array_layers: 1,
            };
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("html.rs image"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * iw),
                    rows_per_image: None,
                },
                size,
            );
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("html.rs image bind"),
                layout: &self.image_bind,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Sampler(&self.linear_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                ],
            });
            Some(LoadedImage {
                texture,
                bind,
                w: iw as f32,
                h: ih as f32,
            })
        }
    }

    /// Renders a draw list to the screen.
    pub fn render(&mut self, list: &DrawList) {
        // Upload camera.
        let camera: [f32; 4] = [self.width, self.height, 0.0, 0.0];
        self.queue
            .write_buffer(&self.camera_buf, 0, bytemuck::cast_slice(&camera));

        // Upload quads (grow buffer if needed). Top-layer quads are appended
        // after the base ones so a single instance buffer serves both passes.
        let quad_split = list.quads.len();
        let quads: Vec<QuadGpu> = list
            .quads
            .iter()
            .chain(list.top_quads.iter())
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
            // Fresh buffer: nothing from the previous frame is valid.
            self.prev_instances.clear();
        }
        // Surgical upload: write only the instance ranges that actually
        // changed since the previous frame (each entry is 64 bytes, so every
        // range offset stays copy-aligned).
        let mut i = 0;
        while i < quads.len() {
            if i < self.prev_instances.len() && self.prev_instances[i] == quads[i] {
                i += 1;
                continue;
            }
            let start = i;
            while i < quads.len() && (i >= self.prev_instances.len() || self.prev_instances[i] != quads[i]) {
                i += 1;
            }
            self.queue.write_buffer(
                &self.instance_buf,
                (start * 64) as u64,
                bytemuck::cast_slice(&quads[start..i]),
            );
        }
        self.prev_instances = quads;
        let quad_count = self.prev_instances.len();

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

        // Draw — two passes: base layer (quads → images → text), then the
        // topmost layer (dialog panels / select menus). Two *submitted*
        // command buffers are required because glyphon's `prepare` rewrites
        // the shared vertex buffer — the top pass must be enqueued only after
        // the base pass has been submitted.
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
                label: Some("html.rs pass (base)"),
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
            if quad_split > 0 {
                pass.set_pipeline(&self.quad_pipeline);
                pass.set_bind_group(0, &self.camera_bind, &[]);
                pass.set_vertex_buffer(0, self.corner_buf.slice(..));
                pass.set_vertex_buffer(1, self.instance_buf.slice(..));
                pass.draw(0..6, 0..quad_split as u32);
            }
            // Images (between quads and text).
            if !list.images.is_empty() {
                // Grow the instance scratch buffer if needed.
                if list.images.len() > self.image_instance_capacity {
                    self.image_instance_buf.destroy();
                    self.image_instance_capacity = list.images.len().next_power_of_two();
                    self.image_instance_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("html.rs image instances"),
                        size: (self.image_instance_capacity * 32) as u64,
                        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    });
                }
                for (i, img) in list.images.iter().enumerate() {
                    let inst = ImageInstanceGpu {
                        pos_size: [img.rect[0], img.rect[1], img.rect[2], img.rect[3]],
                        params: [img.radius, 0.0, 0.0, 0.0],
                    };
                    self.queue.write_buffer(
                        &self.image_instance_buf,
                        (i * 32) as u64,
                        bytemuck::bytes_of(&inst),
                    );
                }
                pass.set_pipeline(&self.image_pipeline);
                pass.set_bind_group(0, &self.camera_bind, &[]);
                pass.set_vertex_buffer(0, self.corner_uv_buf.slice(..));
                pass.set_vertex_buffer(1, self.image_instance_buf.slice(..));
                for (i, img) in list.images.iter().enumerate() {
                    if let Some(Some(loaded)) = self.image_cache.get(&img.src) {
                        pass.set_bind_group(1, &loaded.bind, &[]);
                        pass.set_vertex_buffer(
                            1,
                            self.image_instance_buf.slice((i * 32) as u64..((i + 1) * 32) as u64),
                        );
                        pass.draw(0..6, 0..1);
                    }
                }
            }
            if let Err(e) = self.text_renderer.render(&self.atlas, &self.viewport, &mut pass) {
                eprintln!("html.rs: text render failed: {e:?}");
            }
        }
        self.queue.submit(Some(encoder.finish()));

        // Topmost layer: modal dialogs and open select menus. Never drawn
        // unless something is on it (zero cost otherwise).
        let has_top = !list.top_quads.is_empty() || !list.top_texts.is_empty();
        if has_top {
            // Only call render() if top text was prepared — otherwise it
            // would re-draw the *base* layer's text (still queued).
            let mut drew_text = false;
            if !list.top_texts.is_empty() {
                let areas: Vec<glyphon::TextArea> = list
                    .top_texts
                    .iter()
                    .map(|t| {
                        let color = t.color;
                        let a = (color[3] * 255.0) as u8;
                        glyphon::TextArea {
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
                        }
                    })
                    .collect();
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
                drew_text = true;
            }
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("html.rs frame (top)"),
                });
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("html.rs pass (top)"),
                    multiview_mask: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                if quad_count > quad_split {
                    pass.set_pipeline(&self.quad_pipeline);
                    pass.set_bind_group(0, &self.camera_bind, &[]);
                    pass.set_vertex_buffer(0, self.corner_buf.slice(..));
                    pass.set_vertex_buffer(1, self.instance_buf.slice(..));
                    pass.draw(0..6, quad_split as u32..quad_count as u32);
                }
                if drew_text {
                    if let Err(e) = self.text_renderer.render(&self.atlas, &self.viewport, &mut pass) {
                        eprintln!("html.rs: text render failed: {e:?}");
                    }
                }
            }
            self.queue.submit(Some(encoder.finish()));
        }
        self.queue.present(frame);
    }
}
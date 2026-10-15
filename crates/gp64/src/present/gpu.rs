//! Native GPU scaling and coverage-aware host layers. CRT retains CPU composition.
use super::{Frame, Software};
use crate::{
    overlay,
    scaler::{self, Blitter, Scaler, Viewport},
};
use anyhow::{bail, Context, Result};
use std::{
    future::Future,
    sync::{Arc, Mutex},
    task::{Context as TaskContext, Poll, Wake, Waker},
};
use winit::{dpi::PhysicalSize, window::Window};

const COMMON: &str = include_str!("scaling.wgsl");
const SHADERS: [&str; 3] = [
    include_str!("integer.wgsl"),
    include_str!("smooth.wgsl"),
    include_str!("area.wgsl"),
];
const OVERLAY_SHADER: &str = include_str!("overlay.wgsl");

fn scaling_shader(source: &str) -> String {
    format!("{COMMON}\n{source}")
}

// Native wgpu futures wake the waiting thread themselves. No async runtime is
// needed for the one-time adapter/device requests.
pub(super) fn block_on<T>(future: impl Future<Output = T>) -> T {
    struct ThreadWake(std::thread::Thread);
    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
    let mut context = TaskContext::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::park(),
        }
    }
}

fn instance() -> wgpu::Instance {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    #[cfg(target_os = "windows")]
    {
        descriptor.backends = wgpu::Backends::DX12;
    }
    #[cfg(target_os = "macos")]
    {
        descriptor.backends = wgpu::Backends::METAL;
    }
    wgpu::Instance::new(descriptor)
}

/// Native resources are dropped together before the software fallback opens.
pub struct Gpu {
    pub(super) window: Arc<Window>,
    surface: Option<wgpu::Surface<'static>>,
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    scale_factor: f64,
    configure_pending: bool,
    failures: u8,
    failure: Arc<Mutex<Option<String>>>,
    renderer: Renderer,
    blitter: Blitter,
    staging: Vec<u32>,
}

impl Gpu {
    pub(super) fn new(window: Arc<Window>) -> Result<Self> {
        let instance = instance();
        let surface = instance.create_surface(window.clone())?;
        // Scaling one small frame is trivial work: prefer the integrated GPU
        // rather than waking a discrete one.
        let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))?;
        // A software rasteriser (such as WARP) is slower than the CPU scaler.
        anyhow::ensure!(
            adapter.get_info().device_type != wgpu::DeviceType::Cpu,
            "only a software GPU adapter is available ({})",
            adapter.get_info().name
        );
        let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("gp64 presenter"),
            ..Default::default()
        }))?;
        let failure = Arc::new(Mutex::new(None));
        let lost = failure.clone();
        device.set_device_lost_callback(move |reason, message| {
            if let Ok(mut failure) = lost.lock() {
                failure.get_or_insert_with(|| format!("device lost ({reason:?}): {message}"));
            }
        });
        let invalid = failure.clone();
        device.on_uncaptured_error(Arc::new(move |error: wgpu::Error| {
            if let Ok(mut failure) = invalid.lock() {
                failure.get_or_insert_with(|| error.to_string());
            }
        }));
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| *f == wgpu::TextureFormat::Bgra8Unorm)
            .or_else(|| {
                caps.formats.iter().copied().find(|f| {
                    matches!(
                        f.remove_srgb_suffix(),
                        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
                    )
                })
            })
            .context("surface has no 8-bit RGB format")?;
        if !caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque)
            || !caps.present_modes.contains(&wgpu::PresentMode::Fifo)
        {
            bail!("surface cannot present opaque FIFO frames");
        }
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width,
            height: size.height,
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            view_formats: if format.is_srgb() {
                vec![format.remove_srgb_suffix()]
            } else {
                vec![]
            },
            desired_maximum_frame_latency: 1,
        };
        let renderer = Renderer::new(&device, format.remove_srgb_suffix());
        let mut gpu = Self {
            scale_factor: window.scale_factor(),
            window,
            surface: Some(surface),
            instance,
            adapter,
            device,
            queue,
            config,
            configure_pending: true,
            failures: 0,
            failure,
            renderer,
            blitter: Blitter::default(),
            staging: Vec::new(),
        };
        if size.width != 0 && size.height != 0 {
            gpu.configure(size)?;
        }
        gpu.ensure_live()?;
        tracing::info!("GPU presentation initialized");
        Ok(gpu)
    }

    fn ensure_live(&self) -> Result<()> {
        let failure = self
            .failure
            .lock()
            .map_err(|_| anyhow::anyhow!("GPU failure state poisoned"))?;
        if let Some(reason) = failure.as_ref() {
            bail!("{reason}");
        }
        Ok(())
    }

    fn configure(&mut self, size: PhysicalSize<u32>) -> Result<()> {
        self.ensure_live()?;
        if size.width > self.device.limits().max_texture_dimension_2d
            || size.height > self.device.limits().max_texture_dimension_2d
        {
            bail!("window exceeds GPU texture limits");
        }
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface
            .as_ref()
            .context("missing surface")?
            .configure(&self.device, &self.config);
        self.ensure_live()?;
        self.configure_pending = false;
        self.scale_factor = self.window.scale_factor();
        Ok(())
    }

    fn recover(&mut self, lost: bool) -> Result<()> {
        self.failures += 1;
        if self.failures >= 3 {
            bail!("GPU surface failed three consecutive frames");
        }
        if lost {
            // Drop the old surface before creating its replacement.
            self.surface = None;
            let surface = self.instance.create_surface(self.window.clone())?;
            let caps = surface.get_capabilities(&self.adapter);
            if !caps.formats.contains(&self.config.format) {
                bail!("recreated surface format changed");
            }
            self.surface = Some(surface);
        }
        self.configure_pending = true;
        // A skipped acquire must be retried even when the guest is paused.
        self.window.request_redraw();
        Ok(())
    }

    pub(super) fn present(
        &mut self,
        frame: Frame<'_>,
        size: PhysicalSize<u32>,
        scaler: Scaler,
        overlays: &overlay::Layers<'_>,
    ) -> Result<()> {
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        self.device.poll(wgpu::PollType::Poll)?;
        self.ensure_live()?;
        if self.configure_pending
            || (size.width, size.height) != (self.config.width, self.config.height)
            || self.scale_factor != self.window.scale_factor()
        {
            self.configure(size)?;
        }
        let (output, suboptimal) = match self
            .surface
            .as_ref()
            .context("missing surface")?
            .get_current_texture()
        {
            wgpu::CurrentSurfaceTexture::Success(output) => (output, false),
            wgpu::CurrentSurfaceTexture::Suboptimal(output) => (output, true),
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(())
            }
            wgpu::CurrentSurfaceTexture::Outdated => return self.recover(false),
            wgpu::CurrentSurfaceTexture::Lost => return self.recover(true),
            wgpu::CurrentSurfaceTexture::Validation => bail!("GPU surface validation error"),
        };
        let view = output.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.config.format.remove_srgb_suffix()),
            ..Default::default()
        });
        let composed = scaler == Scaler::Crt;
        let viewport = if composed {
            self.staging
                .resize(size.width as usize * size.height as usize, 0);
            Software::render(
                &mut self.blitter,
                frame,
                &mut self.staging,
                size,
                scaler,
                overlays,
            );
            self.renderer.upload(
                &self.device,
                &self.queue,
                &self.staging,
                size.width,
                size.height,
                None,
            )?;
            Viewport {
                x: 0,
                y: 0,
                width: size.width as usize,
                height: size.height as usize,
            }
        } else {
            self.renderer
                .update_overlays(&self.device, &self.queue, size, overlays)?;
            self.renderer.upload(
                &self.device,
                &self.queue,
                frame.pixels,
                frame.width as u32,
                frame.height as u32,
                Some(frame.generation),
            )?;
            scaler::viewport(
                frame.width,
                size.width as usize,
                size.height as usize,
                scaler,
            )
        };
        self.queue.submit([self.renderer.draw(
            &self.device,
            &self.queue,
            &view,
            &viewport,
            scaler,
            !composed,
        )]);
        self.device.poll(wgpu::PollType::Poll)?;
        self.ensure_live()?;
        self.window.pre_present_notify();
        output.present();
        if suboptimal {
            self.recover(false)?;
        } else {
            self.failures = 0;
        }
        Ok(())
    }
}

struct Renderer {
    pipelines: [wgpu::RenderPipeline; 3],
    overlay_pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    geometry: wgpu::Buffer,
    image: Option<Image>,
    menu_cache: overlay::MenuRasterCache,
    recording_cache: overlay::RecRasterCache,
    menu: Layer,
    recording: Layer,
    menu_open: bool,
    recording_visible: bool,
}
struct Image {
    texture: wgpu::Texture,
    bind: wgpu::BindGroup,
    generation: Option<u64>,
}

impl Renderer {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        // Explicit layout lets every scaler and layer reuse its image binding.
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("gp64 pixels and geometry"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("gp64 presenter"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipelines = SHADERS
            .map(|source| pipeline(device, format, &pipeline_layout, &scaling_shader(source)));
        Self {
            pipelines,
            overlay_pipeline: pipeline(device, format, &pipeline_layout, OVERLAY_SHADER),
            layout,
            geometry: uniform(device, 32),
            image: None,
            menu_cache: overlay::MenuRasterCache::default(),
            recording_cache: overlay::RecRasterCache::default(),
            menu: Layer::new(device),
            recording: Layer::new(device),
            menu_open: false,
            recording_visible: false,
        }
    }

    fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pixels: &[u32],
        width: u32,
        height: u32,
        generation: Option<u64>,
    ) -> Result<()> {
        Image::upload(
            &mut self.image,
            device,
            queue,
            &self.layout,
            &self.geometry,
            pixels,
            width,
            height,
            generation,
        )?;
        Ok(())
    }

    fn update_overlays(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        size: PhysicalSize<u32>,
        overlays: &overlay::Layers<'_>,
    ) -> Result<()> {
        let (width, height) = (size.width, size.height);
        self.menu_open = overlays.menu.is_open();
        if self.menu_open
            && self
                .menu_cache
                .update(overlays.menu, width, height, overlays.values)
        {
            self.menu
                .upload(device, queue, &self.layout, self.menu_cache.raster())?;
        }
        // Match Layers::render: sample the pulse after drawing the menu.
        self.recording_visible = overlays.values.recording.is_some();
        if self.recording_cache.update(
            overlays.values.recording,
            width,
            height,
            (overlays.pulse_ms)(),
        ) {
            if let Some(raster) = self.recording_cache.raster() {
                self.recording.upload(device, queue, &self.layout, raster)?;
            }
        }
        Ok(())
    }

    fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        viewport: &Viewport,
        scaler: Scaler,
        gpu_overlays: bool,
    ) -> wgpu::CommandBuffer {
        let total = self.image.as_ref().map_or(1, |image| {
            u64::from(image.texture.width()) * u64::from(image.texture.height())
        });
        let reciprocal = (1u64 << 40).div_ceil(total);
        let geometry = [
            viewport.x as u32,
            viewport.y as u32,
            viewport.width as u32,
            viewport.height as u32,
            reciprocal as u32,
            (reciprocal >> 32) as u32,
            u32::from(gpu_overlays && self.menu_open),
            0,
        ];
        write_words(queue, &self.geometry, &geometry);
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("gp64 present"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            if let Some(image) = &self.image {
                if viewport.width > 0 && viewport.height > 0 {
                    let index = match scaler {
                        Scaler::Integer | Scaler::Crt => 0,
                        Scaler::Smooth => 1,
                        Scaler::Area => 2,
                    };
                    pass.set_pipeline(&self.pipelines[index]);
                    pass.set_bind_group(0, &image.bind, &[]);
                    set_region(&mut pass, viewport);
                    pass.draw(0..3, 0..1);
                }
            }
            if gpu_overlays {
                pass.set_pipeline(&self.overlay_pipeline);
                if self.menu_open {
                    self.menu.draw(&mut pass);
                }
                if self.recording_visible {
                    self.recording.draw(&mut pass);
                }
            }
        }
        encoder.finish()
    }
}

impl Image {
    /// Returns whether bytes were sent to the GPU (also observed by tests).
    fn upload(
        slot: &mut Option<Self>,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        geometry: &wgpu::Buffer,
        pixels: &[u32],
        width: u32,
        height: u32,
        generation: Option<u64>,
    ) -> Result<bool> {
        if width == 0 || height == 0 || pixels.len() < width as usize * height as usize {
            *slot = None;
            return Ok(false);
        }
        if width > device.limits().max_texture_dimension_2d
            || height > device.limits().max_texture_dimension_2d
        {
            bail!("guest exceeds GPU texture limits");
        }
        if slot
            .as_ref()
            .is_none_or(|image| image.texture.width() != width || image.texture.height() != height)
        {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("gp64 BGRA pixels"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("gp64 image"),
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(
                            &texture.create_view(&Default::default()),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: geometry.as_entire_binding(),
                    },
                ],
            });
            *slot = Some(Image {
                texture,
                bind,
                generation: None,
            });
        }
        let image = slot.as_mut().context("image was allocated")?;
        if generation.is_none() || image.generation != generation {
            // These native targets are little-endian: 0x00RRGGBB is BGRA in
            // memory. Safe conversion also makes that byte order explicit.
            let bytes: Vec<u8> = pixels[..width as usize * height as usize]
                .iter()
                .flat_map(|p| p.to_le_bytes())
                .collect();
            queue.write_texture(
                image.texture.as_image_copy(),
                &bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4),
                    rows_per_image: Some(height),
                },
                image.texture.size(),
            );
            image.generation = generation;
            return Ok(true);
        }
        Ok(false)
    }
}

struct Layer {
    image: Option<Image>,
    placement: wgpu::Buffer,
    origin: [u32; 2],
    #[cfg(test)]
    uploads: usize,
}
impl Layer {
    fn new(device: &wgpu::Device) -> Self {
        Self {
            image: None,
            placement: uniform(device, 16),
            origin: [0, 0],
            #[cfg(test)]
            uploads: 0,
        }
    }

    fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        raster: &overlay::Raster,
    ) -> Result<()> {
        let uploaded = Image::upload(
            &mut self.image,
            device,
            queue,
            layout,
            &self.placement,
            &raster.pixels,
            raster.width,
            raster.height,
            None,
        )?;
        if uploaded {
            self.origin = raster.origin;
            write_words(
                queue,
                &self.placement,
                &[self.origin[0], self.origin[1], 0, 0],
            );
            #[cfg(test)]
            {
                self.uploads += 1;
            }
        }
        Ok(())
    }

    fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        if let Some(image) = &self.image {
            pass.set_bind_group(0, &image.bind, &[]);
            set_region(
                pass,
                &Viewport {
                    x: self.origin[0] as usize,
                    y: self.origin[1] as usize,
                    width: image.texture.width() as usize,
                    height: image.texture.height() as usize,
                },
            );
            pass.draw(0..3, 0..1);
        }
    }
}

fn set_region(pass: &mut wgpu::RenderPass<'_>, viewport: &Viewport) {
    pass.set_viewport(
        viewport.x as f32,
        viewport.y as f32,
        viewport.width as f32,
        viewport.height as f32,
        0.0,
        1.0,
    );
    pass.set_scissor_rect(
        viewport.x as u32,
        viewport.y as u32,
        viewport.width as u32,
        viewport.height as u32,
    );
}

fn uniform(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gp64 geometry"),
        size,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn write_words(queue: &wgpu::Queue, buffer: &wgpu::Buffer, words: &[u32]) {
    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    queue.write_buffer(buffer, 0, &bytes);
}

fn pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    layout: &wgpu::PipelineLayout,
    source: &str,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("gp64 presenter"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("gp64 presenter"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vertex"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fragment"),
            compilation_options: Default::default(),
            targets: &[Some(format.into())],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}

#[cfg(test)]
mod tests;

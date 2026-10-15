//! Minimal native GPU presentation. CPU composition remains the oracle for
//! filtered modes and overlays until their dedicated GPU implementation.
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

const SHADER: &str = include_str!("integer.wgsl");

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
        let composed = scaler != Scaler::Integer
            || overlays.menu.is_open()
            || overlays.values.recording.is_some();
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
        self.queue.submit([self
            .renderer
            .draw(&self.device, &self.queue, &view, &viewport)]);
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
    pipeline: wgpu::RenderPipeline,
    geometry: wgpu::Buffer,
    image: Option<Image>,
}
struct Image {
    texture: wgpu::Texture,
    bind: wgpu::BindGroup,
    generation: Option<u64>,
}

impl Renderer {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("gp64 integer"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("gp64 integer"),
            layout: None,
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
        });
        let geometry = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("gp64 geometry"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            geometry,
            image: None,
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
        if width == 0 || height == 0 || pixels.len() < width as usize * height as usize {
            self.image = None;
            return Ok(());
        }
        if width > device.limits().max_texture_dimension_2d
            || height > device.limits().max_texture_dimension_2d
        {
            bail!("guest exceeds GPU texture limits");
        }
        if self
            .image
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
                layout: &self.pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(
                            &texture.create_view(&Default::default()),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.geometry.as_entire_binding(),
                    },
                ],
            });
            self.image = Some(Image {
                texture,
                bind,
                generation: None,
            });
        }
        let image = self.image.as_mut().context("image was allocated")?;
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
        }
        Ok(())
    }

    fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        viewport: &Viewport,
    ) -> wgpu::CommandBuffer {
        let geometry: Vec<u8> = [viewport.x, viewport.y, viewport.width, viewport.height]
            .iter()
            .flat_map(|v| (*v as u32).to_le_bytes())
            .collect();
        queue.write_buffer(&self.geometry, 0, &geometry);
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
                    pass.set_pipeline(&self.pipeline);
                    pass.set_bind_group(0, &image.bind, &[]);
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
                    pass.draw(0..3, 0..1);
                }
            }
        }
        encoder.finish()
    }
}

#[cfg(test)]
mod tests;

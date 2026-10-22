//! Headless egui rendering into RGBA frames with wgpu.

use std::sync::mpsc;

use egui::{Pos2, RawInput, Rect, Vec2, ViewportId, ViewportIdMap, ViewportInfo};

/// Gamma-space target, the format egui-wgpu prefers; bytes come out as sRGB RGBA.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Off-screen egui renderer with its own GPU device and egui context.
pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter_name: String,
    ctx: egui::Context,
    egui: egui_wgpu::Renderer,
    size: [u32; 2],
    target: wgpu::Texture,
    view: wgpu::TextureView,
    readback: wgpu::Buffer,
    padded_row: u32,
    pixels: Vec<u8>,
}

impl Renderer {
    /// Opens a GPU device without a surface and prepares a `size` (width, height) target.
    pub fn new(size: [u32; 2]) -> Result<Self, String> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .map_err(|e| format!("no GPU adapter: {e}"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("export"),
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .map_err(|e| format!("failed to open GPU device: {e}"))?;
        let info = adapter.get_info();
        let adapter_name = format!("{} ({:?})", info.name, info.backend);

        let (target, view, readback, padded_row) = create_target(&device, size)?;
        let egui = new_egui_renderer(&device);
        Ok(Self {
            device,
            queue,
            adapter_name,
            ctx: new_context(),
            egui,
            size,
            target,
            view,
            readback,
            padded_row,
            pixels: Vec::new(),
        })
    }

    /// Starts over at `size` with a fresh egui context, so no animation or texture state carries over.
    pub fn reset(&mut self, size: [u32; 2]) -> Result<(), String> {
        let (target, view, readback, padded_row) = create_target(&self.device, size)?;
        self.target = target;
        self.view = view;
        self.readback = readback;
        self.padded_row = padded_row;
        self.size = size;
        self.ctx = new_context();
        self.egui = new_egui_renderer(&self.device);
        Ok(())
    }

    /// The egui context frames are drawn with; upload textures here.
    pub fn ctx(&self) -> &egui::Context {
        &self.ctx
    }

    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    /// Runs one egui pass at input time `time` and returns the frame as tightly packed RGBA rows.
    pub fn render_frame(
        &mut self,
        time: f64,
        dt: f32,
        mut draw: impl FnMut(&mut egui::Ui),
    ) -> Result<&[u8], String> {
        let [w, h] = self.size;
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(w as f32, h as f32));
        let mut viewports = ViewportIdMap::default();
        viewports.insert(
            ViewportId::ROOT,
            ViewportInfo {
                native_pixels_per_point: Some(1.0),
                inner_rect: Some(rect),
                outer_rect: Some(rect),
                focused: Some(true),
                ..Default::default()
            },
        );
        let input = RawInput {
            viewport_id: ViewportId::ROOT,
            viewports,
            screen_rect: Some(rect),
            max_texture_side: Some(self.device.limits().max_texture_dimension_2d as usize),
            time: Some(time),
            predicted_dt: dt,
            ..Default::default()
        };

        let mut output = self.ctx.run_ui(input, |ui| {
            egui::CentralPanel::no_frame().show(ui, &mut draw);
        });
        let mut textures = std::mem::take(&mut output.textures_delta);
        let ppp = output.pixels_per_point;
        let primitives = self.ctx.tessellate(output.shapes, ppp);
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [w, h],
            pixels_per_point: ppp,
        };

        for (id, deltas) in textures.set.drain() {
            for delta in &deltas {
                self.egui
                    .update_texture(&self.device, &self.queue, id, delta);
            }
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("export frame"),
            });
        let mut commands = self.egui.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &primitives,
            &screen,
        );
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("export egui"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            self.egui.render(&mut pass, &primitives, &screen);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_row),
                    rows_per_image: Some(h),
                },
            },
            extent([w, h]),
        );
        commands.push(encoder.finish());
        self.queue.submit(commands);
        // Safe right after submission: wgpu keeps resources alive until queued work finishes.
        for id in textures.free.drain() {
            self.egui.free_texture(&id);
        }

        let slice = self.readback.slice(..);
        let (tx, rx) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| format!("GPU wait failed: {e}"))?;
        rx.recv()
            .map_err(|_| "GPU readback was dropped".to_owned())?
            .map_err(|e| format!("GPU readback failed: {e}"))?;

        let row = (w * 4) as usize;
        self.pixels.clear();
        self.pixels.reserve(row * h as usize);
        {
            let mapped = slice
                .get_mapped_range()
                .map_err(|e| format!("GPU readback failed: {e}"))?;
            for padded in mapped.chunks(self.padded_row as usize) {
                self.pixels.extend_from_slice(&padded[..row]);
            }
        }
        self.readback.unmap();
        Ok(&self.pixels)
    }
}

fn new_context() -> egui::Context {
    let ctx = egui::Context::default();
    crate::ui::install_fonts(&ctx);
    ctx
}

fn new_egui_renderer(device: &wgpu::Device) -> egui_wgpu::Renderer {
    egui_wgpu::Renderer::new(device, FORMAT, egui_wgpu::RendererOptions::default())
}

fn extent([w, h]: [u32; 2]) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: w,
        height: h,
        depth_or_array_layers: 1,
    }
}

/// Render target, its view, and a readback buffer with rows padded to wgpu's copy alignment.
fn create_target(
    device: &wgpu::Device,
    size: [u32; 2],
) -> Result<(wgpu::Texture, wgpu::TextureView, wgpu::Buffer, u32), String> {
    let [w, h] = size;
    let max = device.limits().max_texture_dimension_2d;
    if w == 0 || h == 0 || w > max || h > max {
        return Err(format!("unsupported frame size {w}x{h}"));
    }
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("export target"),
        size: extent(size),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let padded_row = (w * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("export readback"),
        size: u64::from(padded_row) * u64::from(h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    Ok((target, view, readback, padded_row))
}

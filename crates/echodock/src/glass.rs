//! Liquid Glass. The compositor draws a rune-fx lens in the shape of the dock first, refracting
//! and frosting a capture of the screen behind it, then draws Rune's UI (tint, rim, icons) over
//! it. The capture sits in a surface-sized texture at the place it was taken from, so the glass
//! bends the right part of the screen wherever the dock is while it slides.

use std::sync::{Arc, Mutex};

use rune::compose::{Compositor, DeviceKey, Frame};
use rune_fx::*;

use crate::dock::Dock;

#[derive(Default)]
struct Shot {
    /// Where it was taken, in surface points.
    band: [f32; 4],
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    generation: u64,
}

/// The latest capture behind one dock, shared by the capture thread and the compositor.
#[derive(Clone, Default)]
pub struct Backdrop(Arc<Mutex<Shot>>);

impl Backdrop {
    pub fn set(&self, band: [f32; 4], width: u32, height: u32, rgba: Vec<u8>) {
        let mut s = self.0.lock().unwrap();
        s.band = band;
        s.width = width;
        s.height = height;
        s.rgba = rgba;
        s.generation += 1;
    }

    pub fn ready(&self) -> bool {
        self.0.lock().unwrap().generation > 0
    }
}

struct Gpu {
    device: DeviceKey,
    fx: FxRenderer,
    tex: Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>,
    uploaded: u64,
}

#[derive(Default)]
pub struct Glass {
    gpu: Option<Gpu>,
}

impl Compositor<Dock> for Glass {
    fn render(&mut self, f: &mut Frame<'_, Dock>) {
        let (device, queue, size) = (f.device, f.queue, f.size);
        let key = f.device_key();
        if self.gpu.as_ref().is_none_or(|g| g.device != key) {
            self.gpu = Some(Gpu { device: key, fx: FxRenderer::new(device, f.format, FxOptions { dither: true }), tex: None, uploaded: 0 });
        }
        let gpu = self.gpu.as_mut().unwrap();
        let state = &f.rt.state;
        let k = size.0 as f32 / state.size.0.max(1.0);
        let refract = state.cfg.glass.refraction && state.backdrop.ready() && state.reveal > 0.0;

        // Keep the backdrop texture the size of the surface, and upload a new capture into it.
        if refract {
            if gpu.tex.as_ref().is_none_or(|t| t.2 != size) {
                let tex = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("echodock backdrop"),
                    size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let view = tex.create_view(&Default::default());
                gpu.tex = Some((tex, view, size));
                gpu.uploaded = 0;
            }
            let shot = state.backdrop.0.lock().unwrap();
            if shot.generation != gpu.uploaded && let Some((tex, _, _)) = &gpu.tex {
                let y = (shot.band[1] * k).round().max(0.0) as u32;
                let w = shot.width.min(size.0);
                let h = shot.height.min(size.1.saturating_sub(y));
                if w > 0 && h > 0 {
                    queue.write_texture(
                        wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d { x: 0, y, z: 0 }, aspect: wgpu::TextureAspect::All },
                        &shot.rgba,
                        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(shot.width * 4), rows_per_image: Some(shot.height) },
                        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                    );
                }
                gpu.uploaded = shot.generation;
            }
        }

        let clear = wgpu::Color::TRANSPARENT;
        if refract && let Some((_, view, _)) = &gpu.tex {
            let g = &state.cfg.glass;
            let [x, y, w, h] = state.pill_rect();
            let r = state.cfg.appearance.corner_radius.min(h / 2.0);
            let tint = if state.dark() { rgb(0x1a1c22) } else { rgb(0xffffff) };
            let layers = [Fx::lens(LensParams {
                frost: g.frost,
                // The bevel can't be deeper than the glass is thick.
                bevel: g.bevel.min(h * 0.45),
                depth: g.depth,
                dispersion: g.dispersion,
                magnification: 1.1,
                rim: 0.2 + g.rim * 0.9,
                shadow: 0.25 + 0.4 * g.shadow,
                tint,
                tint_amount: g.tint,
                ..Default::default()
            }).on(Shape::rounded(x, y, w, h, r)).at(f.rt.t as f32)];
            let fx = &mut gpu.fx;
            fx.set_srgb_blend(f.srgb_blend());
            let slots = [FxSlot::default().scale(k).backdrop_srgb(false)];
            fx.prepare_with_slots(device, queue, &FxFrame { viewport: [size.0, size.1], scale: k, layers: &layers, backdrop: None, sources: &[] }, &slots);
            fx.blur_backdrop(device, queue, f.encoder, view, 0);
            let mut pass = f.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("echodock glass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: f.target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(clear), store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            fx.draw_layer(&mut pass, 0);
            drop(pass);
            let n = f.rt.instances.len() as u32;
            f.ui.prepare(device, queue, &mut f.rt.text.atlas, &f.rt.instances, size);
            f.ui.encode_layers(f.encoder);
            f.ui.draw_target_range_with(f.encoder, f.target, f.format, wgpu::LoadOp::Load, 0..n, &mut |_, _| {});
        } else {
            f.draw_ui();
        }
    }
}

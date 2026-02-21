use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

use image::{DynamicImage, RgbaImage};

use crate::state::EditState;

const STATE_EPS: f32 = 0.001;
const WORKGROUP_SIZE: u32 = 16;

struct GpuContext {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    adapter_name: String,
    adapter_backend: String,
    adapter_driver: String,
}

static GPU_CONTEXT: OnceLock<Option<GpuContext>> = OnceLock::new();
static GPU_FALLBACK_REPORTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, Default)]
pub struct RuntimeStatus {
    pub available: bool,
    pub adapter_name: Option<String>,
    pub adapter_backend: Option<String>,
    pub adapter_driver: Option<String>,
}

/// Try a GPU compute path for preview-safe color operations.
///
/// Returns:
/// - `Some(image)` when GPU path succeeds or when no-op work can be returned directly.
/// - `None` when the edit state is not supported by this spike or GPU init/execution fails.
pub fn try_apply(img: &DynamicImage, state: &EditState) -> Option<DynamicImage> {
    if !is_gpu_state_supported(state) {
        return None;
    }

    // Avoid GPU dispatch for no-op states while still signaling that the
    // operation completed in this backend path.
    if !has_gpu_adjustments(state) {
        return Some(img.clone());
    }

    let rgba = img.to_rgba8();
    if rgba.width() == 0 || rgba.height() == 0 {
        return Some(DynamicImage::ImageRgba8(rgba));
    }

    apply_gpu(&rgba, state).map(DynamicImage::ImageRgba8)
}

pub fn is_available() -> bool {
    gpu_context().is_some()
}

pub fn runtime_status() -> RuntimeStatus {
    match gpu_context() {
        Some(ctx) => RuntimeStatus {
            available: true,
            adapter_name: Some(ctx.adapter_name.clone()),
            adapter_backend: Some(ctx.adapter_backend.clone()),
            adapter_driver: Some(ctx.adapter_driver.clone()),
        },
        None => RuntimeStatus::default(),
    }
}

fn is_gpu_state_supported(state: &EditState) -> bool {
    // Geometry and cropping remain CPU-owned in this spike.
    if state.rotate.rem_euclid(360) != 0 {
        return false;
    }
    if state.flip_h || state.flip_v || state.crop.is_some() {
        return false;
    }
    if state.straighten.abs() > 0.01 {
        return false;
    }
    if state.keystone.vertical.abs() > STATE_EPS || state.keystone.horizontal.abs() > STATE_EPS {
        return false;
    }

    // Keep selective color on CPU for parity.
    if state.selective_color.iter().any(|a| {
        a.hue.abs() > STATE_EPS || a.saturation.abs() > STATE_EPS || a.lightness.abs() > STATE_EPS
    }) {
        return false;
    }

    true
}

fn has_gpu_adjustments(state: &EditState) -> bool {
    let grad_active = state
        .graduated_filter
        .as_ref()
        .map(|grad| grad.exposure.abs() > STATE_EPS && grad.bottom > grad.top + 0.0001)
        .unwrap_or(false);
    state.exposure.abs() > STATE_EPS
        || state.contrast.abs() > STATE_EPS
        || state.highlights.abs() > STATE_EPS
        || state.shadows.abs() > STATE_EPS
        || state.temperature.abs() > STATE_EPS
        || state.saturation.abs() > STATE_EPS
        || state.hue_shift.abs() > STATE_EPS
        || grad_active
}

fn apply_gpu(src: &RgbaImage, state: &EditState) -> Option<RgbaImage> {
    let Some(ctx) = gpu_context() else {
        report_gpu_fallback_once();
        return None;
    };
    let width = src.width();
    let height = src.height();
    let extent = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let unpadded_bytes_per_row = width.saturating_mul(4);

    let src_texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("gpu_spike_src"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let dst_texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("gpu_spike_dst"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });

    ctx.queue.write_texture(
        src_texture.as_image_copy(),
        src.as_raw(),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(unpadded_bytes_per_row),
            rows_per_image: Some(height),
        },
        extent,
    );

    let (grad_enabled, grad_top, grad_bottom, grad_exposure) = state
        .graduated_filter
        .as_ref()
        .and_then(|grad| {
            let top = grad.top.clamp(0.0, 1.0);
            let bottom = grad.bottom.clamp(0.0, 1.0);
            if grad.exposure.abs() <= STATE_EPS || bottom <= top + 0.0001 {
                None
            } else {
                Some((1.0_f32, top, bottom, grad.exposure.clamp(-5.0, 5.0)))
            }
        })
        .unwrap_or((0.0, 0.0, 1.0, 0.0));

    let params: [f32; 16] = [
        width as f32,
        height as f32,
        state.exposure,
        state.contrast,
        state.highlights,
        state.shadows,
        state.temperature,
        state.saturation,
        state.hue_shift,
        grad_enabled,
        grad_top,
        grad_bottom,
        grad_exposure,
        0.0,
        0.0,
        0.0,
    ];
    let params_buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gpu_spike_params"),
        size: std::mem::size_of_val(&params) as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    ctx.queue
        .write_buffer(&params_buffer, 0, f32s_as_bytes(&params));

    let src_view = src_texture.create_view(&wgpu::TextureViewDescriptor::default());
    let dst_view = dst_texture.create_view(&wgpu::TextureViewDescriptor::default());
    let bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("gpu_spike_bind_group"),
        layout: &ctx.bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&src_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&dst_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: params_buffer.as_entire_binding(),
            },
        ],
    });

    let padded_bytes_per_row = ((unpadded_bytes_per_row + wgpu::COPY_BYTES_PER_ROW_ALIGNMENT - 1)
        / wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let readback_size = padded_bytes_per_row as u64 * height as u64;
    let readback = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gpu_spike_readback"),
        size: readback_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_spike_encoder"),
        });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("gpu_spike_compute"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&ctx.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(
            width.div_ceil(WORKGROUP_SIZE),
            height.div_ceil(WORKGROUP_SIZE),
            1,
        );
    }
    encoder.copy_texture_to_buffer(
        dst_texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        extent,
    );

    ctx.queue.submit([encoder.finish()]);
    let slice = readback.slice(..);
    let (tx, rx) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    let _ = ctx.device.poll(wgpu::Maintain::wait());
    let map_result = rx.recv().ok()?;
    if map_result.is_err() {
        report_gpu_fallback_once();
        return None;
    }

    let mapped = slice.get_mapped_range();
    let mut out = vec![0_u8; (unpadded_bytes_per_row as usize) * (height as usize)];
    let padded = padded_bytes_per_row as usize;
    let unpadded = unpadded_bytes_per_row as usize;
    for row in 0..height as usize {
        let src_offset = row * padded;
        let dst_offset = row * unpadded;
        out[dst_offset..dst_offset + unpadded]
            .copy_from_slice(&mapped[src_offset..src_offset + unpadded]);
    }
    drop(mapped);
    readback.unmap();

    let output = RgbaImage::from_raw(width, height, out);
    if output.is_none() {
        report_gpu_fallback_once();
    }
    output
}

fn gpu_context() -> Option<&'static GpuContext> {
    GPU_CONTEXT.get_or_init(init_gpu_context).as_ref()
}

fn init_gpu_context() -> Option<GpuContext> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))?;
    let adapter_info = adapter.get_info();
    let adapter_name = adapter_info.name;
    let adapter_backend = adapter_info.backend.to_string();
    let adapter_driver = if adapter_info.driver.trim().is_empty() {
        "unknown".to_string()
    } else {
        adapter_info.driver
    };
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("gpu_spike_device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
        },
        None,
    ))
    .ok()?;

    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("gpu_spike_bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("gpu_spike_shader"),
        source: wgpu::ShaderSource::Wgsl(SHADER_SRC.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("gpu_spike_layout"),
        bind_group_layouts: &[&bind_group_layout],
        push_constant_ranges: &[],
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("gpu_spike_pipeline"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: Some("main"),
        cache: None,
        compilation_options: wgpu::PipelineCompilationOptions::default(),
    });

    Some(GpuContext {
        device,
        queue,
        pipeline,
        bind_group_layout,
        adapter_name,
        adapter_backend,
        adapter_driver,
    })
}

fn report_gpu_fallback_once() {
    if !GPU_FALLBACK_REPORTED.swap(true, Ordering::Relaxed) {
        eprintln!(
            "photograph: gpu_spike unavailable or failed; preview processing falling back to CPU"
        );
    }
}

fn f32s_as_bytes(values: &[f32]) -> &[u8] {
    // f32 has no invalid bit patterns; reinterpreting as bytes is safe.
    unsafe {
        std::slice::from_raw_parts(values.as_ptr().cast::<u8>(), std::mem::size_of_val(values))
    }
}

const SHADER_SRC: &str = r#"
struct Params {
    width: f32,
    height: f32,
    exposure: f32,
    contrast: f32,
    highlights: f32,
    shadows: f32,
    temperature: f32,
    saturation: f32,
    hue_shift: f32,
    grad_enabled: f32,
    grad_top: f32,
    grad_bottom: f32,
    grad_exposure: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0)
var src_tex: texture_2d<f32>;
@group(0) @binding(1)
var dst_tex: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(2)
var<uniform> params: Params;

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = clamp((x - edge0) / (edge1 - edge0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn wrap_unit(v: f32) -> f32 {
    return fract(v + 1000.0);
}

fn rgb_to_hsl(rgb: vec3<f32>) -> vec3<f32> {
    let max_c = max(rgb.r, max(rgb.g, rgb.b));
    let min_c = min(rgb.r, min(rgb.g, rgb.b));
    let l = (max_c + min_c) * 0.5;
    let d = max_c - min_c;
    if (d <= 1e-6) {
        return vec3<f32>(0.0, 0.0, clamp(l, 0.0, 1.0));
    }

    var h: f32;
    if (max_c == rgb.r) {
        h = (rgb.g - rgb.b) / d;
        if (h < 0.0) {
            h = h + 6.0;
        }
    } else if (max_c == rgb.g) {
        h = ((rgb.b - rgb.r) / d) + 2.0;
    } else {
        h = ((rgb.r - rgb.g) / d) + 4.0;
    }
    h = h / 6.0;
    let s = d / max(1.0 - abs(2.0 * l - 1.0), 1e-6);
    return vec3<f32>(wrap_unit(h), clamp(s, 0.0, 1.0), clamp(l, 0.0, 1.0));
}

fn hue_to_rgb(p: f32, q: f32, t_raw: f32) -> f32 {
    let t = wrap_unit(t_raw);
    if (t < (1.0 / 6.0)) {
        return p + (q - p) * 6.0 * t;
    }
    if (t < 0.5) {
        return q;
    }
    if (t < (2.0 / 3.0)) {
        return p + (q - p) * ((2.0 / 3.0) - t) * 6.0;
    }
    return p;
}

fn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {
    let h = hsl.x;
    let s = hsl.y;
    let l = hsl.z;
    if (s <= 1e-6) {
        return vec3<f32>(l, l, l);
    }
    let q = select(l + s - l * s, l * (1.0 + s), l < 0.5);
    let p = 2.0 * l - q;
    let r = hue_to_rgb(p, q, h + 1.0 / 3.0);
    let g = hue_to_rgb(p, q, h);
    let b = hue_to_rgb(p, q, h - 1.0 / 3.0);
    return vec3<f32>(clamp(r, 0.0, 1.0), clamp(g, 0.0, 1.0), clamp(b, 0.0, 1.0));
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let width = u32(params.width + 0.5);
    let height = u32(params.height + 0.5);
    if (gid.x >= width || gid.y >= height) {
        return;
    }

    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let px = textureLoad(src_tex, coord, 0);
    var r = px.r;
    var g = px.g;
    var b = px.b;

    let exposure_gain = exp2(clamp(params.exposure, -5.0, 5.0));
    let contrast_gain = 1.0 + clamp(params.contrast, -1.0, 1.0);
    r = clamp((r * exposure_gain - 0.5) * contrast_gain + 0.5, 0.0, 1.0);
    g = clamp((g * exposure_gain - 0.5) * contrast_gain + 0.5, 0.0, 1.0);
    b = clamp((b * exposure_gain - 0.5) * contrast_gain + 0.5, 0.0, 1.0);

    let highlights = clamp(params.highlights, -1.0, 1.0);
    let shadows = clamp(params.shadows, -1.0, 1.0);
    let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    var target_luma = luma;

    if (abs(shadows) > 0.001) {
        let w = 1.0 - smoothstep(0.0, 0.5, target_luma);
        if (shadows >= 0.0) {
            target_luma = target_luma + (1.0 - target_luma) * shadows * w;
        } else {
            target_luma = target_luma * (1.0 + shadows * w);
        }
    }
    if (abs(highlights) > 0.001) {
        let w = smoothstep(0.5, 1.0, target_luma);
        if (highlights >= 0.0) {
            target_luma = target_luma + (1.0 - target_luma) * highlights * w;
        } else {
            target_luma = target_luma * (1.0 + highlights * w);
        }
    }
    let scale = select(1.0, target_luma / luma, luma > 1e-5);
    r = clamp(r * scale, 0.0, 1.0);
    g = clamp(g * scale, 0.0, 1.0);
    b = clamp(b * scale, 0.0, 1.0);

    let temp = clamp(params.temperature, -1.0, 1.0);
    if (temp > 0.0) {
        r = r + (1.0 - r) * temp * 0.25;
        b = b * (1.0 - temp * 0.25);
    } else if (temp < 0.0) {
        let cool = -temp;
        b = b + (1.0 - b) * cool * 0.25;
        r = r * (1.0 - cool * 0.25);
    }
    r = clamp(r, 0.0, 1.0);
    g = clamp(g, 0.0, 1.0);
    b = clamp(b, 0.0, 1.0);

    let sat_adjust = clamp(params.saturation, -1.0, 1.0);
    let hue_shift = params.hue_shift / 360.0;
    var hsl = rgb_to_hsl(vec3<f32>(r, g, b));
    hsl.x = wrap_unit(hsl.x + hue_shift);
    hsl.y = clamp(hsl.y * (1.0 + sat_adjust), 0.0, 1.0);
    var out_rgb = hsl_to_rgb(hsl);

    if (params.grad_enabled > 0.5) {
        let h_denom = max(params.height - 1.0, 1.0);
        let y_norm = f32(gid.y) / h_denom;
        var weight = 0.0;
        if (y_norm <= params.grad_top) {
            weight = 1.0;
        } else if (y_norm >= params.grad_bottom) {
            weight = 0.0;
        } else {
            weight = (params.grad_bottom - y_norm) / (params.grad_bottom - params.grad_top);
        }
        if (weight > 0.0) {
            let gain = exp2(params.grad_exposure * weight);
            out_rgb = clamp(out_rgb * vec3<f32>(gain, gain, gain), vec3<f32>(0.0), vec3<f32>(1.0));
        }
    }

    textureStore(dst_tex, coord, vec4<f32>(out_rgb, px.a));
}
"#;

#[cfg(test)]
mod tests {
    use image::{DynamicImage, ImageBuffer, Rgba};

    use crate::state::{EditState, GradFilter};

    use super::{is_gpu_state_supported, try_apply};

    #[test]
    fn rejects_geometry_states() {
        let mut s = EditState::default();
        s.rotate = 90;
        assert!(!is_gpu_state_supported(&s));

        s = EditState::default();
        s.straighten = 1.0;
        assert!(!is_gpu_state_supported(&s));
    }

    #[test]
    fn rejects_selective_states() {
        let mut s = EditState::default();
        s.selective_color[0].saturation = 0.2;
        assert!(!is_gpu_state_supported(&s));
    }

    #[test]
    fn no_op_returns_image_without_gpu_requirement() {
        let img = DynamicImage::ImageRgba8(ImageBuffer::from_pixel(2, 2, Rgba([10, 20, 30, 255])));
        let out = try_apply(&img, &EditState::default()).expect("default state should be handled");
        assert_eq!(out.to_rgba8().into_raw(), img.to_rgba8().into_raw());
    }

    #[test]
    fn parity_matches_cpu_for_supported_color_adjustments() {
        if !super::is_available() {
            return;
        }

        let img = DynamicImage::ImageRgba8(ImageBuffer::from_fn(32, 24, |x, y| {
            Rgba([
                ((x * 7 + y * 3) % 256) as u8,
                ((x * 11 + y * 5) % 256) as u8,
                ((x * 13 + y * 17) % 256) as u8,
                255,
            ])
        }));
        let mut state = EditState::default();
        state.exposure = 0.35;
        state.contrast = 0.2;
        state.highlights = -0.2;
        state.shadows = 0.2;
        state.temperature = 0.1;
        state.saturation = 0.15;
        state.hue_shift = 8.0;

        let cpu = crate::processing::transform::apply(&img, &state).to_rgba8();
        let gpu = try_apply(&img, &state)
            .expect("gpu apply should succeed for supported state")
            .to_rgba8();
        assert_rgba_close(&cpu, &gpu, 2);
    }

    #[test]
    fn parity_matches_cpu_for_supported_graduated_filter() {
        if !super::is_available() {
            return;
        }

        let img = DynamicImage::ImageRgba8(ImageBuffer::from_fn(24, 24, |x, y| {
            Rgba([
                ((x * 9 + y * 2) % 256) as u8,
                ((x * 3 + y * 11) % 256) as u8,
                ((x * 5 + y * 7) % 256) as u8,
                255,
            ])
        }));
        let mut state = EditState::default();
        state.exposure = 0.25;
        state.saturation = 0.12;
        state.graduated_filter = Some(GradFilter {
            top: 0.1,
            bottom: 0.9,
            exposure: -0.8,
        });

        let cpu = crate::processing::transform::apply(&img, &state).to_rgba8();
        let gpu = try_apply(&img, &state)
            .expect("gpu apply should succeed for supported graduated filter")
            .to_rgba8();
        assert_rgba_close(&cpu, &gpu, 2);
    }

    fn assert_rgba_close(cpu: &image::RgbaImage, gpu: &image::RgbaImage, tolerance: u8) {
        assert_eq!(cpu.dimensions(), gpu.dimensions());
        for (c, g) in cpu.pixels().zip(gpu.pixels()) {
            for i in 0..4 {
                let d = c[i].abs_diff(g[i]);
                assert!(
                    d <= tolerance,
                    "channel {} differed by {} (cpu={}, gpu={}, tol={})",
                    i,
                    d,
                    c[i],
                    g[i],
                    tolerance
                );
            }
        }
    }
}

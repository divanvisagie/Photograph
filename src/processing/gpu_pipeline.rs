use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

use image::{DynamicImage, RgbaImage};

use crate::state::EditState;

pub const DEBUG_ALLOW_CPU_FALLBACK_ENV: &str = "PHOTOGRAPH_DEBUG_ALLOW_CPU_FALLBACK";
const STATE_EPS: f32 = 0.001;
const WORKGROUP_SIZE: u32 = 16;

struct PipelineBundle {
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
}

struct GpuContext {
    device: wgpu::Device,
    queue: wgpu::Queue,
    color: PipelineBundle,
    geometry: OnceLock<PipelineBundle>,
    blur_h: OnceLock<PipelineBundle>,
    blur_v_usm: OnceLock<PipelineBundle>,
    adapter_name: String,
    adapter_backend: String,
    adapter_driver: String,
    adapter_vendor_id: u32,
}

impl GpuContext {
    fn geometry(&self) -> &PipelineBundle {
        self.geometry.get_or_init(|| {
            let entries = tex_storage_uniform_entries();
            create_pipeline_bundle(&self.device, "gpu_geometry", GEOMETRY_SHADER_SRC, &entries)
        })
    }

    fn blur_h(&self) -> &PipelineBundle {
        self.blur_h.get_or_init(|| {
            let entries = tex_storage_uniform_entries();
            create_pipeline_bundle(&self.device, "gpu_blur_h", BLUR_H_SHADER_SRC, &entries)
        })
    }

    fn blur_v_usm(&self) -> &PipelineBundle {
        self.blur_v_usm.get_or_init(|| {
            let entries = [
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
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ];
            create_pipeline_bundle(
                &self.device,
                "gpu_blur_v_usm",
                BLUR_V_USM_SHADER_SRC,
                &entries,
            )
        })
    }
}

static GPU_CONTEXT: OnceLock<Option<GpuContext>> = OnceLock::new();
static GPU_FALLBACK_REPORTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, Default)]
/// Snapshot of GPU preview runtime availability and adapter details.
pub struct RuntimeStatus {
    pub available: bool,
    pub adapter_vendor_id: Option<u32>,
    pub adapter_name: Option<String>,
    pub adapter_backend: Option<String>,
    pub adapter_driver: Option<String>,
}

/// Try a GPU compute path for preview-safe color operations.
///
/// Returns:
/// - `Some(image)` when GPU path succeeds or when no-op work can be returned directly.
/// - `None` when GPU init/execution fails.
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

    // Guard against images exceeding device texture limits (important for export).
    let max_dim = max_texture_dimension();
    if max_dim > 0 && (rgba.width() > max_dim || rgba.height() > max_dim) {
        return None;
    }

    apply_gpu(&rgba, state).map(DynamicImage::ImageRgba8)
}

/// Returns whether the GPU preview path is available.
pub fn is_available() -> bool {
    gpu_context().is_some()
}

/// Returns the device's maximum 2D texture dimension, or 0 if GPU is unavailable.
pub fn max_texture_dimension() -> u32 {
    gpu_context()
        .map(|ctx| ctx.device.limits().max_texture_dimension_2d)
        .unwrap_or(0)
}

/// Returns detailed GPU runtime status for UI diagnostics.
pub fn runtime_status() -> RuntimeStatus {
    match gpu_context() {
        Some(ctx) => RuntimeStatus {
            available: true,
            adapter_vendor_id: Some(ctx.adapter_vendor_id),
            adapter_name: Some(ctx.adapter_name.clone()),
            adapter_backend: Some(ctx.adapter_backend.clone()),
            adapter_driver: Some(ctx.adapter_driver.clone()),
        },
        None => RuntimeStatus::default(),
    }
}

pub fn allow_debug_cpu_fallback() -> bool {
    std::env::var(DEBUG_ALLOW_CPU_FALLBACK_ENV)
        .ok()
        .map(|raw| debug_fallback_truthy(&raw))
        .unwrap_or(false)
}

fn debug_fallback_truthy(raw: &str) -> bool {
    let norm = raw.trim().to_ascii_lowercase();
    norm == "1" || norm == "true" || norm == "yes" || norm == "on"
}

fn is_gpu_state_supported(_state: &EditState) -> bool {
    true
}

fn has_geometry(state: &EditState) -> bool {
    state.rotate.rem_euclid(360) != 0
        || state.flip_h
        || state.flip_v
        || state.crop.is_some()
        || state.straighten.abs() > 0.01
        || state.keystone.vertical.abs() > STATE_EPS
        || state.keystone.horizontal.abs() > STATE_EPS
}

fn has_gpu_adjustments(state: &EditState) -> bool {
    let grad_active = state
        .graduated_filter
        .as_ref()
        .map(|grad| grad.exposure.abs() > STATE_EPS && grad.bottom > grad.top + 0.0001)
        .unwrap_or(false);
    let selective_active = state.selective_color.iter().any(|a| {
        a.hue.abs() > STATE_EPS || a.saturation.abs() > STATE_EPS || a.lightness.abs() > STATE_EPS
    });
    state.exposure.abs() > STATE_EPS
        || state.contrast.abs() > STATE_EPS
        || state.highlights.abs() > STATE_EPS
        || state.shadows.abs() > STATE_EPS
        || state.temperature.abs() > STATE_EPS
        || state.saturation.abs() > STATE_EPS
        || state.hue_shift.abs() > STATE_EPS
        || grad_active
        || selective_active
        || state.sharpness > STATE_EPS
        || has_geometry(state)
}

/// Compute output dimensions after geometry transforms (rotation + crop).
fn compute_geometry_output_dims(state: &EditState, src_w: u32, src_h: u32) -> (u32, u32) {
    let (mut w, mut h) = match state.rotate.rem_euclid(360) {
        90 | 270 => (src_h, src_w),
        _ => (src_w, src_h),
    };
    if let Some(ref crop) = state.crop {
        // Match CPU: (crop.x * w) as u32, then cw = (crop.width * w).min(w - cx)
        let cx = (crop.x * w as f32) as u32;
        let cy = (crop.y * h as f32) as u32;
        let cw = (crop.width * w as f32).min(w as f32 - cx as f32) as u32;
        let ch = (crop.height * h as f32).min(h as f32 - cy as f32) as u32;
        if cw > 0 && ch > 0 {
            w = cw;
            h = ch;
        }
    }
    (w.max(1), h.max(1))
}

/// Compute the 3×3 inverse perspective matrix for dst→src mapping.
/// Replicates the control-point logic from transform.rs:apply_keystone.
/// Returns [f32; 9] in row-major order, or identity if no keystone.
fn compute_perspective_matrix(state: &EditState, w: f32, h: f32) -> [f32; 9] {
    let v = state.keystone.vertical;
    let hz = state.keystone.horizontal;
    if v.abs() <= STATE_EPS && hz.abs() <= STATE_EPS {
        return [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    }

    // Source corners: TL, TR, BR, BL (same as CPU)
    let src: [(f64, f64); 4] = [
        (0.0, 0.0),
        (w as f64, 0.0),
        (w as f64, h as f64),
        (0.0, h as f64),
    ];

    // Destination corners with perspective shifts
    let v = v as f64;
    let hz = hz as f64;
    let w64 = w as f64;
    let h64 = h as f64;
    let dst: [(f64, f64); 4] = [
        (v.max(0.0) * w64, hz.max(0.0) * h64),
        (w64 - v.max(0.0) * w64, (-hz).max(0.0) * h64),
        (w64 - (-v).max(0.0) * w64, h64 - (-hz).max(0.0) * h64),
        ((-v).max(0.0) * w64, h64 - hz.max(0.0) * h64),
    ];

    // imageproc's from_control_points(src, dst) creates P mapping input→output.
    // warp() internally inverts P to get output→input for sampling.
    // Our GPU shader applies the matrix directly to output coords to get input coords,
    // so we need the inverse: dst→src (output corners → input sample locations).
    if let Some(mat) = compute_homography(&dst, &src) {
        [
            mat[0] as f32,
            mat[1] as f32,
            mat[2] as f32,
            mat[3] as f32,
            mat[4] as f32,
            mat[5] as f32,
            mat[6] as f32,
            mat[7] as f32,
            mat[8] as f32,
        ]
    } else {
        [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]
    }
}

/// Compute 3×3 homography mapping src corners → dst corners using DLT.
/// Returns None if the system is degenerate.
fn compute_homography(src: &[(f64, f64); 4], dst: &[(f64, f64); 4]) -> Option<[f64; 9]> {
    // Build 8×8 system Ah = b where h = [h0..h7], h8 = 1
    // For each point pair (sx,sy) → (dx,dy):
    //   sx = (h0*dx + h1*dy + h2) / (h6*dx + h7*dy + 1)
    //   sy = (h3*dx + h4*dy + h5) / (h6*dx + h7*dy + 1)
    // Rearranged:
    //   h0*dx + h1*dy + h2 - h6*dx*sx - h7*dy*sx = sx
    //   h3*dx + h4*dy + h5 - h6*dx*sy - h7*dy*sy = sy
    let mut a = [[0.0_f64; 8]; 8];
    let mut b = [0.0_f64; 8];

    for i in 0..4 {
        let (dx, dy) = src[i]; // "from" coords
        let (sx, sy) = dst[i]; // "to" coords
        let row1 = i * 2;
        let row2 = i * 2 + 1;
        a[row1] = [dx, dy, 1.0, 0.0, 0.0, 0.0, -dx * sx, -dy * sx];
        b[row1] = sx;
        a[row2] = [0.0, 0.0, 0.0, dx, dy, 1.0, -dx * sy, -dy * sy];
        b[row2] = sy;
    }

    // Gaussian elimination with partial pivoting
    for col in 0..8 {
        // Find pivot
        let mut max_row = col;
        let mut max_val = a[col][col].abs();
        for row in (col + 1)..8 {
            if a[row][col].abs() > max_val {
                max_val = a[row][col].abs();
                max_row = row;
            }
        }
        if max_val < 1e-12 {
            return None;
        }
        if max_row != col {
            a.swap(col, max_row);
            b.swap(col, max_row);
        }
        let pivot = a[col][col];
        for j in col..8 {
            a[col][j] /= pivot;
        }
        b[col] /= pivot;
        for row in 0..8 {
            if row == col {
                continue;
            }
            let factor = a[row][col];
            for j in col..8 {
                a[row][j] -= factor * a[col][j];
            }
            b[row] -= factor * b[col];
        }
    }

    Some([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], 1.0])
}

fn apply_gpu(src: &RgbaImage, state: &EditState) -> Option<RgbaImage> {
    let Some(ctx) = gpu_context() else {
        report_gpu_fallback_once();
        return None;
    };
    let src_w = src.width();
    let src_h = src.height();
    let src_extent = wgpu::Extent3d {
        width: src_w,
        height: src_h,
        depth_or_array_layers: 1,
    };
    let needs_geometry = has_geometry(state);
    let needs_sharpness = state.sharpness > STATE_EPS;

    // Compute output dimensions after geometry
    let (out_w, out_h) = if needs_geometry {
        compute_geometry_output_dims(state, src_w, src_h)
    } else {
        (src_w, src_h)
    };
    let out_extent = wgpu::Extent3d {
        width: out_w,
        height: out_h,
        depth_or_array_layers: 1,
    };

    // Source texture (uploaded from CPU)
    let src_texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("gpu_pipeline_src"),
        size: src_extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    ctx.queue.write_texture(
        src_texture.as_image_copy(),
        src.as_raw(),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(src_w.saturating_mul(4)),
            rows_per_image: Some(src_h),
        },
        src_extent,
    );

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_pipeline_encoder"),
        });

    // The texture that feeds into the color pass — either geometry output or src
    let color_input_texture;
    let color_input_view;

    if needs_geometry {
        // Geometry output: different dimensions, needs TEXTURE_BINDING for color pass to read
        let geo_out = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("gpu_pipeline_geo_out"),
            size: out_extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });

        // Build geometry uniform (24 floats, padded to 28 for vec4 alignment)
        let perspective = compute_perspective_matrix(state, src_w as f32, src_h as f32);
        let rot = state.rotate.rem_euclid(360);
        let straighten_rad = state.straighten.to_radians();

        let (crop_x, crop_y, crop_w, crop_h) = match &state.crop {
            Some(crop) => {
                // Crop is applied after rotation, so use post-rotation dims.
                // Truncate to integer to match CPU's `(crop.x * w) as u32`.
                let (rw, rh) = match rot {
                    90 | 270 => (src_h, src_w),
                    _ => (src_w, src_h),
                };
                let cx = (crop.x * rw as f32) as u32;
                let cy = (crop.y * rh as f32) as u32;
                let cw = (crop.width * rw as f32).min(rw as f32 - cx as f32) as u32;
                let ch = (crop.height * rh as f32).min(rh as f32 - cy as f32) as u32;
                (cx as f32, cy as f32, cw as f32, ch as f32)
            }
            None => {
                let (rw, rh) = match rot {
                    90 | 270 => (src_h, src_w),
                    _ => (src_w, src_h),
                };
                (0.0, 0.0, rw as f32, rh as f32)
            }
        };

        // 28 floats: geometry uniform layout
        // [0-3]: src_width, src_height, dst_width, dst_height
        // [4-7]: straighten_rad, rotate_mode, flip_h, flip_v
        // [8-11]: crop_x, crop_y, crop_w, crop_h
        // [12-14, pad]: perspective row 0 (3 floats + pad)
        // [16-18, pad]: perspective row 1 (3 floats + pad)
        // [20-22, pad]: perspective row 2 (3 floats + pad)
        // = 24 floats total
        let mut geo_params: [f32; 24] = [0.0; 24];
        geo_params[0] = src_w as f32;
        geo_params[1] = src_h as f32;
        geo_params[2] = out_w as f32;
        geo_params[3] = out_h as f32;
        geo_params[4] = straighten_rad;
        geo_params[5] = rot as f32;
        geo_params[6] = if state.flip_h { 1.0 } else { 0.0 };
        geo_params[7] = if state.flip_v { 1.0 } else { 0.0 };
        geo_params[8] = crop_x;
        geo_params[9] = crop_y;
        geo_params[10] = crop_w;
        geo_params[11] = crop_h;
        // Perspective matrix in row-major, padded to vec4 rows
        geo_params[12] = perspective[0];
        geo_params[13] = perspective[1];
        geo_params[14] = perspective[2];
        // [15] = pad
        geo_params[16] = perspective[3];
        geo_params[17] = perspective[4];
        geo_params[18] = perspective[5];
        // [19] = pad
        geo_params[20] = perspective[6];
        geo_params[21] = perspective[7];
        geo_params[22] = perspective[8];
        // [23] = pad

        let geo_params_buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("gpu_pipeline_geo_params"),
            size: std::mem::size_of_val(&geo_params) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue
            .write_buffer(&geo_params_buffer, 0, f32s_as_bytes(&geo_params));

        let src_view = src_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let geo_out_view = geo_out.create_view(&wgpu::TextureViewDescriptor::default());

        let geo_bundle = ctx.geometry();
        let geo_bg = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("gpu_pipeline_geo_bg"),
            layout: &geo_bundle.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&src_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&geo_out_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: geo_params_buffer.as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("gpu_pipeline_geo_pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&geo_bundle.pipeline);
            pass.set_bind_group(0, &geo_bg, &[]);
            pass.dispatch_workgroups(
                out_w.div_ceil(WORKGROUP_SIZE),
                out_h.div_ceil(WORKGROUP_SIZE),
                1,
            );
        }

        color_input_view = geo_out_view;
        color_input_texture = geo_out;
    } else {
        color_input_view = src_texture.create_view(&wgpu::TextureViewDescriptor::default());
        color_input_texture = src_texture;
    };
    // Keep color_input_texture alive (it owns the GPU memory)
    let _color_input_texture = color_input_texture;

    // Color output — needs TEXTURE_BINDING when sharpness follows
    let color_out_usage = if needs_sharpness {
        wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING
    } else {
        wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC
    };
    let color_out_texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("gpu_pipeline_color_out"),
        size: out_extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: color_out_usage,
        view_formats: &[],
    });

    // Build color params uniform (uses output dimensions)
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

    let mut params: [f32; 40] = [0.0; 40];
    params[0] = out_w as f32;
    params[1] = out_h as f32;
    params[2] = state.exposure;
    params[3] = state.contrast;
    params[4] = state.highlights;
    params[5] = state.shadows;
    params[6] = state.temperature;
    params[7] = state.saturation;
    params[8] = state.hue_shift;
    params[9] = grad_enabled;
    params[10] = grad_top;
    params[11] = grad_bottom;
    params[12] = grad_exposure;
    for (i, adj) in state.selective_color.iter().enumerate() {
        params[16 + i * 3] = adj.hue;
        params[16 + i * 3 + 1] = adj.saturation;
        params[16 + i * 3 + 2] = adj.lightness;
    }
    let color_params_buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gpu_pipeline_color_params"),
        size: std::mem::size_of_val(&params) as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    ctx.queue
        .write_buffer(&color_params_buffer, 0, f32s_as_bytes(&params));

    let color_out_view = color_out_texture.create_view(&wgpu::TextureViewDescriptor::default());
    let color_bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("gpu_pipeline_color_bg"),
        layout: &ctx.color.bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&color_input_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&color_out_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: color_params_buffer.as_entire_binding(),
            },
        ],
    });

    // Color pass
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("gpu_pipeline_color_pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&ctx.color.pipeline);
        pass.set_bind_group(0, &color_bind_group, &[]);
        pass.dispatch_workgroups(
            out_w.div_ceil(WORKGROUP_SIZE),
            out_h.div_ceil(WORKGROUP_SIZE),
            1,
        );
    }

    // Sharpness passes (using output dimensions)
    let final_texture = if needs_sharpness {
        let blur_h_texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("gpu_pipeline_blur_h"),
            size: out_extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let usm_out_texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("gpu_pipeline_usm_out"),
            size: out_extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        let blur_params: [f32; 4] = [out_w as f32, out_h as f32, state.sharpness, 0.0];
        let blur_params_buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("gpu_pipeline_blur_params"),
            size: std::mem::size_of_val(&blur_params) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue
            .write_buffer(&blur_params_buffer, 0, f32s_as_bytes(&blur_params));

        let blur_h_view = blur_h_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let usm_out_view = usm_out_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let blur_h_bundle = ctx.blur_h();
        let blur_v_usm_bundle = ctx.blur_v_usm();

        let blur_h_bg = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("gpu_pipeline_blur_h_bg"),
            layout: &blur_h_bundle.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&color_out_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&blur_h_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: blur_params_buffer.as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("gpu_pipeline_blur_h_pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&blur_h_bundle.pipeline);
            pass.set_bind_group(0, &blur_h_bg, &[]);
            pass.dispatch_workgroups(
                out_w.div_ceil(WORKGROUP_SIZE),
                out_h.div_ceil(WORKGROUP_SIZE),
                1,
            );
        }

        let blur_v_usm_bg = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("gpu_pipeline_blur_v_usm_bg"),
            layout: &blur_v_usm_bundle.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&blur_h_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&color_out_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&usm_out_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: blur_params_buffer.as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("gpu_pipeline_blur_v_usm_pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&blur_v_usm_bundle.pipeline);
            pass.set_bind_group(0, &blur_v_usm_bg, &[]);
            pass.dispatch_workgroups(
                out_w.div_ceil(WORKGROUP_SIZE),
                out_h.div_ceil(WORKGROUP_SIZE),
                1,
            );
        }

        usm_out_texture
    } else {
        color_out_texture
    };

    // Readback
    let unpadded_bytes_per_row = out_w.saturating_mul(4);
    let padded_bytes_per_row = ((unpadded_bytes_per_row + wgpu::COPY_BYTES_PER_ROW_ALIGNMENT - 1)
        / wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let readback_size = padded_bytes_per_row as u64 * out_h as u64;
    let readback = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gpu_pipeline_readback"),
        size: readback_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        final_texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(out_h),
            },
        },
        out_extent,
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
    let unpadded = unpadded_bytes_per_row as usize;
    let padded = padded_bytes_per_row as usize;
    let mut out = vec![0_u8; unpadded * (out_h as usize)];
    for row in 0..out_h as usize {
        let src_offset = row * padded;
        let dst_offset = row * unpadded;
        out[dst_offset..dst_offset + unpadded]
            .copy_from_slice(&mapped[src_offset..src_offset + unpadded]);
    }
    drop(mapped);
    readback.unmap();

    let output = RgbaImage::from_raw(out_w, out_h, out);
    if output.is_none() {
        report_gpu_fallback_once();
    }
    output
}

fn gpu_context() -> Option<&'static GpuContext> {
    GPU_CONTEXT.get_or_init(init_gpu_context).as_ref()
}

fn create_pipeline_bundle(
    device: &wgpu::Device,
    label: &str,
    shader_src: &str,
    bgl_entries: &[wgpu::BindGroupLayoutEntry],
) -> PipelineBundle {
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: bgl_entries,
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(shader_src.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[&bgl],
        push_constant_ranges: &[],
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        module: &shader,
        entry_point: Some("main"),
        cache: None,
        compilation_options: wgpu::PipelineCompilationOptions::default(),
    });
    PipelineBundle { pipeline, bgl }
}

/// Standard bind group layout entries: texture_2d input + storage_texture output + uniform buffer.
fn tex_storage_uniform_entries() -> [wgpu::BindGroupLayoutEntry; 3] {
    [
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
    ]
}

fn init_gpu_context() -> Option<GpuContext> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))?;
    let adapter_info = adapter.get_info();
    if adapter_info.backend != wgpu::Backend::Vulkan {
        return None;
    }
    if adapter_info.device_type != wgpu::DeviceType::DiscreteGpu {
        return None;
    }
    let adapter_vendor_id = adapter_info.vendor;
    let adapter_name = adapter_info.name;
    let adapter_backend = adapter_info.backend.to_string();
    let adapter_driver = if adapter_info.driver.trim().is_empty() {
        "unknown".to_string()
    } else {
        adapter_info.driver
    };
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("gpu_pipeline_device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
        },
        None,
    ))
    .ok()?;

    let standard_entries = tex_storage_uniform_entries();
    let color = create_pipeline_bundle(&device, "gpu_color", COLOR_SHADER_SRC, &standard_entries);

    Some(GpuContext {
        device,
        queue,
        color,
        geometry: OnceLock::new(),
        blur_h: OnceLock::new(),
        blur_v_usm: OnceLock::new(),
        adapter_name,
        adapter_backend,
        adapter_driver,
        adapter_vendor_id,
    })
}

fn report_gpu_fallback_once() {
    if !GPU_FALLBACK_REPORTED.swap(true, Ordering::Relaxed) {
        eprintln!(
            "photograph: gpu_pipeline unavailable or failed; set {}=1 to enable debug CPU fallback",
            DEBUG_ALLOW_CPU_FALLBACK_ENV
        );
    }
}

fn f32s_as_bytes(values: &[f32]) -> &[u8] {
    // f32 has no invalid bit patterns; reinterpreting as bytes is safe.
    unsafe {
        std::slice::from_raw_parts(values.as_ptr().cast::<u8>(), std::mem::size_of_val(values))
    }
}

const COLOR_SHADER_SRC: &str = r#"
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
    // 8 selective color ranges × 3 (hue, saturation, lightness)
    sel_hue_0: f32, sel_sat_0: f32, sel_light_0: f32,
    sel_hue_1: f32, sel_sat_1: f32, sel_light_1: f32,
    sel_hue_2: f32, sel_sat_2: f32, sel_light_2: f32,
    sel_hue_3: f32, sel_sat_3: f32, sel_light_3: f32,
    sel_hue_4: f32, sel_sat_4: f32, sel_light_4: f32,
    sel_hue_5: f32, sel_sat_5: f32, sel_light_5: f32,
    sel_hue_6: f32, sel_sat_6: f32, sel_light_6: f32,
    sel_hue_7: f32, sel_sat_7: f32, sel_light_7: f32,
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

fn hue_distance_deg(a: f32, b: f32) -> f32 {
    let diff = abs(a - b);
    return min(diff, 360.0 - diff);
}

fn selective_weight(hue_unit: f32, center_deg: f32, half_width: f32) -> f32 {
    let hue_deg = wrap_unit(hue_unit) * 360.0;
    let dist = hue_distance_deg(hue_deg, center_deg);
    if (dist >= half_width) {
        return 0.0;
    }
    return 1.0 - (dist / half_width);
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

    // Selective color: 8 unrolled blocks matching CPU centers
    // [0, 30, 60, 120, 180, 240, 285, 330] with half_width=30
    {
        let sel_hues = array<f32, 8>(
            params.sel_hue_0, params.sel_hue_1, params.sel_hue_2, params.sel_hue_3,
            params.sel_hue_4, params.sel_hue_5, params.sel_hue_6, params.sel_hue_7
        );
        let sel_sats = array<f32, 8>(
            params.sel_sat_0, params.sel_sat_1, params.sel_sat_2, params.sel_sat_3,
            params.sel_sat_4, params.sel_sat_5, params.sel_sat_6, params.sel_sat_7
        );
        let sel_lights = array<f32, 8>(
            params.sel_light_0, params.sel_light_1, params.sel_light_2, params.sel_light_3,
            params.sel_light_4, params.sel_light_5, params.sel_light_6, params.sel_light_7
        );
        let centers = array<f32, 8>(0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 285.0, 330.0);
        let half_w = 30.0;
        for (var ci = 0u; ci < 8u; ci = ci + 1u) {
            let sh = sel_hues[ci];
            let ss = sel_sats[ci];
            let sl = sel_lights[ci];
            if (abs(sh) < 0.001 && abs(ss) < 0.001 && abs(sl) < 0.001) {
                continue;
            }
            let w = selective_weight(hsl.x, centers[ci], half_w);
            if (w <= 0.0) {
                continue;
            }
            hsl.x = wrap_unit(hsl.x + (sh / 360.0) * w);
            hsl.y = clamp(hsl.y * (1.0 + ss * w), 0.0, 1.0);
            hsl.z = clamp(hsl.z + sl * w, 0.0, 1.0);
        }
    }

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

// Geometry transform shader: inverse-maps each output pixel to source coordinates.
// Pipeline order (CPU): straighten → keystone → rotate → flip → crop
// Inverse order (GPU, per output pixel): undo crop → undo flip → undo rotate → undo keystone → undo straighten
const GEOMETRY_SHADER_SRC: &str = r#"
struct GeoParams {
    src_width: f32,
    src_height: f32,
    dst_width: f32,
    dst_height: f32,
    straighten_rad: f32,
    rotate_mode: f32,
    flip_h: f32,
    flip_v: f32,
    crop_x: f32,
    crop_y: f32,
    crop_w: f32,
    crop_h: f32,
    // Perspective matrix rows (vec4 padded, only xyz used)
    persp_r0: vec4<f32>,
    persp_r1: vec4<f32>,
    persp_r2: vec4<f32>,
};

@group(0) @binding(0)
var src_tex: texture_2d<f32>;
@group(0) @binding(1)
var dst_tex: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(2)
var<uniform> params: GeoParams;

fn bilinear_sample(x: f32, y: f32, sw: i32, sh: i32) -> vec4<f32> {
    let fx = floor(x);
    let fy = floor(y);
    let ix = i32(fx);
    let iy = i32(fy);
    let dx = x - fx;
    let dy = y - fy;

    // Near-integer coords (from exact pixel remaps like rotation/flip):
    // use nearest neighbor to avoid needing the next pixel.
    if (dx < 0.001 && dy < 0.001) {
        if (ix < 0 || ix >= sw || iy < 0 || iy >= sh) {
            return vec4<f32>(0.0, 0.0, 0.0, 1.0);
        }
        return textureLoad(src_tex, vec2<i32>(ix, iy), 0);
    }

    // Strict bilinear bounds matching imageproc::interpolate_bilinear:
    // requires floor(x) >= 0 && floor(x)+1 < width (and same for y).
    if (ix < 0 || ix + 1 >= sw || iy < 0 || iy + 1 >= sh) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    let p00 = textureLoad(src_tex, vec2<i32>(ix, iy), 0);
    let p10 = textureLoad(src_tex, vec2<i32>(ix + 1, iy), 0);
    let p01 = textureLoad(src_tex, vec2<i32>(ix, iy + 1), 0);
    let p11 = textureLoad(src_tex, vec2<i32>(ix + 1, iy + 1), 0);

    let top = mix(p00, p10, dx);
    let bot = mix(p01, p11, dx);
    return mix(top, bot, dy);
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dw = i32(params.dst_width + 0.5);
    let dh = i32(params.dst_height + 0.5);
    if (i32(gid.x) >= dw || i32(gid.y) >= dh) {
        return;
    }

    let sw = i32(params.src_width + 0.5);
    let sh = i32(params.src_height + 0.5);
    let rot = i32(params.rotate_mode + 0.5);

    // Start with output pixel coordinate
    var px = f32(gid.x) + 0.5;
    var py = f32(gid.y) + 0.5;

    // 1. Inverse crop: add crop offset
    px = px + params.crop_x;
    py = py + params.crop_y;

    // 2. Inverse flip
    // Post-rotation dimensions (before crop)
    var post_rot_w: f32;
    var post_rot_h: f32;
    if (rot == 90 || rot == 270) {
        post_rot_w = params.src_height;
        post_rot_h = params.src_width;
    } else {
        post_rot_w = params.src_width;
        post_rot_h = params.src_height;
    }

    if (params.flip_h > 0.5) {
        px = post_rot_w - px;
    }
    if (params.flip_v > 0.5) {
        py = post_rot_h - py;
    }

    // 3. Inverse orthogonal rotation
    // image crate rotate90 on W×H → H×W: out(ox,oy) = in(oy, H-1-ox)
    // Inverse: source_x = oy = py, source_y = H-1-ox = src_h - px
    // rotate270: out(ox,oy) = in(W-1-oy, ox)
    // Inverse: source_x = W-1-oy = src_w - py, source_y = ox = px
    var rx: f32;
    var ry: f32;
    if (rot == 90) {
        rx = py;
        ry = params.src_height - px;
    } else if (rot == 180) {
        rx = params.src_width - px;
        ry = params.src_height - py;
    } else if (rot == 270) {
        rx = params.src_width - py;
        ry = px;
    } else {
        rx = px;
        ry = py;
    }

    // Convert from pixel-center space to integer-pixel coords.
    // imageproc's warp()/rotate_about_center() use integer pixel coords (x, y),
    // not pixel centers (x+0.5, y+0.5). This matters for perspective (non-linear).
    rx = rx - 0.5;
    ry = ry - 0.5;

    // 4. Inverse perspective (homography: output→source mapping)
    let denom = params.persp_r2.x * rx + params.persp_r2.y * ry + params.persp_r2.z;
    if (abs(denom) > 1e-8) {
        let nx = (params.persp_r0.x * rx + params.persp_r0.y * ry + params.persp_r0.z) / denom;
        let ny = (params.persp_r1.x * rx + params.persp_r1.y * ry + params.persp_r1.z) / denom;
        rx = nx;
        ry = ny;
    }

    // 5. Inverse straighten (rotate about center by negated angle)
    if (abs(params.straighten_rad) > 0.0001) {
        let cx = f32(sw) * 0.5;
        let cy = f32(sh) * 0.5;
        let drx = rx - cx;
        let dry = ry - cy;
        let angle = -params.straighten_rad;
        let cos_a = cos(angle);
        let sin_a = sin(angle);
        rx = drx * cos_a - dry * sin_a + cx;
        ry = drx * sin_a + dry * cos_a + cy;
    }

    // rx, ry are now in integer-pixel coords — pass directly to bilinear_sample
    var color: vec4<f32>;
    color = bilinear_sample(rx, ry, sw, sh);

    textureStore(dst_tex, vec2<i32>(i32(gid.x), i32(gid.y)), color);
}
"#;

// Horizontal separable Gaussian blur (sigma=1.5, radius=5, 11 taps)
// Fully unrolled to avoid driver crashes from array+loop SPIR-V patterns.
const BLUR_H_SHADER_SRC: &str = r#"
struct BlurParams {
    width: f32,
    height: f32,
    sharpness: f32,
    _pad: f32,
};

@group(0) @binding(0)
var src_tex: texture_2d<f32>;
@group(0) @binding(1)
var dst_tex: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(2)
var<uniform> params: BlurParams;

fn sample_h(cx: i32, y: i32, offset: i32, w: i32) -> vec4<f32> {
    let sx = clamp(cx + offset, 0, w - 1);
    return textureLoad(src_tex, vec2<i32>(sx, y), 0);
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let w = i32(params.width + 0.5);
    let h = i32(params.height + 0.5);
    if (i32(gid.x) >= w || i32(gid.y) >= h) {
        return;
    }

    // Normalized Gaussian weights for sigma=1.5, radius=5
    let cx = i32(gid.x);
    let y = i32(gid.y);
    var acc = sample_h(cx, y, -5, w) * 0.0010284
            + sample_h(cx, y, -4, w) * 0.0075988
            + sample_h(cx, y, -3, w) * 0.0360008
            + sample_h(cx, y, -2, w) * 0.1093607
            + sample_h(cx, y, -1, w) * 0.2130055
            + sample_h(cx, y,  0, w) * 0.2660117
            + sample_h(cx, y,  1, w) * 0.2130055
            + sample_h(cx, y,  2, w) * 0.1093607
            + sample_h(cx, y,  3, w) * 0.0360008
            + sample_h(cx, y,  4, w) * 0.0075988
            + sample_h(cx, y,  5, w) * 0.0010284;
    textureStore(dst_tex, vec2<i32>(cx, y), acc);
}
"#;

// Vertical Gaussian blur + USM blend in one pass.
// Fully unrolled to avoid driver crashes from array+loop SPIR-V patterns.
const BLUR_V_USM_SHADER_SRC: &str = r#"
struct BlurParams {
    width: f32,
    height: f32,
    sharpness: f32,
    _pad: f32,
};

@group(0) @binding(0)
var blur_h_tex: texture_2d<f32>;
@group(0) @binding(1)
var orig_tex: texture_2d<f32>;
@group(0) @binding(2)
var dst_tex: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(3)
var<uniform> params: BlurParams;

fn sample_v(x: i32, cy: i32, offset: i32, h: i32) -> vec3<f32> {
    let sy = clamp(cy + offset, 0, h - 1);
    return textureLoad(blur_h_tex, vec2<i32>(x, sy), 0).rgb;
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let w = i32(params.width + 0.5);
    let h = i32(params.height + 0.5);
    if (i32(gid.x) >= w || i32(gid.y) >= h) {
        return;
    }

    let x = i32(gid.x);
    let cy = i32(gid.y);

    // Vertical blur on the H-blurred texture (unrolled, same weights)
    let blurred = sample_v(x, cy, -5, h) * 0.0010284
                + sample_v(x, cy, -4, h) * 0.0075988
                + sample_v(x, cy, -3, h) * 0.0360008
                + sample_v(x, cy, -2, h) * 0.1093607
                + sample_v(x, cy, -1, h) * 0.2130055
                + sample_v(x, cy,  0, h) * 0.2660117
                + sample_v(x, cy,  1, h) * 0.2130055
                + sample_v(x, cy,  2, h) * 0.1093607
                + sample_v(x, cy,  3, h) * 0.0360008
                + sample_v(x, cy,  4, h) * 0.0075988
                + sample_v(x, cy,  5, h) * 0.0010284;

    // USM: sharp = orig + amount * (orig - blurred)
    let coord = vec2<i32>(x, cy);
    let orig = textureLoad(orig_tex, coord, 0);
    let sharp = clamp(orig.rgb + params.sharpness * (orig.rgb - blurred), vec3<f32>(0.0), vec3<f32>(1.0));
    textureStore(dst_tex, coord, vec4<f32>(sharp, orig.a));
}
"#;

#[cfg(test)]
mod tests {
    use image::{DynamicImage, ImageBuffer, Rgba};

    use crate::state::{EditState, GradFilter, Rect};

    use super::{debug_fallback_truthy, has_gpu_adjustments, is_gpu_state_supported, try_apply};

    #[test]
    fn accepts_all_states() {
        // GPU path now supports all states
        let mut s = EditState::default();
        s.rotate = 90;
        assert!(is_gpu_state_supported(&s));

        s = EditState::default();
        s.straighten = 1.0;
        assert!(is_gpu_state_supported(&s));
    }

    #[test]
    fn debug_fallback_truthy_parser_matches_expected_values() {
        assert!(debug_fallback_truthy("1"));
        assert!(debug_fallback_truthy(" true "));
        assert!(debug_fallback_truthy("YES"));
        assert!(debug_fallback_truthy("on"));
        assert!(!debug_fallback_truthy("0"));
        assert!(!debug_fallback_truthy("false"));
        assert!(!debug_fallback_truthy("no"));
    }

    #[test]
    fn accepts_selective_color_states() {
        let mut s = EditState::default();
        s.selective_color[0].saturation = 0.2;
        assert!(is_gpu_state_supported(&s));

        let mut s2 = EditState::default();
        s2.selective_color[3].hue = 10.0;
        s2.selective_color[5].lightness = -0.2;
        assert!(is_gpu_state_supported(&s2));
    }

    #[test]
    fn has_gpu_adjustments_includes_selective_color() {
        let mut s = EditState::default();
        s.selective_color[2].hue = 15.0;
        assert!(has_gpu_adjustments(&s));

        let mut s2 = EditState::default();
        s2.selective_color[7].lightness = 0.1;
        assert!(has_gpu_adjustments(&s2));
    }

    #[test]
    fn parity_matches_cpu_for_selective_color() {
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
        state.selective_color[0].saturation = -0.5;
        state.selective_color[2].hue = 15.0;
        state.selective_color[5].lightness = 0.1;

        let cpu = crate::processing::transform::apply(&img, &state).to_rgba8();
        let gpu = try_apply(&img, &state)
            .expect("gpu apply should succeed for selective color state")
            .to_rgba8();
        assert_rgba_close(&cpu, &gpu, 2);
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

    #[test]
    fn has_gpu_adjustments_includes_sharpness() {
        let mut s = EditState::default();
        s.sharpness = 0.5;
        assert!(has_gpu_adjustments(&s));
    }

    #[test]
    fn parity_matches_cpu_for_sharpness() {
        if !super::is_available() {
            return;
        }

        // Edge pattern: left dark, right bright
        let img = DynamicImage::ImageRgba8(ImageBuffer::from_fn(32, 24, |x, _y| {
            if x < 16 {
                Rgba([40, 40, 40, 255])
            } else {
                Rgba([200, 200, 200, 255])
            }
        }));
        let mut state = EditState::default();
        state.sharpness = 1.0;

        let cpu = crate::processing::transform::apply(&img, &state).to_rgba8();
        let gpu = try_apply(&img, &state)
            .expect("gpu apply should succeed for sharpness state")
            .to_rgba8();
        assert_rgba_close(&cpu, &gpu, 3);
    }

    #[test]
    fn parity_matches_cpu_combined_color_and_sharpness() {
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
        state.exposure = 0.3;
        state.saturation = 0.1;
        state.sharpness = 0.5;

        let cpu = crate::processing::transform::apply(&img, &state).to_rgba8();
        let gpu = try_apply(&img, &state)
            .expect("gpu apply should succeed for combined color and sharpness")
            .to_rgba8();
        // Tolerance 4: color quantization at intermediate texture boundary adds 1 LSB rounding
        assert_rgba_close(&cpu, &gpu, 4);
    }

    #[test]
    fn accepts_geometry_states() {
        let mut s = EditState::default();
        s.rotate = 90;
        assert!(is_gpu_state_supported(&s));

        let mut s2 = EditState::default();
        s2.flip_h = true;
        assert!(is_gpu_state_supported(&s2));

        let mut s3 = EditState::default();
        s3.straighten = 5.0;
        assert!(is_gpu_state_supported(&s3));

        let mut s4 = EditState::default();
        s4.crop = Some(Rect {
            x: 0.1,
            y: 0.1,
            width: 0.8,
            height: 0.8,
        });
        assert!(is_gpu_state_supported(&s4));

        let mut s5 = EditState::default();
        s5.keystone.vertical = 0.1;
        assert!(is_gpu_state_supported(&s5));
    }

    #[test]
    fn parity_rotate_90() {
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
        state.rotate = 90;
        let cpu = crate::processing::transform::apply(&img, &state).to_rgba8();
        let gpu = try_apply(&img, &state)
            .expect("gpu apply should succeed for rotate 90")
            .to_rgba8();
        assert_rgba_close(&cpu, &gpu, 0);
    }

    #[test]
    fn parity_rotate_180() {
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
        state.rotate = 180;
        let cpu = crate::processing::transform::apply(&img, &state).to_rgba8();
        let gpu = try_apply(&img, &state)
            .expect("gpu apply should succeed for rotate 180")
            .to_rgba8();
        assert_rgba_close(&cpu, &gpu, 0);
    }

    #[test]
    fn parity_flip_h() {
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
        state.flip_h = true;
        let cpu = crate::processing::transform::apply(&img, &state).to_rgba8();
        let gpu = try_apply(&img, &state)
            .expect("gpu apply should succeed for flip_h")
            .to_rgba8();
        assert_rgba_close(&cpu, &gpu, 0);
    }

    #[test]
    fn parity_flip_v() {
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
        state.flip_v = true;
        let cpu = crate::processing::transform::apply(&img, &state).to_rgba8();
        let gpu = try_apply(&img, &state)
            .expect("gpu apply should succeed for flip_v")
            .to_rgba8();
        assert_rgba_close(&cpu, &gpu, 0);
    }

    #[test]
    fn parity_crop() {
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
        state.crop = Some(Rect {
            x: 0.25,
            y: 0.25,
            width: 0.5,
            height: 0.5,
        });
        let cpu = crate::processing::transform::apply(&img, &state).to_rgba8();
        let gpu = try_apply(&img, &state)
            .expect("gpu apply should succeed for crop")
            .to_rgba8();
        assert_rgba_close(&cpu, &gpu, 0);
    }

    #[test]
    fn parity_straighten() {
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
        state.straighten = 5.0;
        let cpu = crate::processing::transform::apply(&img, &state).to_rgba8();
        let gpu = try_apply(&img, &state)
            .expect("gpu apply should succeed for straighten")
            .to_rgba8();
        // Skip boundary fill pixels; higher tolerance for bilinear edge differences
        assert_rgba_close_skip_fill(&cpu, &gpu, 12);
    }

    #[test]
    fn parity_keystone() {
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
        state.keystone.vertical = 0.1;
        let cpu = crate::processing::transform::apply(&img, &state).to_rgba8();
        let gpu = try_apply(&img, &state)
            .expect("gpu apply should succeed for keystone")
            .to_rgba8();
        // Skip boundary fill pixels; higher tolerance for perspective interpolation
        assert_rgba_close_skip_fill(&cpu, &gpu, 16);
    }

    #[test]
    fn parity_full_pipeline() {
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
        state.rotate = 90;
        state.flip_h = true;
        state.crop = Some(Rect {
            x: 0.1,
            y: 0.1,
            width: 0.8,
            height: 0.8,
        });
        state.exposure = 0.2;
        state.saturation = 0.1;
        state.sharpness = 0.5;
        let cpu = crate::processing::transform::apply(&img, &state).to_rgba8();
        let gpu = try_apply(&img, &state)
            .expect("gpu apply should succeed for full pipeline")
            .to_rgba8();
        assert_rgba_close_skip_fill(&cpu, &gpu, 16);
    }

    #[test]
    fn gpu_export_matches_cpu_output() {
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
        state.exposure = 0.3;
        state.contrast = 0.1;
        state.temperature = 0.2;
        let cpu = crate::processing::transform::apply(&img, &state).to_rgba8();
        let gpu = try_apply(&img, &state)
            .expect("gpu apply should succeed for export test")
            .to_rgba8();
        assert_rgba_close(&cpu, &gpu, 4);
    }

    #[test]
    fn gpu_export_handles_full_pipeline() {
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
        state.rotate = 90;
        state.flip_h = true;
        state.crop = Some(Rect {
            x: 0.1,
            y: 0.1,
            width: 0.8,
            height: 0.8,
        });
        state.exposure = 0.2;
        state.saturation = 0.1;
        state.sharpness = 0.5;
        let cpu = crate::processing::transform::apply(&img, &state).to_rgba8();
        let gpu = try_apply(&img, &state)
            .expect("gpu apply should succeed for full pipeline export")
            .to_rgba8();
        assert_rgba_close_skip_fill(&cpu, &gpu, 16);
    }

    fn assert_rgba_close(cpu: &image::RgbaImage, gpu: &image::RgbaImage, tolerance: u8) {
        assert_rgba_close_inner(cpu, gpu, tolerance, false);
    }

    /// Compare two images, optionally skipping boundary fill pixels where either
    /// RGB is (0,0,0) — these arise from different bilinear interpolation at edges.
    fn assert_rgba_close_skip_fill(cpu: &image::RgbaImage, gpu: &image::RgbaImage, tolerance: u8) {
        assert_rgba_close_inner(cpu, gpu, tolerance, true);
    }

    fn assert_rgba_close_inner(
        cpu: &image::RgbaImage,
        gpu: &image::RgbaImage,
        tolerance: u8,
        skip_fill: bool,
    ) {
        assert_eq!(cpu.dimensions(), gpu.dimensions());
        let mut skipped_fill = 0usize;
        let total_pixels = (cpu.width() as usize) * (cpu.height() as usize);
        for (c, g) in cpu.pixels().zip(gpu.pixels()) {
            if skip_fill {
                let c_black = c[0] == 0 && c[1] == 0 && c[2] == 0;
                let g_black = g[0] == 0 && g[1] == 0 && g[2] == 0;
                if c_black || g_black {
                    skipped_fill += 1;
                    continue;
                }
            }
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
        if skip_fill {
            let skipped_ratio = skipped_fill as f32 / total_pixels.max(1) as f32;
            assert!(
                skipped_ratio < 0.75,
                "too many skipped fill pixels ({:.1}%); possible black-output regression",
                skipped_ratio * 100.0
            );
        }
    }
}

mod app;
mod browser;
mod config;
mod editor;
mod metadata;
mod processing;
mod state;
mod thumbnail;
mod viewer;

use app::PhotographApp;
use config::AppConfig;
use viewer::PreviewBackend;

#[cfg(target_os = "macos")]
const WINDOW_ICON_PNG: &[u8] = include_bytes!("../assets/photograph-icon-macos-128.png");
#[cfg(not(target_os = "macos"))]
const WINDOW_ICON_PNG: &[u8] = include_bytes!("../assets/photograph-icon-128.png");

fn build_window_icon() -> egui::IconData {
    let icon = image::load_from_memory_with_format(WINDOW_ICON_PNG, image::ImageFormat::Png)
        .expect("embedded window icon should decode as PNG")
        .into_rgba8();
    let (width, height) = icon.dimensions();

    egui::IconData {
        rgba: icon.into_raw(),
        width,
        height,
    }
}

fn parse_preview_backend(value: &str) -> PreviewBackend {
    match value.trim().to_ascii_lowercase().as_str() {
        "cpu" => PreviewBackend::Cpu,
        "auto" => PreviewBackend::Auto,
        "gpu" | "gpu_pipeline" | "gpu_spike" | "spike" | "wgpu" => PreviewBackend::GpuPipeline,
        _ => PreviewBackend::Auto,
    }
}

fn resolve_preview_backend(config: &AppConfig) -> PreviewBackend {
    if let Ok(raw) = std::env::var("PHOTOGRAPH_PREVIEW_BACKEND") {
        return parse_preview_backend(&raw);
    }
    if let Some(raw) = config.preview_backend.as_deref() {
        return parse_preview_backend(raw);
    }
    PreviewBackend::Auto
}

fn effective_preview_backend(
    requested: PreviewBackend,
    allow_debug_cpu_fallback: bool,
) -> PreviewBackend {
    if requested == PreviewBackend::Cpu && !allow_debug_cpu_fallback {
        PreviewBackend::GpuPipeline
    } else {
        requested
    }
}

fn enforce_runtime_gpu_policy(allow_debug_cpu_fallback: bool) {
    if processing::gpu_pipeline::is_available() || allow_debug_cpu_fallback {
        return;
    }
    eprintln!(
        "photograph: no compatible discrete Vulkan GPU detected. Set {}=1 for debug CPU fallback.",
        processing::gpu_pipeline::DEBUG_ALLOW_CPU_FALLBACK_ENV
    );
    std::process::exit(2);
}

fn report_preview_backend(backend: PreviewBackend) {
    let status = processing::gpu_pipeline::runtime_status();
    let adapter_desc = match (
        status.adapter_name.as_deref(),
        status.adapter_backend.as_deref(),
    ) {
        (Some(name), Some(api)) => format!("{} ({})", name, api),
        (Some(name), None) => name.to_string(),
        _ => "n/a".to_string(),
    };
    match backend {
        PreviewBackend::Cpu => {
            eprintln!("photograph: preview backend = cpu (debug fallback mode)");
        }
        PreviewBackend::Auto => {
            if status.available {
                eprintln!(
                    "photograph: preview backend = auto (gpu_pipeline active on {})",
                    adapter_desc
                );
            } else {
                eprintln!(
                    "photograph: preview backend = auto (gpu unavailable; debug cpu fallback)"
                );
            }
        }
        PreviewBackend::GpuPipeline => {
            if status.available {
                eprintln!(
                    "photograph: preview backend = gpu_pipeline ({})",
                    adapter_desc
                );
            } else {
                eprintln!(
                    "photograph: preview backend = gpu_pipeline requested, but gpu unavailable"
                );
            }
        }
    }
}

fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = AppConfig::load();
    let requested_preview_backend = resolve_preview_backend(&config);
    let allow_debug_cpu_fallback = processing::gpu_pipeline::allow_debug_cpu_fallback();
    let preview_backend =
        effective_preview_backend(requested_preview_backend, allow_debug_cpu_fallback);
    if requested_preview_backend == PreviewBackend::Cpu && preview_backend != PreviewBackend::Cpu {
        eprintln!(
            "photograph: cpu backend requires {}=1; using gpu_pipeline policy",
            processing::gpu_pipeline::DEBUG_ALLOW_CPU_FALLBACK_ENV
        );
    }
    enforce_runtime_gpu_policy(allow_debug_cpu_fallback);
    report_preview_backend(preview_backend);

    let width = config.window_width.unwrap_or(1200.0);
    let height = config.window_height.unwrap_or(800.0);

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Photograph")
            .with_app_id("photograph")
            .with_icon(build_window_icon())
            .with_inner_size([width, height]),
        ..Default::default()
    };

    eframe::run_native(
        "photograph",
        native_options,
        Box::new(|cc| Ok(Box::new(PhotographApp::new(cc, config, preview_backend)))),
    )
}

#[cfg(test)]
mod tests {
    use super::{effective_preview_backend, parse_preview_backend};
    use crate::viewer::PreviewBackend;

    #[test]
    fn parse_preview_backend_handles_supported_values() {
        assert_eq!(parse_preview_backend("cpu"), PreviewBackend::Cpu);
        assert_eq!(parse_preview_backend("auto"), PreviewBackend::Auto);
        assert_eq!(parse_preview_backend("gpu"), PreviewBackend::GpuPipeline);
        assert_eq!(
            parse_preview_backend("gpu_pipeline"),
            PreviewBackend::GpuPipeline
        );
        assert_eq!(
            parse_preview_backend("gpu_spike"),
            PreviewBackend::GpuPipeline
        );
    }

    #[test]
    fn parse_preview_backend_defaults_to_auto_for_unknown_values() {
        assert_eq!(parse_preview_backend("unknown"), PreviewBackend::Auto);
    }

    #[test]
    fn cpu_backend_requires_debug_fallback_flag() {
        assert_eq!(
            effective_preview_backend(PreviewBackend::Cpu, false),
            PreviewBackend::GpuPipeline
        );
        assert_eq!(
            effective_preview_backend(PreviewBackend::Cpu, true),
            PreviewBackend::Cpu
        );
    }

    #[test]
    fn app_icon_buffer_matches_declared_dimensions() {
        let icon = super::build_window_icon();
        assert_eq!(icon.width, 128);
        assert_eq!(icon.height, 128);
        assert_eq!(icon.rgba.len(), (icon.width * icon.height * 4) as usize);
    }
}

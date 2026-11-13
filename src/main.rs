mod app;
mod browser;
mod config;
mod editor;
mod locations;
#[cfg(target_os = "macos")]
mod macos_open;
mod metadata;
mod network;
mod processing;
mod screenshot;
mod state;
mod thumbnail;
mod viewer;

use app::PhotographApp;
use config::AppConfig;
use viewer::PreviewBackend;

const WINDOW_ICON_PNG: &[u8] = include_bytes!("../assets/photograph-icon-128.png");

/// What Photograph was asked to open, from the command line (what a file
/// manager's Open With passes): the first argument that is a file or folder
/// that exists, made absolute. Anything else (flags, missing paths) is ignored.
fn start_from_args(args: impl Iterator<Item = std::ffi::OsString>) -> Option<std::path::PathBuf> {
    args.map(std::path::PathBuf::from)
        .find(|p| p.exists())
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
}

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
        "photograph: no compatible GPU detected. Set {}=1 for debug CPU fallback.",
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

    #[cfg(target_os = "macos")]
    macos_open::install();
    let start = start_from_args(std::env::args_os().skip(1));
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
        // Photograph draws its own frame unless asked not to (ADR-0022), with
        // Patina's frame (ADR-0025).
        viewport: patina::frame::viewport(
            egui::ViewportBuilder::default()
                .with_title("Photograph")
                .with_app_id("photograph")
                .with_icon(build_window_icon())
                .with_inner_size([width, height]),
            !config.system_title_bar,
        ),
        ..Default::default()
    };

    eframe::run_native(
        "photograph",
        native_options,
        Box::new(|cc| Ok(Box::new(PhotographApp::new(cc, config, preview_backend, start)))),
    )
}

#[cfg(test)]
mod tests {
    use super::{effective_preview_backend, parse_preview_backend, start_from_args};
    use crate::viewer::PreviewBackend;

    #[test]
    fn the_start_is_the_first_argument_that_exists() {
        let dir = std::env::temp_dir();
        let args = ["--flag".into(), "/no/such/file.nef".into(), dir.clone().into_os_string()];
        assert_eq!(start_from_args(args.into_iter()), std::fs::canonicalize(&dir).ok());
        assert_eq!(start_from_args(std::iter::empty()), None);
    }

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

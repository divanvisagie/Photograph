# Photograph

<img src="packaging/linux/photograph.svg" alt="Photograph logo" width="180" />

Native Rust desktop photo browser/editor for image management and color grading.

Photograph focuses on practical desktop workflows: browsing folders, opening images, applying non-destructive edits, and exporting rendered files.

## Status

Active project (MVP is usable and still evolving).

## Screenshot

![Photograph screenshot](docs/photograph-ui.png)

## What It Does Today

- Folder browser with thumbnail grid
- Full image viewer/editor windows (egui/eframe)
- EXIF metadata display
- Non-destructive edits stored as sidecar JSON (`<image>.json`)
- Geometry edits: rotate, flip, crop, straighten, keystone
- Color/tone edits: exposure, white balance, HSL, selective color, graduated filter, highlight/shadow recovery
- Export rendered images as `JPG`, `PNG`, or `WebP` with quality/compression and optional resize
- Background rendering/export progress UI

## Supported Formats

- RAW decode via `rawler`: `RAF`, `DNG`, `NEF`, `CR2`, `ARW`
- Standard image formats via `image` crate fast path (for example `JPG`, `PNG`, `TIFF`, `WebP`, `BMP`)
- The browser also recognizes `HEIC` and `AVIF` extensions, but actual decode support depends on the image stack available in the current build

## Quick Start

```bash
cargo run --bin photograph
```

The app opens a native window and remembers UI state/config between runs.

## Development

Build, run, and test:

```bash
cargo build
cargo run --bin photograph
cargo test
```

Live-reload dev loop (requires `cargo-watch`):

```bash
cargo install cargo-watch
make dev
```

## Configuration

Photograph stores config at:

- Linux: `~/.config/photograph/config.toml`
- macOS: `~/Library/Application Support/photograph/config.toml` (via `dirs::config_dir()`)

Current persisted settings include window sizes/positions, last browsed path, and preview backend preference.

Example:

```toml
browse_path = "/path/to/photos"
preview_backend = "auto" # auto | cpu | gpu | gpu_spike | wgpu
```

You can also override preview backend at runtime:

```bash
PHOTOGRAPH_PREVIEW_BACKEND=cpu cargo run --bin photograph
```

## Packaging

The `Makefile` supports Linux (`.deb`) and macOS (`.app`, optional `.dmg`) packaging.

Icon assets are derived from the SVG source at `packaging/linux/photograph.svg`:

```bash
make icons
```

This regenerates the embedded runtime PNG (`assets/photograph-icon-128.png`) and, on macOS, the bundle icon (`packaging/macos/photograph.icns`).

Common targets:

```bash
make build          # platform-aware packaging build
make install        # platform-aware install
make build-linux    # build .deb on Linux
make build-macos    # build .app on macOS
make package-macos  # build .dmg on macOS
```

Linux packaging assets live under `packaging/linux/`.
macOS bundle metadata lives under `packaging/macos/`.

## Performance Probe

There is a CLI benchmark helper for raw preview/export throughput:

```bash
cargo run --bin perf_probe -- /path/to/raws [count] [auto|cpu|gpu_spike]
```

It prints `METRIC ...` lines for preview latency and export throughput.

## Project Layout

- `src/` application code (`browser`, `viewer`, `editor`, processing pipeline)
- `src/bin/perf_probe.rs` benchmark helper
- `assets/` embedded app assets (including icon)
- `packaging/` Linux/macOS packaging files
- `Makefile` dev/build/install/package commands

## License

GPL-2.0-only. See `LICENSE`.

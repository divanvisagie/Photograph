# 0011. Restore macOS support via a Metal GPU backend

Date: 2026-03-29

## Status

Superseded by [ADR-0012](0012-drop-macos-support-linux-only.md)

## Context

macOS support, dropped in [ADR-0008](0008-drop-macos-packaging.md), was needed again. Beyond
restoring the Makefile packaging targets, the GPU pipeline itself was a blocker: adapter selection
was hardcoded to `wgpu::Backends::VULKAN`, and Vulkan does not exist as a native backend on macOS
(it isn't Apple's supported GPU API — Metal is). [ADR-0003](0003-require-vulkan-gpu.md)'s adapter
policy was written Linux-only and never accounted for a second native backend.

## Decision

We will select the GPU backend per platform at compile time: `wgpu::Backends::METAL` (filtering
adapters to `wgpu::Backend::Metal`) on macOS, `wgpu::Backends::VULKAN` (filtering to
`wgpu::Backend::Vulkan`) everywhere else, via a `NATIVE_BACKEND`/`NATIVE_BACKEND_FILTER`
`cfg(target_os = "macos")` pair. Adapter selection logic itself (prefer discrete, then integrated,
reject CPU) is unchanged — only the backend/filter constants become platform-conditional. Restore
the macOS Makefile targets (`build-macos`, `package-macos`, `install-macos`, `icon-macos`) dropped
in ADR-0008.

## Consequences

- macOS gets a working GPU pipeline again, on the API Apple actually supports, rather than one
  that can never find a Vulkan adapter.
- The adapter-selection and device/pipeline-creation code (`init_gpu_context` and everything
  downstream) is now genuinely cross-platform: the same `wgpu`-mediated code path runs against two
  different native graphics APIs depending on target OS, rather than being written and reasoned
  about as Linux/Vulkan-only.
- Any future GPU-pipeline change has to keep working under both Vulkan and Metal semantics via
  `wgpu`'s abstraction — it can no longer assume Vulkan-specific behavior or extensions are
  available, since Metal must also satisfy the same code path.
- The macOS packaging maintenance cost that motivated ADR-0008 returns.

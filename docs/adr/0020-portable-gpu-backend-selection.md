# 0020. Select each platform's primary GPU backend instead of Vulkan only

Date: 2026-09-27

## Status

Accepted

## Context

[ADR-0003](0003-require-vulkan-gpu.md) initialized `wgpu` with a Vulkan-only backend, and
[ADR-0012](0012-drop-macos-support-linux-only.md) collapsed the Metal branch from
[ADR-0011](0011-restore-macos-metal-backend.md) back to that single backend. ADR-0012's technical
argument was that staying within what both Vulkan and Metal can do caps the pipeline, and that going
Vulkan-only would unblock Vulkan-specific work (subgroup ops, queue overlap, a pipelined async apply
path). It said explicitly that none of that was being built.

None of it has been built since. The pipeline is still fully portable `wgpu`: WGSL shaders,
`request_device` with `Features::empty()` and `Limits::default()`, one queue. The only thing that
Vulkan-only actually does is the backend constant and the adapter filter in `gpu_pipeline.rs`, and
all they do is refuse adapters on platforms without Vulkan. macOS has no native Vulkan, and on
Windows `wgpu` would otherwise use DX12.

Publishing to crates.io made this matter: `cargo install photograph` builds fine on macOS and then
exits at startup with "no compatible GPU detected" on hardware that could run the pipeline.

Options considered:

- **Keep Vulkan-only.** Holds a position that nothing uses, and costs anyone off Linux a working app.
- **Restore a per-platform `cfg` branch (as in ADR-0011).** Works, but brings back code we'd be
  maintaining for platforms we don't support.
- **Use `wgpu::Backends::PRIMARY` and drop the adapter backend filter.** `wgpu` picks each platform's
  primary native API (Vulkan, Metal, DX12) with no platform-specific code of ours.

## Decision

We will initialize `wgpu` with `wgpu::Backends::PRIMARY` and select adapters by device type alone
(prefer discrete, then integrated, reject CPU), with no backend filter. The supported-platform
position from ADR-0012 is unchanged: Linux is the only platform we package, test or document. Other
platforms are allowed to run, not supported.

## Consequences

- Linux behavior is unchanged: `PRIMARY` resolves to Vulkan there, and the app still reports
  `gpu_pipeline active on … (vulkan)`.
- macOS and Windows builds (e.g. via `cargo install`) are no longer blocked by our code. Whether they
  work end to end is untested. Some features, such as mount discovery for the sidebar
  ([ADR-0017](0017-discover-mounts-for-sidebar.md)), are Linux-specific and will show less there.
- The pipeline is once again held to what `wgpu` can do on every primary backend, which ADR-0012
  wanted to get away from. In practice that's no change, because it never left that set. When work
  arrives that genuinely needs a Vulkan-only feature, that's the point to revisit: request the
  feature and fall back or restrict backends then, rather than paying for the restriction up front.
- The debug-only CPU fallback ([ADR-0004](0004-cpu-fallback-debug-only.md)) and the exit on no GPU
  are unchanged. The only change is that a GPU no longer has to speak Vulkan to count.

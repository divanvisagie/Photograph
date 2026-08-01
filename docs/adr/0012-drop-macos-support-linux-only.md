# 0012. Drop macOS support; Photograph is Linux-only

Date: 2026-08-01

## Status

Accepted

## Context

macOS support has flip-flopped twice already ([ADR-0001](0001-platform-packaging-linux-and-macos.md)
added it, [ADR-0008](0008-drop-macos-packaging.md) dropped it, [ADR-0011](0011-restore-macos-metal-backend.md)
restored it via a Metal GPU backend), each time driven by demand rather than a settled position on
whether macOS is worth the ongoing cost. Revisiting it now:

- **Who actually needs it.** macOS users doing this kind of RAW browsing/develop/export workflow
  are already well served by the platform's own Photos app (and other native RAW editors) for the
  functionality Photograph targets. There isn't a gap on macOS that Photograph is uniquely filling,
  the way there arguably is on Linux.
- **What supporting it costs technically**, beyond packaging maintenance: the GPU pipeline is
  mediated through `wgpu` specifically so the same code can target both Vulkan and Metal
  ([ADR-0011](0011-restore-macos-metal-backend.md)). That abstraction has a real, non-hypothetical
  floor — `request_device` asks for `wgpu::Features::empty()` (the portable core only, ruling out
  subgroup intrinsics and timestamp queries that either platform could otherwise use), the pipeline
  is limited to `wgpu`'s single-queue model (no separate async-compute/transfer queue overlap that
  Vulkan exposes natively), and the per-image apply path (`gpu_pipeline.rs`) is a fully synchronous
  submit → block-wait → map → copy → unmap round trip per call, partly *because* a properly
  pipelined, multi-frame-in-flight design is harder to get right portably across two native GPU
  APIs than to write against one directly. None of this is about Vulkan vs. Metal being better or
  worse — it's the cost of the abstraction that makes "either backend" possible at all.
- Options considered: keep both platforms and accept the abstraction ceiling as the price of macOS
  reach; keep both platforms and let Linux and macOS diverge onto separate native backends
  (Vulkan-direct and Metal-direct) to remove the ceiling, at the cost of maintaining two GPU
  pipelines instead of one; or drop macOS and let the Linux/Vulkan path stop being constrained by
  what Metal can also do. Given the Photos-app point above, there's no real user-facing reason to
  pay either of the first two costs.

## Decision

We will drop macOS support entirely: remove the macOS Makefile targets (`build-macos`,
`package-macos`, `install-macos`, `icon-macos`, `publish-macos`), the `packaging/macos/` assets, the
macOS-specific window icon, and the `NATIVE_BACKEND`/`NATIVE_BACKEND_FILTER` platform branching in
`gpu_pipeline.rs` added by [ADR-0011](0011-restore-macos-metal-backend.md) — collapsing back to a
single Vulkan-only backend as originally described in [ADR-0003](0003-require-vulkan-gpu.md).
Linux (`.deb` and snap, per [ADR-0008](0008-drop-macos-packaging.md) and
[ADR-0010](0010-snap-packaging-vulkan-gpu-2404.md)) becomes the only supported target.

## Consequences

- Removes the macOS packaging maintenance burden for good this time, rather than leaving it
  dormant to be requested back a third time.
- The GPU pipeline no longer has to stay within the intersection of what Vulkan and Metal can both
  do — future work (subgroup ops, explicit queue overlap, a genuinely pipelined async apply path)
  can target Vulkan directly without a Metal compatibility ceiling, though none of that is being
  built as part of this decision; it's what this unblocks, not what it delivers.
- This is a real, deliberate loss of reach: anyone relying on the restored macOS build
  ([ADR-0011](0011-restore-macos-metal-backend.md)) loses it, with the native Photos app (or
  another native RAW editor) as the suggested alternative rather than a Photograph migration path.
- If macOS demand returns a third time, the honest options are the same two rejected above:
  accept the abstraction ceiling again, or maintain a second, Metal-direct pipeline. Simply
  restoring the `wgpu` Metal branch a third time without addressing why it keeps getting cut would
  just repeat this cycle.

# 1. Require Vulkan GPU (Prefer Discrete, Allow Integrated)

Date: 2026-02-28

## Status

Accepted

## Context

The project is optimized for Linux Vulkan execution, but integrated-GPU-only systems
are still valid runtime targets.

## Decision

Initialize [`wgpu`](https://wgpu.rs/) with a Vulkan-only backend and select the best
non-CPU adapter, preferring a discrete GPU when available.

## Consequences

Keeps predictable GPU execution while avoiding unnecessary startup rejection on
integrated-only systems.

Implemented in `src/processing/gpu_pipeline.rs` (`init_gpu_context`).

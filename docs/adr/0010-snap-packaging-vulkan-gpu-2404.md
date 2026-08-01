# 0010. Package for Linux via snap, using the gpu-2404 content interface for Vulkan

Date: 2026-03-01

## Status

Accepted

## Context

A `.deb` covers Debian/Ubuntu users willing to `apt install` a locally-built package, but doesn't
reach the Snap Store distribution channel or give sandboxed confinement. Packaging a Vulkan
GPU-dependent app under strict confinement raises a specific problem: the snap sandbox can't see
the host's Vulkan ICD (driver loader) files by default, and manually wiring ICD search paths per
distro/driver combination is fragile and a maintenance burden of its own.

Options considered:

- **Classic (unconfined) snap.** Would sidestep the ICD visibility problem entirely, but gives up
  sandboxing and is subject to tighter Snap Store review requirements. Rejected — defeats the
  point of using snap's confinement model.
- **Manual ICD path wiring** (bind-mounting or environment-variable-pointing at host Vulkan ICD
  JSON files from inside the strict sandbox). Rejected — fragile across distros/driver versions,
  and exactly the kind of glue code a content interface exists to avoid.
- **`gpu-2404` content interface** (backed by the `mesa-2404` provider snap), which supplies Vulkan
  ICD discovery and driver management to consuming snaps without the snap author needing to wire
  ICD paths manually. Chosen — see Decision.

## Decision

We will package Photograph as a strictly-confined snap (`core24` base) and consume the `gpu-2404`
content interface for Vulkan GPU access, alongside `opengl`, `desktop`, `wayland`, `x11`, `home`,
and `removable-media` plugs for the rest of the app's I/O needs.

## Consequences

- Reaches Snap Store distribution and sandboxed installs without hand-rolling Vulkan ICD discovery.
- Ties Vulkan availability inside the snap to the `gpu-2404`/`mesa-2404` provider snap being
  installed and kept current on the user's system — a dependency outside this project's direct
  control.
- Strict confinement means any future I/O the app needs has to go through an explicit plug, rather
  than assuming unrestricted host access like the `.deb` build has.

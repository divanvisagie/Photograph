# 0024. Ship a macOS build again, with every release built on GitHub from one `make release`

Date: 2026-10-10

## Status

Accepted. The user asked for UnAmp's release process to be copied, so that the macOS and `.deb`
releases are done in one go from the Makefile. That process is UnAmp's
[ADR-0025](https://github.com/divanvisagie/UnAmp/blob/master/docs/adr/0025-release-from-a-tag-with-ci-packages.md).

## Context

[ADR-0012](0012-drop-macos-support-linux-only.md) made Photograph Linux-only. Its argument was the
cost of the `wgpu` abstraction: keeping a Metal backend capped the GPU pipeline at what both Vulkan
and Metal can do. [ADR-0020](0020-portable-gpu-backend-selection.md) has since made `wgpu` pick each
platform's primary backend, so the pipeline already runs on Metal with no platform code of ours;
`cargo install photograph` builds on macOS today. The ceiling ADR-0012 described is the one the
pipeline already lives under.

`make release` built the `.deb` on the user's machine and uploaded it with the GitHub release, after
a separate `make bump` had set the version and the website's download links. A Mac build can't come
from a Linux machine. UnAmp, which shares Photograph's window frame, appearance and mount code,
already releases both packages from GitHub runners.

Photograph's Linux-only parts fail gracefully on macOS:
- mount discovery reads `/proc/self/mounts` and the gvfs directory, and finds nothing;
- the light/dark watcher needs a session D-Bus. It only supplies egui's fallback theme, and the
  window system's own theme is used where it's available.

The crate type-checks for `aarch64-apple-darwin`.

## Decision

We will ship a macOS `.dmg` alongside the `.deb` again, and release both the way UnAmp does:

- **One local target, `make release`:** fast-forward `master`; check it's clean, in sync and the tag
  is unused; bump the version (patch by default, `BUMP=minor|major`, or `V=x.y.z`, which may be the
  current never-tagged version); dry-run `cargo publish`; tag and push `master` and the tag; open a
  **draft** GitHub release (with any `NOTES=` file above the generated changelog); then
  `cargo publish`, last, as it can't be undone, followed by `cargo clean -p photograph` so
  `cargo run` doesn't keep running the published build.
- **The workflow (`.github/workflows/release.yml`) runs on the tag:**
  - **deb:** `make build-deb` on Ubuntu 22.04, the oldest supported runner, for a low glibc floor,
    plus an unversioned `photograph_amd64.deb` copy;
  - **dmg:** `make dmg` on macOS 14 (`packaging/macos/build-dmg.sh`): a universal Apple Silicon +
    Intel `Photograph.app`, icon rendered from the SVG, ad-hoc signed, on a drag-to-Applications
    disk image as `Photograph-<version>.dmg` plus a `Photograph.dmg` copy;
  - **publish:** attach both to the draft and publish it as latest;
  - **site:** run `scripts/update-site-links.sh` on `master` to move the website's download links to
    the new files, commit as `github-actions[bot]`, push, and request a Pages build.
- Running the workflow by hand builds both packages as artifacts without releasing anything.
- `make bump` is removed: the bump is part of `make release`, the link update part of the workflow.
- **macOS is a release build only.** Photograph is still developed and used on Linux. The `.dmg`
  isn't notarised; the website and README say how to allow it, and that the sidebar's drives and
  shares are Linux-only.

## Consequences

- A release is one command and a wait. It only becomes public, and the website only moves to it,
  once both packages are attached.
- Packages are built the same way every time, on clean runners.
- The crate is published before the packages are built. A failed package build leaves crates.io
  ahead of GitHub until the workflow is fixed and re-run on the tag.
- The site update is a bot commit on GitHub's `master`; pull after releasing.
- The macOS build isn't tried on a Mac before release; the runner compiling and packaging it is the
  only check. Unsigned, every user has to allow it by hand.
- macOS support has now flipped four times (ADR-0001, 0008, 0011, 0012). What's different this
  time is the cost: no platform code, no packaging run by hand. Should the pipeline ever need
  Vulkan-only features that ADR-0012 wanted room for, that is the point to drop the `.dmg` again
  rather than hold the Linux pipeline back.
- This supersedes ADR-0012.

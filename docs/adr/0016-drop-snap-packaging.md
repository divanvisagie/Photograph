# 0016. Drop Snap packaging; ship Linux .deb only

Date: 2026-09-26

Note: This ADR supersedes [ADR-0010](0010-snap-packaging-vulkan-gpu-2404.md).

## Status

Accepted

## Context

Photograph is a photo browser, and users need to browse photos wherever they're stored. That
includes ZFS pools mounted at the filesystem root (e.g. `/tank`), custom `/etc/fstab` mount points,
and network shares mounted outside the standard directories.

The Snap build uses `confinement: strict`. Under strict confinement, the only filesystem locations
the app can reach are:

- `$HOME` (non-hidden files, via `home`), and
- `/media`, `/mnt` and `/run/media` (via `removable-media`). Store snaps don't get this
  interface auto-connected, so the user has to run `snap connect` manually.

Anything else is blocked by AppArmor, and symlinks don't help because AppArmor checks the resolved
path. For example, the Snap build could not see a ZFS pool at `/tank` on the development machine.
This is the same class of limitation that drove [ADR-0013](0013-network-mounts-deb-only.md) and
[ADR-0015](0015-drop-specialized-network-mounts.md).

Options considered and rejected:

- **Classic confinement.** This would give unrestricted filesystem access, but the Snap Store
  grants classic only after manual review, and mostly to dev tools such as IDEs and compilers. A
  photo browser is unlikely to qualify.
- **`system-files` plug for specific paths.** This needs manual store approval, and the paths are
  machine-specific (`/tank` is one user's pool name), so it can't cover "anywhere".
- **XDG desktop portal (FileChooser + document portal).** This is the sanctioned way for a
  confined app to reach arbitrary user-chosen folders, and it works for any location the host
  can read. It was rejected because:
  - The app would have to move from free filesystem browsing to a model where the user adds
    "library roots" one at a time.
  - Paths get remapped under `/run/user/$UID/doc/…`, which affects sidecars and any stored paths.
  - All reads go through a FUSE layer, which is slow for thumbnailing large RAW folders.
  - inotify doesn't work through that FUSE layer.
- **Keep the Snap as a restricted variant alongside the `.deb`.** This was rejected because two
  install paths that behave differently are confusing, and the snap-specific code and docs keep
  costing maintenance.

The snap was only ever published to `latest/edge` (pre-release), so there were no stable-channel
users to migrate.

## Decision

We will stop distributing Photograph as a Snap. Linux distribution is the `.deb` only, built by
`make build-deb` and published on GitHub Releases by `make release`.

As part of this decision:

- The `latest/edge` channel was closed in the Snap Store on 2026-09-26. The `photograph` snap name
  is still registered to the project.
- The `snap/` directory (`snapcraft.yaml`, the desktop launcher wrapper and the GUI assets) is
  removed.
- The Makefile's snap targets are removed: `snap`, `snap-install`, `snap-publish` and
  `snap-screenshot`.
- The Snap-specific `removable-media` hint in the browser's permission-denied error is removed.
- The README and the landing page point to the `.deb` on GitHub Releases instead of the Snap Store.

## Consequences

Benefits:

- The app can browse any location the user's account can read, including root-level ZFS pools,
  custom mounts and network shares, without extra user steps.
- One install path, one behavior. There's no Snap-vs-`.deb` feature matrix to document, and no
  confinement workarounds (`gpu-2404` content interface, `desktop-launch` socket bridging) to
  maintain.
- [ADR-0015](0015-drop-specialized-network-mounts.md)'s "mount it at the OS level" guidance now
  holds without the caveat that Snap users must mount under `/mnt` or `/media`.

Costs:

- Photograph no longer appears in the Ubuntu App Center, which is built around the Snap Store.
  Discovery depends on the README, the landing page and word of mouth.
- Users don't get automatic updates. Installing a downloaded `.deb` from GitHub Releases doesn't
  add an update source.
- There's no sandbox. The app runs with the user's full permissions.

Possible future steps (not decided):

- An APT repository, either self-hosted (e.g. on GitHub Pages and fed by `make release`) or a
  Launchpad PPA, would restore `apt upgrade` updates. Neither would make the app discoverable in
  the App Center.
- Flathub allows `--filesystem=host` with a justification, as darktable and digiKam use. It's the
  realistic store route if storefront discovery becomes important.

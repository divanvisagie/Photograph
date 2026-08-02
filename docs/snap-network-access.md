# Snap Packaging: Network Mount Access

The Browser sidebar's "NETWORK" section (active SMB/NFS/GVfs shares) is
**not available in the Snap build**. It's gated behind the `network-mounts`
Cargo feature, which the Snap part disables at build time
(`rust-no-default-features: true` in `snap/snapcraft.yaml`). See
[ADR-0013](adr/0013-network-mounts-deb-only.md) for why, and use the `.deb`
if you need this feature.

## Why This Exists

`Browser::scan_network_locations` (`src/browser.rs`) covers two unrelated
things at the OS level:

- **Classic NFS/CIFS/sshfs mounts** under `/mnt`, `/media`, `/run/media` —
  readable under strict confinement given `removable-media` plus
  `mount-observe` (needed just to read `/proc/mounts` at all).
- **GVfs mounts** (`/run/user/$UID/gvfs`) — GNOME's virtual filesystem, where
  Files/Nautilus mounts `smb://`/`sftp://`/`ftp://` shares. There's no
  interface that grants generic access to another process's per-user FUSE
  mount; the closest fit, `system-files`, is a "privileged interface" that
  the Snap Store's automated review rejects until a human reviewer approves
  it for this specific snap, via
  [forum.snapcraft.io → Store Requests → Privileged Interfaces](https://forum.snapcraft.io/c/store-requests/19)
  — a ~1 week vote among reviewers — and `system-files` paths are literal
  (`/run/user/<uid>/gvfs`), not wildcards, so there's no single path that
  works across every installer's UID anyway.

Since GVfs is the common case (any share connected through Files/Nautilus),
shipping classic-mounts-only in the Snap would silently work for some shares
and not others depending on how they were mounted — worse than not having
the feature at all. [ADR-0013](adr/0013-network-mounts-deb-only.md) opts the
Snap build out of the feature entirely instead of chasing store review for
`system-files`.

## Alternative (No Review Needed)

Routing folder browsing through `xdg-desktop-portal`'s file-chooser portal
would see GVfs locations without any special interface, since portals run
outside the sandbox. That's a real feature-level change to how the browser
picks folders, not a packaging tweak — worth reconsidering if Snap network
support becomes a priority again.

## Relevant Files

- `Cargo.toml` (`network-mounts` feature)
- `snap/snapcraft.yaml` (`rust-no-default-features: true`)
- `src/browser.rs` (`scan_network_locations`, `friendly_gvfs_label`,
  `unescape_mount_field`)

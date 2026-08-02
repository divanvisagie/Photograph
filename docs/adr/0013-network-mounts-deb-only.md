# 0013. Network drive browsing is .deb-only; the Snap build disables it

Date: 2026-08-02

## Status

Accepted

## Context

The Browser sidebar's "NETWORK" section (`Browser::scan_network_locations` in `src/browser.rs`)
surfaces active network shares from two unrelated OS-level sources:

- Classic NFS/CIFS/sshfs mounts under `/mnt`, `/media`, `/run/media`.
- GVfs mounts under `/run/user/$UID/gvfs` — where GNOME's Files/Nautilus actually puts
  `smb://`/`sftp://`/`ftp://` connections, which is how most users connect to a network share in
  the first place.

Under the Snap's `confinement: strict`, the classic mounts need only `mount-observe` (to read
`/proc/mounts`) alongside the already-plugged `removable-media` — both ordinary "manual connect"
interfaces. GVfs access is the hard part: there's no interface that grants generic access to
another process's per-user FUSE mount. The closest fit, `system-files`, is a "privileged
interface" — the Snap Store's automated review rejects any upload that plugs it until a human
reviewer approves that specific snap for that specific interface via a Store Requests forum
thread, a roughly one-week manual vote. Even after approval, `system-files` paths are literal
strings, not wildcards, so `/run/user/<uid>/gvfs` has to be hardcoded to one UID — there's no
single snap build that works across every installer's UID.

Shipping only the classic-mounts half in the Snap (dropping `system-files`/GVfs) was considered
and rejected: GVfs is the common case, so a Snap build that finds `/mnt` shares but not
Nautilus-connected `smb://` shares would work inconsistently depending on how a user happened to
mount things — worse than not offering the feature at all, since it looks like a bug rather than
an absence.

Also considered: routing folder browsing through `xdg-desktop-portal`'s file-chooser portal, which
sees GVfs locations without any special interface because portals run outside the sandbox. Rejected
for now because it's a real feature-level change to how the browser picks folders, not a packaging
tweak — worth reconsidering if Snap network support becomes a priority again.

## Decision

We will gate `scan_network_locations` and its network-mount UI behind a new Cargo feature,
`network-mounts` (on by default). The Snap build (`snap/snapcraft.yaml`) sets
`rust-no-default-features: true` on the `photograph` part, compiling it out entirely, and drops the
`mount-observe` and `gvfs-access` (`system-files`) plugs it would otherwise need. Network drive
browsing is now `.deb`-only: anyone who needs it installs the `.deb` instead of the Snap.

## Consequences

- No privileged-interface Store review to request, wait on, or maintain (`system-files` is gone
  from `snap/snapcraft.yaml` entirely) — removes an ongoing approval/portability liability for a
  path that was hardcoded to one dev machine's UID.
- Snap users lose network-share browsing outright; the sidebar's "NETWORK" section simply won't
  appear in that build. This is a real capability gap between the two packaging formats, not a
  cosmetic difference — anyone relying on it must switch to the `.deb`.
- `docs/snap-network-access.md` and the ADR-0010 Snap packaging docs now need to be read alongside
  this decision to understand the Snap's feature set.
- If Snap network support is revisited, the `xdg-desktop-portal` file-chooser route is the more
  promising path — it sidesteps the privileged-interface problem entirely — but it's a browser
  feature change, not a flag flip, and hasn't been scoped.

# 0017. Discover storage and network mounts for the sidebar

Date: 2026-09-26

Note: This ADR supersedes [ADR-0015](0015-drop-specialized-network-mounts.md).

## Status

Accepted

## Context

[ADR-0015](0015-drop-specialized-network-mounts.md) removed network-share detection. The main
reason was that the Snap build couldn't support it: the `.deb` and the Snap had to behave
differently, and that had to be maintained behind a Cargo feature. What remained was a sidebar that
listed only Home and the entries under `/media/$USER`, `/mnt` and `/run/media/$USER`, which
assumed every mount lives in those directories.

[ADR-0016](0016-drop-snap-packaging.md) dropped the Snap, so the constraint behind ADR-0015 is
gone. The directory-based sidebar also missed real photo storage. A ZFS pool at `/tank` never
appeared, custom `/etc/fstab` mount points never appeared, and neither did GVfs shares opened from
GNOME Files (`/run/user/$UID/gvfs`).

## Decision

We will build the sidebar from what is actually mounted, in a `locations` module:

- **LOCATIONS:** Home and Computer (`/`).
- **STORAGE:** mounts from `/proc/self/mounts` whose filesystem type is on an allowlist of
  data-holding filesystems (ext4, xfs, btrfs, zfs, vfat, exfat, ntfs, …).
  - Excluded: the root filesystem, OS-owned trees (`/boot`, `/var`, `/usr`, `/snap`, `/run`, …),
    and the partition containing the user's home directory.
  - Always kept: mounts inside the home directory, `/media` and `/run/media`.
  - Nested mounts collapse into their top-most ancestor. For example, ZFS child datasets are
    reached by browsing into the pool, rather than getting their own sidebar entries.
- **NETWORK:** NFS, CIFS, sshfs, rclone, WebDAV and similar mounts, found through the same
  `/proc/self/mounts` filtering, plus GVfs connections with readable labels ("photos on nas").

The path bar also accepts `~`, and it accepts a path to a photo, which opens that photo's folder
with the photo focused. An invalid path shows an error and keeps the typed text so it can be
corrected, instead of being silently reverted.

## Consequences

Benefits:

- Pools, custom mounts and network shares show up without the user having to relocate or re-mount
  anything. ADR-0015's advice to "mount it under `/mnt` or `/media`" is no longer needed.
- Discovery never stats the paths it finds, only `/proc/self/mounts` and one `read_dir` of the GVfs
  directory. A hung network mount therefore can't freeze the sidebar. Browsing *into* a hung
  mount can still block, because directory scanning runs on the UI thread.

Costs:

- The filesystem-type allowlist and the system-prefix list are heuristics. An unusual filesystem
  type won't be listed, and a genuine data mount under an excluded prefix such as `/srv` won't be
  either. Both can still be reached through Computer or the path bar.
- Mount discovery is Linux-specific (`/proc/self/mounts`), which is consistent with
  [ADR-0012](0012-drop-macos-support-linux-only.md).
- The location list refreshes on every navigation, not live, so a newly plugged-in drive appears
  after the next folder change.

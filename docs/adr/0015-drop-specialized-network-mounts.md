# 0015. Drop specialized network-mount detection in favor of local mounts

Date: 2026-09-04
Note: This ADR supersedes [ADR-0013](0013-network-mounts-deb-only.md).

## Status

Accepted

## Context

Currently, we have a way to detect and browse network shares (NFS, CIFS, etc.) by looking at common mount points like `/mnt` or the GVfs folder `/run/user/$UID/gvfs`. 

Because Snap packages are strictly confined, they cannot access `system-files` (like those used by GVfs) without special permissions that we do not want to request. To work around this, we implemented a Cargo feature `network-mounts` which is enabled for `.deb` builds and disabled for Snap builds.

This "platform branching" introduces complexity:
1. We have to maintain separate logic paths or feature gates in the codebase.
2. The user experience is inconsistent; users on Snaps don't see a "Network" section, while .deb users do.
3. It violates the Unix philosophy of doing one thing well.

## Decision

We will remove all specialized logic for identifying network mounts and the corresponding `network-mounts` Cargo feature. 

The application will treat all paths as local files. If a user wants to access a network share, they are responsible for mounting it at the OS level (e.g., via `/etc/fstab`, a desktop environment's mount tool, or and manual mount command). Once mounted by the OS, these locations appear in standard directories like `/mnt` or `/media` and will be accessible to both `.deb` and Snap versions without special branching.

## Consequences

- **Simplification**: The `network-mounts` Cargo feature is removed.
- **Consistency**: The UI now looks identical on all platforms, as the "Network" section in the sidebar will be removed (since standard mounting is sufficient).
- **Maintenance**: We no longer need to maintain special logic or account for Snap's `system-files` limitations in our code.
- **User Experience**: Users requiring network share access must perform a local mount, which is standard behavior in Linux environments.

## Supersedes
[ADR-0013](0013-network-mounts-deb-only.md)

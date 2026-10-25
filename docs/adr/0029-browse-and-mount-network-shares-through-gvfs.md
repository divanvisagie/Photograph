# 0029. Browse and mount network shares through GVfs, as Files does

Date: 2026-10-25

## Status

Accepted. The user asked for Photograph to browse and mount network drives "in the same way"
Sector F's Files can. Supersedes [ADR-0017](0017-discover-mounts-for-sidebar.md)'s GVfs discovery
(a `read_dir` of `$XDG_RUNTIME_DIR/gvfs`); its storage and classic network mounts stand.

## Context

[ADR-0017](0017-discover-mounts-for-sidebar.md) listed network shares that were already mounted:
NFS, CIFS and sshfs from `/proc/self/mounts`, and GVfs shares by reading the FUSE bridge's
directory. Getting to a share that wasn't mounted meant opening Nautilus or Files first.

Files mounts shares itself through GVfs (its ADR-0005): `network:///` lists the servers DNS-SD and
SMB browsing find, `smb://host/` lists a server's shares, mounting one asks for a password through
a `GMountOperation` (which can save it in the keyring), and every mount appears as a folder under
`/run/user/<uid>/gvfs`, where the rest of the app works on it unchanged. Files' ADR-0005 also ruled
out `mount.cifs` (needs root and doesn't browse), libsmbclient (mounts no other app can see, and a
second code path for every file operation) and the `gio` command line (passwords on stdin, no
keyring). The same reasons hold for Photograph, which reads photos only by path.

## Decision

We will take Files' GVfs module into Photograph (`src/network.rs`), on a thread of its own running
a GLib main loop:

- **The sidebar's Network section** starts with a Network place, listing the network's servers in
  place of the photos. Then come the classic mounts from ADR-0017, then every GVfs share, taken
  from GIO's volume monitor (so mounts made by Nautilus or Files show too), each with an unmount
  button.
- **A server opens to its shares; a share mounts and opens** as an ordinary folder, the folder tree
  rooted at it. A share that's already mounted just opens. Up from a share's top goes to its
  server, and from a server to the network. If a share is unmounted while it's open, the browser
  goes to its server.
- **A server's password or question** comes up as a dialog, as in Files and Nautilus: registered
  user or anonymous, username, domain, password, and forget, remember until logout or remember
  forever. Cancelling is quiet; errors go to the status bar.
- **The path bar takes URIs** such as `smb://host/` or `smb://host/share`, which is how a server
  that isn't advertised is reached.
- **Linux only.** `gio` is a Linux-only dependency. On macOS `Network` finds and mounts nothing,
  and the Network place isn't shown.

## Consequences

- Photograph shares mounts and saved passwords with the rest of the desktop.
- Photograph now links GLib and GIO on Linux. The `.deb` depends on `libglib2.0-0` and recommends
  `gvfs-backends` (for SMB) and `gvfs-fuse` (for the folders). Without gvfsd-fuse a share mounts
  but has no folder, which the status bar says. The CI runner needs `libglib2.0-dev` to build.
- Photos over FUSE are slower than local ones, and the folder scan still runs on the UI thread, so
  a slow share can stall the window while it's listed (as ADR-0017 noted for browsing into a hung
  mount).
- Browsing depends on what GVfs finds. Servers that advertise neither DNS-SD nor WS-Discovery don't
  appear and have to be typed into the path bar.
- The module is a copy of Files', not a shared crate: fixes to one have to be made to the other
  until it moves into a crate both use.

//! Discovers sidebar locations: mounted storage (local disks, ZFS pools,
//! removable drives) and network shares (classic NFS/CIFS/sshfs mounts plus
//! GVfs connections opened from the desktop file manager).
//!
//! Nothing here stats the discovered paths — a hung network mount would
//! otherwise block the UI thread. Everything comes from `/proc/self/mounts`
//! and a single `read_dir` of the GVfs runtime directory.

use std::path::{Path, PathBuf};

/// A navigable place shown in the sidebar.
#[derive(Debug, Clone, PartialEq)]
pub struct Location {
    pub path: PathBuf,
    pub label: String,
    /// Hover text: full path and where it comes from.
    pub detail: String,
}

/// Filesystems that hold user data on local or removable disks.
const STORAGE_FS_TYPES: &[&str] = &[
    "ext2", "ext3", "ext4", "xfs", "btrfs", "bcachefs", "zfs", "f2fs", "jfs", "reiserfs", "vfat",
    "exfat", "ntfs", "ntfs3", "fuseblk", "hfsplus", "iso9660", "udf",
];

/// Filesystems backed by a remote server.
const NETWORK_FS_TYPES: &[&str] = &[
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "smbfs",
    "fuse.sshfs",
    "fuse.rclone",
    "davfs",
    "fuse.davfs2",
    "afs",
    "9p",
    "ftpfs",
    "fuse.curlftpfs",
];

/// OS-owned trees whose mounts (`/boot/efi`, ZFS-on-root `/var/lib`, docker
/// overlays, …) are never photo libraries. Mounts inside the user's home
/// directory and removable-media directories are always kept.
const SYSTEM_PREFIXES: &[&str] = &[
    "/boot", "/efi", "/proc", "/sys", "/dev", "/run", "/tmp", "/var", "/usr", "/snap", "/etc",
    "/opt", "/root", "/nix", "/home",
];

const REMOVABLE_PREFIXES: &[&str] = &["/media", "/run/media"];

/// Storage and network mounts, read from `/proc/self/mounts`.
pub fn mounted_locations(home: Option<&Path>) -> (Vec<Location>, Vec<Location>) {
    match std::fs::read_to_string("/proc/self/mounts") {
        Ok(contents) => parse_mounts(&contents, home),
        Err(_) => (Vec::new(), Vec::new()),
    }
}

/// Active GVfs mounts — how GNOME Files surfaces `smb://`, `sftp://`, etc.
/// connections — exposed through the `gvfsd-fuse` directory.
pub fn gvfs_locations() -> Vec<Location> {
    let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return Vec::new();
    };
    let gvfs_dir = PathBuf::from(runtime_dir).join("gvfs");
    let Ok(entries) = std::fs::read_dir(&gvfs_dir) else {
        return Vec::new();
    };
    let mut out: Vec<Location> = entries
        .flatten()
        .map(|entry| {
            let raw = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            Location {
                label: friendly_gvfs_label(&raw),
                detail: path.display().to_string(),
                path,
            }
        })
        .collect();
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

fn parse_mounts(contents: &str, home: Option<&Path>) -> (Vec<Location>, Vec<Location>) {
    let mut storage = Vec::new();
    let mut network = Vec::new();

    for line in contents.lines() {
        let mut fields = line.split_whitespace();
        let (Some(source), Some(mount_point), Some(fs_type)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let bucket = if STORAGE_FS_TYPES.contains(&fs_type) {
            &mut storage
        } else if NETWORK_FS_TYPES.contains(&fs_type) {
            &mut network
        } else {
            continue;
        };
        let path = PathBuf::from(unescape_mount_field(mount_point));
        if !is_user_mount(&path, home) {
            continue;
        }
        let label = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let detail = format!(
            "{} ({}, {})",
            path.display(),
            unescape_mount_field(source),
            fs_type
        );
        bucket.push(Location {
            path,
            label,
            detail,
        });
    }

    (collapse_nested(storage), collapse_nested(network))
}

/// Whether a mount point is somewhere a user would keep photos, as opposed
/// to the root filesystem, an OS-owned tree, or a parent of their home.
fn is_user_mount(path: &Path, home: Option<&Path>) -> bool {
    if path == Path::new("/") {
        return false;
    }
    if let Some(home) = home {
        if home.starts_with(path) {
            // Home itself, or a partition containing it (e.g. a separate /home):
            // already reachable via the Home location.
            return false;
        }
        if path.starts_with(home) {
            return true;
        }
    }
    if REMOVABLE_PREFIXES.iter().any(|p| path.starts_with(p)) {
        return true;
    }
    !SYSTEM_PREFIXES.iter().any(|p| path.starts_with(p))
}

/// Keeps only top-most mounts, so a ZFS pool at `/tank` isn't listed again for
/// each child dataset (`/tank/backups`, …) — those are one click away.
fn collapse_nested(mut locations: Vec<Location>) -> Vec<Location> {
    locations.sort_by(|a, b| a.path.cmp(&b.path));
    let mut kept: Vec<Location> = Vec::new();
    for loc in locations {
        if !kept.iter().any(|k| loc.path.starts_with(&k.path)) {
            kept.push(loc);
        }
    }
    kept.sort_by(|a, b| a.label.cmp(&b.label));
    kept
}

/// Resolves text typed into the path bar: trims whitespace and expands a
/// leading `~` to the home directory.
pub fn expand_typed_path(input: &str, home: Option<&Path>) -> PathBuf {
    let input = input.trim();
    match (input.strip_prefix('~'), home) {
        (Some(""), Some(home)) => home.to_path_buf(),
        (Some(rest), Some(home)) if rest.starts_with('/') => home.join(&rest[1..]),
        _ => PathBuf::from(input),
    }
}

/// Turns a raw GVfs mount directory name (e.g.
/// `smb-share:server=nas,share=photos`) into a readable label like
/// `photos on nas`. Falls back to the raw name for unrecognized schemes.
fn friendly_gvfs_label(raw: &str) -> String {
    let Some((scheme, rest)) = raw.split_once(':') else {
        return raw.to_string();
    };

    let mut server = None;
    let mut share = None;
    for kv in rest.split(',') {
        if let Some((key, value)) = kv.split_once('=') {
            match key {
                "server" | "host" => server = server.or(Some(value)),
                "share" => share = Some(value),
                _ => {}
            }
        }
    }

    match (share, server) {
        (Some(share), Some(server)) => format!("{share} on {server}"),
        (None, Some(server)) => format!("{server} ({scheme})"),
        _ => raw.to_string(),
    }
}

/// Decodes the octal escapes (`\040` for space, etc.) `/proc/mounts` uses
/// for whitespace and backslashes in mount-point paths.
fn unescape_mount_field(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            if let Ok(code) = u8::from_str_radix(&field[i + 1..i + 4], 8) {
                out.push(code);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTS: &str = "\
/dev/nvme0n1p2 / ext4 rw,relatime 0 0
/dev/nvme0n1p1 /boot/efi vfat rw 0 0
proc /proc proc rw 0 0
tmpfs /run tmpfs rw 0 0
/dev/sdb1 /run/media/divan/RawOne exfat rw 0 0
/dev/sdc1 /media/divan/Card\\040One vfat rw 0 0
portal /run/user/1000/doc fuse.portal rw 0 0
gvfsd-fuse /run/user/1000/gvfs fuse.gvfsd-fuse rw 0 0
tank /tank zfs rw 0 0
tank/backups /tank/backups zfs rw 0 0
tank/backups/home /tank/backups/home zfs rw 0 0
rpool/USERDATA/divan /home/divan zfs rw 0 0
rpool/ROOT/ubuntu/var/lib /var/lib zfs rw 0 0
overlay /var/lib/docker/rootfs/overlayfs/abc overlay rw 0 0
nas:/photos /mnt/photos nfs4 rw 0 0
//nas/raw /home/divan/nas cifs rw 0 0
/dev/sdd1 /home ext4 rw 0 0
";

    fn paths(locs: &[Location]) -> Vec<&str> {
        locs.iter().map(|l| l.path.to_str().unwrap()).collect()
    }

    #[test]
    fn parse_mounts_keeps_user_storage_and_collapses_datasets() {
        let (storage, _) = parse_mounts(MOUNTS, Some(Path::new("/home/divan")));
        assert_eq!(
            paths(&storage),
            vec!["/media/divan/Card One", "/run/media/divan/RawOne", "/tank"]
        );
    }

    #[test]
    fn parse_mounts_finds_network_shares_including_inside_home() {
        let (_, network) = parse_mounts(MOUNTS, Some(Path::new("/home/divan")));
        assert_eq!(paths(&network), vec!["/home/divan/nas", "/mnt/photos"]);
    }

    #[test]
    fn parse_mounts_labels_with_last_component_and_details_source() {
        let (storage, _) = parse_mounts(MOUNTS, Some(Path::new("/home/divan")));
        let tank = storage.iter().find(|l| l.label == "tank").unwrap();
        assert_eq!(tank.detail, "/tank (tank, zfs)");
    }

    #[test]
    fn expand_typed_path_handles_tilde_and_whitespace() {
        let home = Some(Path::new("/home/divan"));
        assert_eq!(expand_typed_path("  ~ ", home), PathBuf::from("/home/divan"));
        assert_eq!(
            expand_typed_path("~/Pictures", home),
            PathBuf::from("/home/divan/Pictures")
        );
        assert_eq!(expand_typed_path("/tank/photos", home), PathBuf::from("/tank/photos"));
        assert_eq!(expand_typed_path("~other", home), PathBuf::from("~other"));
    }

    #[test]
    fn friendly_gvfs_label_formats_smb_share() {
        assert_eq!(
            friendly_gvfs_label("smb-share:server=nas,share=photos"),
            "photos on nas"
        );
    }

    #[test]
    fn friendly_gvfs_label_formats_sftp_with_host_only() {
        assert_eq!(
            friendly_gvfs_label("sftp:host=example.com"),
            "example.com (sftp)"
        );
    }

    #[test]
    fn friendly_gvfs_label_falls_back_for_unknown_scheme() {
        assert_eq!(friendly_gvfs_label("unknown-thing"), "unknown-thing");
    }

    #[test]
    fn unescape_mount_field_decodes_octal_space() {
        assert_eq!(unescape_mount_field(r"/mnt/My\040Share"), "/mnt/My Share");
    }
}

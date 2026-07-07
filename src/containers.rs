//! Attribute indexed disk usage to containers (Docker / Podman) — the practical
//! "usage by container / pod / cgroup" view for an SRE host. It reads each
//! container's ON-DISK metadata (no daemon socket, no `docker` binary, no root
//! beyond what reading the storage dir needs) and sizes the container's writable
//! overlay layer, json-log, and named volumes from the existing index — so it's
//! an index-only query with no filesystem re-walk.
//!
//! Notes / limits:
//! - Base image layers are SHARED between containers, so they are deliberately
//!   NOT attributed here; `writable` is what THIS container actually wrote.
//! - k8s pods run as containers under containerd/CRI-O; those runtimes are not
//!   parsed yet (documented), but Docker/Podman cover the common case.
//! - "by cgroup" for *disk space* is not a kernel concept (cgroups meter cpu/mem/
//!   io, not space); this per-container view is the meaningful equivalent.

use crate::query;
use crate::store::Store;
use anyhow::Result;
use serde_json::Value;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub struct ContainerRow {
    pub runtime: &'static str,
    pub id: String,
    pub name: String,
    pub image: String,
    pub running: bool,
    pub writable_bytes: i64,
    pub log_bytes: i64,
    pub volume_bytes: i64,
}

impl ContainerRow {
    pub fn total(&self) -> i64 {
        self.writable_bytes + self.log_bytes + self.volume_bytes
    }
}

/// (dev,inode) for a live path — the key the index is keyed by.
fn path_id(p: &Path) -> Option<(i64, i64)> {
    let m = fs::symlink_metadata(p).ok()?;
    Some((m.dev() as i64, m.ino() as i64))
}

/// Allocated bytes of a directory subtree, via the index (0 if unscanned/missing).
fn dir_bytes(store: &Store, p: &Path) -> i64 {
    match path_id(p) {
        Some((d, i)) => query::subtree_bytes(store, d, i),
        None => 0,
    }
}

/// Own size of a single file straight from the fs (one stat; logs aren't dirs).
fn file_bytes(p: &Path) -> i64 {
    fs::symlink_metadata(p)
        .map(|m| m.blocks() as i64 * 512)
        .unwrap_or(0)
}

/// The overlay writable layer (`upperdir`) of a running container, read from its
/// mount namespace. Storage-driver agnostic: works for BOTH the classic docker
/// overlay2 graphdriver and the newer containerd snapshotter (Docker's default
/// image store since v25), which have completely different on-disk layouts.
fn overlay_upperdir(pid: i64) -> Option<PathBuf> {
    if pid <= 0 {
        return None;
    }
    let mi = fs::read_to_string(format!("/proc/{pid}/mountinfo")).ok()?;
    parse_upperdir(&mi).map(PathBuf::from)
}

/// Parse the container-root overlay `upperdir` out of a /proc/<pid>/mountinfo
/// blob. Pure (no I/O) so it's unit-testable across driver layouts.
fn parse_upperdir(mountinfo: &str) -> Option<String> {
    for line in mountinfo.lines() {
        // mountinfo line: "... <mountpoint> ... - <fstype> <source> <superopts>"
        let Some((pre, post)) = line.split_once(" - ") else {
            continue;
        };
        if pre.split_whitespace().nth(4) != Some("/") {
            continue; // want the container root mount
        }
        let mut it = post.split_whitespace();
        if it.next() != Some("overlay") {
            continue;
        }
        // remaining: <source> <superopts>
        if let Some(superopts) = it.nth(1) {
            for opt in superopts.split(',') {
                if let Some(u) = opt.strip_prefix("upperdir=") {
                    return Some(u.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::parse_upperdir;

    #[test]
    fn upperdir_from_containerd_snapshotter() {
        // real-world shape: root overlay mount with a containerd snapshotter upper.
        let mi = "\
2337 1900 0:219 / / rw,relatime - overlay overlay rw,lowerdir=/x/1/fs,upperdir=/var/lib/containerd/io.containerd.snapshotter.v1.overlayfs/snapshots/795/fs,workdir=/y/work
2338 2337 0:220 / /proc rw - proc proc rw
";
        assert_eq!(
            parse_upperdir(mi).as_deref(),
            Some("/var/lib/containerd/io.containerd.snapshotter.v1.overlayfs/snapshots/795/fs")
        );
    }

    #[test]
    fn upperdir_classic_overlay2() {
        let mi = "1 0 0:1 / / rw - overlay overlay rw,lowerdir=/l,upperdir=/var/lib/docker/overlay2/abc/diff,workdir=/w\n";
        assert_eq!(
            parse_upperdir(mi).as_deref(),
            Some("/var/lib/docker/overlay2/abc/diff")
        );
    }

    #[test]
    fn no_overlay_root_returns_none() {
        // only a non-root overlay + a root that isn't overlay -> nothing to size.
        let mi = "1 0 0:1 / /data rw - overlay overlay rw,upperdir=/some/diff\n\
                  2 0 0:2 / / rw - ext4 /dev/sda1 rw\n";
        assert_eq!(parse_upperdir(mi), None);
    }
}

/// Docker data-root: honour `data-root`/`graph` in /etc/docker/daemon.json, else
/// the default /var/lib/docker.
fn docker_root() -> PathBuf {
    if let Ok(s) = fs::read_to_string("/etc/docker/daemon.json") {
        if let Ok(v) = serde_json::from_str::<Value>(&s) {
            for k in ["data-root", "graph"] {
                if let Some(p) = v.get(k).and_then(|x| x.as_str()) {
                    if !p.is_empty() {
                        return PathBuf::from(p);
                    }
                }
            }
        }
    }
    PathBuf::from("/var/lib/docker")
}

fn short(id: &str) -> String {
    id.chars().take(12).collect()
}

fn docker(store: &Store, out: &mut Vec<ContainerRow>) {
    let root = docker_root();
    let cdir = root.join("containers");
    let entries = match fs::read_dir(&cdir) {
        Ok(e) => e,
        Err(_) => return, // no docker on this host / not scanned
    };
    for ent in entries.flatten() {
        let id = ent.file_name().to_string_lossy().into_owned();
        let cfg_path = ent.path().join("config.v2.json");
        let cfg = match fs::read_to_string(&cfg_path).ok().and_then(|s| {
            serde_json::from_str::<Value>(&s).ok()
        }) {
            Some(c) => c,
            None => continue,
        };
        let name = cfg
            .get("Name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim_start_matches('/')
            .to_string();
        let image = cfg
            .get("Config")
            .and_then(|c| c.get("Image"))
            .and_then(|v| v.as_str())
            .or_else(|| cfg.get("Image").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
        let state = cfg.get("State");
        let running = state
            .and_then(|s| s.get("Running"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let pid = state
            .and_then(|s| s.get("Pid"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        // writable layer size — two resolution paths:
        //  1) running container: the overlay `upperdir` from its mountinfo (works
        //     for the classic overlay2 driver AND the containerd snapshotter);
        //  2) fallback (stopped / older docker): the classic overlay2 layerdb,
        //     which maps container id -> overlay2 dir hash -> <hash>/diff.
        let mut writable_bytes = overlay_upperdir(pid)
            .map(|u| dir_bytes(store, &u))
            .unwrap_or(0);
        if writable_bytes == 0 {
            let mount_id_file = root
                .join("image/overlay2/layerdb/mounts")
                .join(&id)
                .join("mount-id");
            if let Ok(hash) = fs::read_to_string(&mount_id_file) {
                let diff = root.join("overlay2").join(hash.trim()).join("diff");
                writable_bytes = dir_bytes(store, &diff);
            }
        }

        // json-file log
        let log_bytes = cfg
            .get("LogPath")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| file_bytes(Path::new(s)))
            .unwrap_or(0);

        // named volumes mounted into the container
        let mut volume_bytes = 0;
        if let Some(mp) = cfg.get("MountPoints").and_then(|v| v.as_object()) {
            for m in mp.values() {
                let is_vol = m.get("Type").and_then(|v| v.as_str()) == Some("volume");
                if let Some(src) = m.get("Source").and_then(|v| v.as_str()) {
                    if is_vol && !src.is_empty() {
                        volume_bytes += dir_bytes(store, Path::new(src));
                    }
                }
            }
        }

        out.push(ContainerRow {
            runtime: "docker",
            id: short(&id),
            name,
            image,
            running,
            writable_bytes,
            log_bytes,
            volume_bytes,
        });
    }
}

fn podman(store: &Store, out: &mut Vec<ContainerRow>) {
    // Podman (root storage): containers.json lists {id, names, image, layer};
    // the writable layer is overlay/<layer>/diff.
    let base = PathBuf::from("/var/lib/containers/storage");
    let list = base.join("overlay-containers/containers.json");
    let txt = match fs::read_to_string(&list) {
        Ok(t) => t,
        Err(_) => return,
    };
    let arr: Value = match serde_json::from_str(&txt) {
        Ok(v) => v,
        Err(_) => return,
    };
    let Some(items) = arr.as_array() else { return };
    for c in items {
        let id = c.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if id.is_empty() {
            continue;
        }
        let name = c
            .get("names")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let image = c
            .get("image-name")
            .or_else(|| c.get("image"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let layer = c.get("layer").and_then(|v| v.as_str()).unwrap_or("");
        let writable_bytes = if layer.is_empty() {
            0
        } else {
            dir_bytes(store, &base.join("overlay").join(layer).join("diff"))
        };
        out.push(ContainerRow {
            runtime: "podman",
            id: short(&id),
            name,
            image,
            running: false, // podman state lives elsewhere; not resolved here
            writable_bytes,
            log_bytes: 0,
            volume_bytes: 0,
        });
    }
}

/// All containers found on the host, largest total first.
pub fn list(store: &Store) -> Result<Vec<ContainerRow>> {
    let mut out = Vec::new();
    docker(store, &mut out);
    podman(store, &mut out);
    out.sort_by_key(|c| std::cmp::Reverse(c.total()));
    Ok(out)
}

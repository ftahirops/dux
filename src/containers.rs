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
use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone)]
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
        self.writable_bytes
            .saturating_add(self.log_bytes)
            .saturating_add(self.volume_bytes)
    }
    /// Bytes freed by `docker rm`-ing THIS container: a stopped container's
    /// writable layer + its json-log are dead weight; a running one's are live.
    /// (Volumes are NOT counted here — they outlive the container and are
    /// reclaimed only when orphaned; see [`Waste`].)
    pub fn reclaimable(&self) -> i64 {
        if self.running {
            0
        } else {
            self.writable_bytes.saturating_add(self.log_bytes)
        }
    }
    pub fn state(&self) -> &'static str {
        if self.running {
            "running"
        } else {
            "stopped"
        }
    }
}

/// Host-level reclaimable disk — what `docker system prune` would roughly free,
/// computed from on-disk metadata + the index (no docker socket).
#[derive(Clone, Default)]
pub struct Waste {
    /// Writable layers + logs of STOPPED containers (freed by `docker rm`).
    pub stopped_bytes: i64,
    /// Named volumes referenced by NO container (freed by `docker volume prune`).
    pub orphan_volume_bytes: i64,
    pub orphan_volume_count: usize,
    /// Build cache (freed by `docker builder prune`).
    pub build_cache_bytes: i64,
}

impl Waste {
    pub fn total(&self) -> i64 {
        self.stopped_bytes
            .saturating_add(self.orphan_volume_bytes)
            .saturating_add(self.build_cache_bytes)
    }
}

/// Volume-directory names on disk that are referenced by NO container — i.e.
/// orphaned. Pure set-difference so it's unit-testable. `referenced` holds the
/// volume *names* (the dir name under <root>/volumes) each container mounts.
fn orphan_volumes<'a>(
    on_disk: &'a [String],
    referenced: &std::collections::HashSet<String>,
) -> Vec<&'a String> {
    on_disk
        .iter()
        .filter(|name| !referenced.contains(*name))
        .collect()
}

/// (dev,inode) for a live path — the key the index is keyed by.
fn path_id(p: &Path) -> Result<Option<(i64, i64)>> {
    match fs::symlink_metadata(p) {
        Ok(m) => Ok(Some(crate::util::indexed_identity(&m)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Allocated bytes of a directory subtree, via the index (0 if unscanned/missing).
fn dir_bytes(store: &Store, p: &Path) -> Result<i64> {
    match path_id(p)? {
        Some((d, i)) => query::subtree_bytes(store, d, i),
        None => Ok(0),
    }
}

/// Own size of a single file straight from the fs (one stat; logs aren't dirs).
fn file_bytes(p: &Path) -> Result<i64> {
    match fs::symlink_metadata(p) {
        Ok(m) => crate::util::allocated_bytes(&m)
            .with_context(|| format!("reading allocated size for {}", p.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

/// The overlay writable layer (`upperdir`) of a running container, read from its
/// mount namespace. Storage-driver agnostic: works for BOTH the classic docker
/// overlay2 graphdriver and the newer containerd snapshotter (Docker's default
/// image store since v25), which have completely different on-disk layouts.
fn overlay_upperdir(pid: i64) -> Result<Option<PathBuf>> {
    if pid <= 0 {
        return Ok(None);
    }
    let path = PathBuf::from(format!("/proc/{pid}/mountinfo"));
    let mi = match fs::read_to_string(&path) {
        Ok(mountinfo) => mountinfo,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    Ok(parse_upperdir(&mi).map(PathBuf::from))
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
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::{
        docker_at_root, docker_referenced_volumes, docker_root_from_config, docker_volumes_on_disk,
        orphan_volumes, overlay_upperdir, parse_upperdir, ContainerRow, Waste,
    };
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn temp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dux-containers-{tag}-{}", std::process::id()))
    }

    fn clean(path: &std::path::Path) {
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn volume_inventory_reports_unreadable_or_missing_reference_data() {
        let root = temp("volume-inventory");
        clean(&root);
        let cdir = root.join("containers/c1");
        let vdir = root.join("volumes/used");
        std::fs::create_dir_all(&cdir).unwrap();
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(
            cdir.join("config.v2.json"),
            r#"{"MountPoints":{"/data":{"Type":"volume","Name":"used"}}}"#,
        )
        .unwrap();
        assert!(docker_referenced_volumes(&root).unwrap().contains("used"));
        assert_eq!(docker_volumes_on_disk(&root).unwrap().len(), 1);

        std::fs::remove_file(cdir.join("config.v2.json")).unwrap();
        assert!(docker_referenced_volumes(&root).is_err());
        std::fs::remove_dir_all(root.join("containers")).unwrap();
        assert!(docker_referenced_volumes(&root).is_err());
        clean(&root);
    }

    #[test]
    fn volume_inventory_propagates_directory_iteration_errors() {
        let root = temp("volume-iteration");
        clean(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("volumes"), b"not a directory").unwrap();
        assert!(docker_volumes_on_disk(&root).is_err());
        clean(&root);
    }

    #[test]
    fn docker_config_root_and_container_state_are_validated() {
        assert_eq!(
            docker_root_from_config(None).unwrap(),
            PathBuf::from("/var/lib/docker")
        );
        assert_eq!(
            docker_root_from_config(Some(r#"{"data-root":"/srv/docker"}"#)).unwrap(),
            PathBuf::from("/srv/docker")
        );
        assert!(docker_root_from_config(Some("{")).is_err());
        assert!(docker_root_from_config(Some(r#"{"data-root":"relative"}"#)).is_err());

        let root = temp("container-state");
        clean(&root);
        let container = root.join("containers/c1");
        std::fs::create_dir_all(&container).unwrap();
        let store = crate::store::Store {
            conn: rusqlite::Connection::open_in_memory().unwrap(),
            schema_version: crate::store::SCHEMA_VERSION,
        };
        store.conn.execute_batch(crate::store::SCHEMA_SQL).unwrap();

        std::fs::write(
            container.join("config.v2.json"),
            r#"{"Name":"/example","State":{"Running":true,"Pid":999999999}}"#,
        )
        .unwrap();
        let mut rows = Vec::new();
        docker_at_root(&store, &mut rows, &root).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].running);
        assert_eq!(rows[0].reclaimable(), 0);

        std::fs::write(
            container.join("config.v2.json"),
            r#"{"Name":"/example","State":{}}"#,
        )
        .unwrap();
        let mut rows = Vec::new();
        assert!(docker_at_root(&store, &mut rows, &root).is_err());
        assert!(rows.is_empty());
        clean(&root);
    }

    #[test]
    fn missing_process_mountinfo_is_distinguished_from_invalid_pid() {
        assert!(overlay_upperdir(0).unwrap().is_none());
        assert!(overlay_upperdir(i64::MAX).unwrap().is_none());
    }

    #[test]
    fn orphan_volumes_are_the_unreferenced_ones() {
        let on_disk = vec![
            "used_a".to_string(),
            "orphan_1".to_string(),
            "used_b".to_string(),
            "orphan_2".to_string(),
        ];
        let referenced: HashSet<String> = ["used_a".to_string(), "used_b".to_string()]
            .into_iter()
            .collect();
        let mut got: Vec<&String> = orphan_volumes(&on_disk, &referenced);
        got.sort();
        assert_eq!(got, vec![&"orphan_1".to_string(), &"orphan_2".to_string()]);
    }

    #[test]
    fn reclaimable_counts_stopped_only() {
        let mk = |running, w, l| ContainerRow {
            runtime: "docker",
            id: "x".into(),
            name: "n".into(),
            image: "i".into(),
            running,
            writable_bytes: w,
            log_bytes: l,
            volume_bytes: 0,
        };
        // running container: nothing reclaimable, even with a big writable layer
        assert_eq!(mk(true, 5000, 10).reclaimable(), 0);
        // stopped: writable + logs are dead weight
        assert_eq!(mk(false, 5000, 10).reclaimable(), 5010);
        assert_eq!(mk(false, 0, 0).state(), "stopped");

        let boundary = mk(false, i64::MAX, i64::MAX);
        assert_eq!(boundary.total(), i64::MAX);
        assert_eq!(boundary.reclaimable(), i64::MAX);
    }

    #[test]
    fn waste_total_sums_categories() {
        let w = Waste {
            stopped_bytes: 100,
            orphan_volume_bytes: 200,
            orphan_volume_count: 2,
            build_cache_bytes: 50,
        };
        assert_eq!(w.total(), 350);
        let boundary = Waste {
            stopped_bytes: i64::MAX,
            orphan_volume_bytes: i64::MAX,
            build_cache_bytes: i64::MAX,
            ..Waste::default()
        };
        assert_eq!(boundary.total(), i64::MAX);
    }

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
fn docker_root() -> Result<PathBuf> {
    let config_path = Path::new("/etc/docker/daemon.json");
    let config = match fs::read_to_string(config_path) {
        Ok(config) => Some(config),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", config_path.display()))
        }
    };
    docker_root_from_config(config.as_deref())
}

fn docker_root_from_config(config: Option<&str>) -> Result<PathBuf> {
    if let Some(config) = config {
        let value: Value = serde_json::from_str(config).context("parsing Docker daemon config")?;
        let object = value
            .as_object()
            .context("Docker daemon configuration must be a JSON object")?;
        for key in ["data-root", "graph"] {
            if let Some(value) = object.get(key) {
                let root = value
                    .as_str()
                    .with_context(|| format!("Docker {key} must be a string"))?;
                if !root.is_empty() {
                    anyhow::ensure!(
                        Path::new(root).is_absolute(),
                        "Docker {key} must be absolute"
                    );
                    return Ok(PathBuf::from(root));
                }
            }
        }
    }
    Ok(PathBuf::from("/var/lib/docker"))
}

fn short(id: &str) -> String {
    id.chars().take(12).collect()
}

fn docker(store: &Store, out: &mut Vec<ContainerRow>) -> Result<()> {
    let root = docker_root()?;
    docker_at_root(store, out, &root)
}

fn docker_at_root(store: &Store, out: &mut Vec<ContainerRow>, root: &Path) -> Result<()> {
    let cdir = root.join("containers");
    let Some(entries) = read_dir_if_present(&cdir)? else {
        return Ok(());
    };
    for entry in entries {
        let ent = entry.with_context(|| format!("reading entry in {}", cdir.display()))?;
        anyhow::ensure!(
            ent.file_type()?.is_dir(),
            "unexpected non-directory in {}",
            cdir.display()
        );
        let id = ent.file_name().into_string().map_err(|name| {
            anyhow::anyhow!("non-UTF8 Docker container directory name: {name:?}")
        })?;
        let cfg_path = ent.path().join("config.v2.json");
        let cfg_text = fs::read_to_string(&cfg_path)
            .with_context(|| format!("reading Docker container config {}", cfg_path.display()))?;
        let cfg: Value = serde_json::from_str(&cfg_text)
            .with_context(|| format!("parsing Docker container config {}", cfg_path.display()))?;
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
        let running = cfg
            .get("State")
            .and_then(|state| state.get("Running"))
            .and_then(Value::as_bool)
            .with_context(|| format!("missing boolean State.Running in {}", cfg_path.display()))?;
        let state = cfg.get("State");
        let pid = state
            .and_then(|s| s.get("Pid"))
            .and_then(Value::as_i64)
            .with_context(|| format!("missing integer State.Pid in {}", cfg_path.display()))?;
        anyhow::ensure!(
            !running || pid > 0,
            "running Docker container has invalid State.Pid in {}",
            cfg_path.display()
        );

        // writable layer size — two resolution paths:
        //  1) running container: the overlay `upperdir` from its mountinfo (works
        //     for the classic overlay2 driver AND the containerd snapshotter);
        //  2) fallback (stopped / older docker): the classic overlay2 layerdb,
        //     which maps container id -> overlay2 dir hash -> <hash>/diff.
        let mut writable_bytes = match overlay_upperdir(pid)? {
            Some(path) => dir_bytes(store, &path)?,
            None => 0,
        };
        if writable_bytes == 0 {
            let mount_id_file = root
                .join("image/overlay2/layerdb/mounts")
                .join(&id)
                .join("mount-id");
            let hash = match fs::read_to_string(&mount_id_file) {
                Ok(hash) => Some(hash),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("reading {}", mount_id_file.display()))
                }
            };
            if let Some(hash) = hash {
                let diff = root.join("overlay2").join(hash.trim()).join("diff");
                writable_bytes = dir_bytes(store, &diff)?;
            }
        }

        // json-file log
        let log_bytes = cfg
            .get("LogPath")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| file_bytes(Path::new(s)))
            .transpose()?
            .unwrap_or(0);

        // named volumes mounted into the container
        let mut volume_bytes = 0i64;
        if let Some(mp) = cfg.get("MountPoints").and_then(|v| v.as_object()) {
            for m in mp.values() {
                let is_vol = m.get("Type").and_then(|v| v.as_str()) == Some("volume");
                if let Some(src) = m.get("Source").and_then(|v| v.as_str()) {
                    if is_vol && !src.is_empty() {
                        volume_bytes =
                            volume_bytes.saturating_add(dir_bytes(store, Path::new(src))?);
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
    Ok(())
}

fn podman(store: &Store, out: &mut Vec<ContainerRow>) -> Result<()> {
    // Podman (root storage): containers.json lists {id, names, image, layer};
    // the writable layer is overlay/<layer>/diff.
    let base = PathBuf::from("/var/lib/containers/storage");
    let list = base.join("overlay-containers/containers.json");
    let txt = match fs::read_to_string(&list) {
        Ok(t) => t,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let arr: Value = serde_json::from_str(&txt)?;
    let Some(items) = arr.as_array() else {
        anyhow::bail!("Podman containers.json must contain an array");
    };
    for c in items {
        let id = c
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
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
            dir_bytes(store, &base.join("overlay").join(layer).join("diff"))?
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
    Ok(())
}

/// All containers found on the host, largest total first.
pub fn list(store: &Store) -> Result<Vec<ContainerRow>> {
    let mut out = Vec::new();
    docker(store, &mut out)?;
    podman(store, &mut out)?;
    out.sort_by_key(|c| std::cmp::Reverse(c.total()));
    Ok(out)
}

/// The Docker named-volume names referenced by ANY container (from every
/// container's `MountPoints` with Type=volume). Used to find orphans.
fn read_dir_if_present(path: &Path) -> Result<Option<fs::ReadDir>> {
    match fs::read_dir(path) {
        Ok(entries) => Ok(Some(entries)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("reading directory {}", path.display())),
    }
}

fn docker_referenced_volumes(root: &Path) -> Result<std::collections::HashSet<String>> {
    let mut refd = std::collections::HashSet::new();
    let cdir = root.join("containers");
    let Some(entries) = read_dir_if_present(&cdir)? else {
        if root.join("volumes").exists() {
            anyhow::bail!(
                "Docker volume inventory exists but {} is missing",
                cdir.display()
            );
        }
        return Ok(refd);
    };
    for entry in entries {
        let ent = entry.with_context(|| format!("reading entry in {}", cdir.display()))?;
        anyhow::ensure!(
            ent.file_type()?.is_dir(),
            "unexpected non-directory in {}",
            cdir.display()
        );
        let cfg_path = ent.path().join("config.v2.json");
        let config = fs::read_to_string(&cfg_path)
            .with_context(|| format!("reading Docker container config {}", cfg_path.display()))?;
        let cfg: Value = serde_json::from_str(&config)
            .with_context(|| format!("parsing Docker container config {}", cfg_path.display()))?;
        let mounts = match cfg.get("MountPoints") {
            None | Some(Value::Null) => None,
            Some(value) => Some(value.as_object().with_context(|| {
                format!("MountPoints must be an object in {}", cfg_path.display())
            })?),
        };
        if let Some(mp) = mounts {
            for m in mp.values() {
                let mount_type = m
                    .get("Type")
                    .and_then(Value::as_str)
                    .with_context(|| format!("missing mount Type in {}", cfg_path.display()))?;
                if mount_type != "volume" {
                    continue;
                }
                // prefer the explicit volume Name; else the dir under volumes/.
                if let Some(n) = m.get("Name").and_then(|v| v.as_str()) {
                    if !n.is_empty() {
                        refd.insert(n.to_string());
                        continue;
                    }
                }
                if let Some(src) = m.get("Source").and_then(|v| v.as_str()) {
                    // .../volumes/<name>/_data  -> <name>
                    if let Some(name) = Path::new(src).parent().and_then(|p| p.file_name()) {
                        refd.insert(name.to_string_lossy().into_owned());
                        continue;
                    }
                }
                anyhow::bail!(
                    "cannot identify referenced Docker volume in {}",
                    cfg_path.display()
                );
            }
        }
    }
    Ok(refd)
}

/// Named volumes present on disk: (name, _data path). Skips the store's own
/// bookkeeping entries (`metadata.db`, `backingFsBlockDev`).
fn docker_volumes_on_disk(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let vdir = root.join("volumes");
    let mut out = Vec::new();
    let Some(entries) = read_dir_if_present(&vdir)? else {
        return Ok(out);
    };
    for entry in entries {
        let ent = entry.with_context(|| format!("reading entry in {}", vdir.display()))?;
        if !ent.file_type()?.is_dir() {
            continue; // metadata.db / backingFsBlockDev are files
        }
        let name = ent
            .file_name()
            .into_string()
            .map_err(|name| anyhow::anyhow!("non-UTF8 Docker volume directory name: {name:?}"))?;
        out.push((name, ent.path().join("_data")));
    }
    Ok(out)
}

/// Host-level reclaimable disk (≈ what `docker system prune --volumes` frees),
/// from on-disk metadata + the index. `rows` is the output of [`list`].
pub fn waste(store: &Store, rows: &[ContainerRow]) -> Result<Waste> {
    // stopped containers: writable layer + logs are dead weight
    let stopped_bytes = rows
        .iter()
        .filter(|c| c.runtime == "docker" && !c.running)
        .fold(0i64, |total, c| total.saturating_add(c.reclaimable()));

    let root = docker_root()?;
    // orphaned named volumes: on disk but referenced by no container
    let referenced = docker_referenced_volumes(&root)?;
    let on_disk = docker_volumes_on_disk(&root)?;
    let names: Vec<String> = on_disk.iter().map(|(n, _)| n.clone()).collect();
    let orphan_names: std::collections::HashSet<&String> =
        orphan_volumes(&names, &referenced).into_iter().collect();
    let (mut orphan_volume_bytes, mut orphan_volume_count) = (0i64, 0usize);
    for (name, data) in &on_disk {
        if orphan_names.contains(name) {
            orphan_volume_bytes = orphan_volume_bytes.saturating_add(dir_bytes(store, data)?);
            orphan_volume_count += 1;
        }
    }

    // build cache (buildkit) — reclaimable via `docker builder prune`
    let build_cache_bytes = ["buildkit", "buildx"]
        .iter()
        .map(|sub| dir_bytes(store, &root.join(sub)))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .fold(0i64, i64::saturating_add);

    Ok(Waste {
        stopped_bytes,
        orphan_volume_bytes,
        orphan_volume_count,
        build_cache_bytes,
    })
}

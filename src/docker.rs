//! `dux docker`: where Docker's disk space actually goes.
//!
//! Uses `docker system df -v` for sizes Docker itself computes (shared image
//! layers counted once), adds container log sizes from disk, the real folder
//! sizes under Docker's and containerd's data roots, and a per compose
//! project roll-up. Cleanup is planned with the same safety rules as
//! `dux cleanup` and only runs with `--apply`.

use crate::cleanup::{Action, Item};
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::process::Command;

/// Parse Docker's decimal SI sizes: "31MB", "12.92kB", "1.2GB", "0B", "N/A".
pub fn parse_si(s: &str) -> u64 {
    let s = s.trim();
    let split = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let Ok(n) = num.trim().parse::<f64>() else {
        return 0;
    };
    let mult = match unit.trim() {
        "B" | "" => 1.0,
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        _ => return 0,
    };
    (n * mult) as u64
}

/// Docker prints labels as `k=v,k=v`, but a value can itself contain
/// commas (compose `config_files` lists several files): a segment without
/// `=` belongs to the previous value.
pub(crate) fn labels_map(labels: &str) -> BTreeMap<String, String> {
    let mut m: BTreeMap<String, String> = BTreeMap::new();
    let mut last: Option<String> = None;
    for seg in labels.split(',') {
        match seg.split_once('=') {
            Some((k, v)) if !k.is_empty() && !k.contains('/') => {
                m.insert(k.to_string(), v.to_string());
                last = Some(k.to_string());
            }
            _ => {
                if let Some(v) = last.as_ref().and_then(|k| m.get_mut(k)) {
                    v.push(',');
                    v.push_str(seg);
                }
            }
        }
    }
    m
}

fn label(labels: &str, key: &str) -> Option<String> {
    labels_map(labels).remove(key).filter(|v| !v.is_empty())
}

fn list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(str::to_string)
        .collect()
}

#[derive(Clone, Debug, Default)]
pub struct Image {
    /// Docker's own "dangling" (what `image prune` removes when unused).
    pub dangling: bool,
    /// When the image arrived on this host (unix; 0 unknown). Docker's
    /// `--filter until=` judges by this, not by the build date.
    pub arrived: i64,
    /// "docker" or "podman" (empty means docker).
    pub runtime: &'static str,
    pub name: String,
    pub id: String,
    pub size: u64,
    pub unique: u64,
    pub shared: u64,
    pub containers: u32,
    pub created: String,
}

#[derive(Clone, Debug, Default)]
pub struct Container {
    pub runtime: &'static str,
    pub name: String,
    pub id: String,
    pub image: String,
    /// Image ID when the runtime reports it (Podman does).
    pub image_id: String,
    pub state: String,
    pub status: String,
    pub writable: u64,
    pub log_bytes: u64,
    pub log_path: String,
    pub project: Option<String>,
    /// Compose service name.
    pub service: Option<String>,
    /// Compose project folder and compose files (where the stack comes from).
    pub working_dir: Option<String>,
    pub config_files: Vec<String>,
    pub command: String,
    pub created: String,
    pub running_for: String,
    pub ports: String,
    pub networks: Vec<String>,
    /// Volume names or bind-mount sources (Docker may shorten long names with "…").
    pub mounts: Vec<String>,
    pub health: String,
}

impl Container {
    pub fn running(&self) -> bool {
        self.state == "running"
    }
}

#[derive(Clone, Debug, Default)]
pub struct Volume {
    pub runtime: &'static str,
    pub name: String,
    pub size: u64,
    pub links: u32,
    pub project: Option<String>,
    pub driver: String,
    pub mountpoint: String,
    /// Compose volume name inside its project.
    pub compose_name: Option<String>,
    /// Creation time (unix), 0 when unknown.
    pub created: i64,
}

/// A compose project (or the standalone containers), with everything it owns.
#[derive(Clone, Debug, Default)]
pub struct Stack {
    pub name: String,
    pub working_dir: Option<String>,
    pub config_files: Vec<String>,
    /// Indexes into Report.containers / images / volumes.
    pub containers: Vec<usize>,
    pub images: Vec<usize>,
    pub volumes: Vec<usize>,
    pub running: usize,
    pub writable: u64,
    pub logs: u64,
    pub image_bytes: u64,
    pub volume_bytes: u64,
}

impl Stack {
    pub fn total(&self) -> u64 {
        self.writable + self.logs + self.image_bytes + self.volume_bytes
    }
}

/// Label used for containers that belong to no compose project.
pub const STANDALONE: &str = "(standalone containers)";

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub root_dir: String,
    pub images: Vec<Image>,
    pub containers: Vec<Container>,
    pub volumes: Vec<Volume>,
    pub build_cache: u64,
    pub build_cache_reclaimable: u64,
    /// (path, bytes) of folders under Docker's and containerd's data roots.
    pub on_disk: Vec<(String, u64)>,
}

impl Report {
    /// The volume a mount names. Docker shortens long names with "…"; a
    /// shortened name counts only when exactly one volume has that prefix,
    /// so an ambiguous mount is never attributed to a guessed volume.
    pub fn volume_for_mount(&self, m: &str) -> Option<usize> {
        if let Some(i) = self.volumes.iter().position(|v| v.name == m) {
            return Some(i);
        }
        let p = m.strip_suffix('…').filter(|p| !p.is_empty())?;
        let mut hits = (0..self.volumes.len()).filter(|&i| self.volumes[i].name.starts_with(p));
        let first = hits.next()?;
        hits.next().is_none().then_some(first)
    }

    /// Does container `c` mount volume `v`?
    pub fn mounts_volume(&self, c: &Container, v: usize) -> bool {
        c.mounts.iter().any(|m| self.volume_for_mount(m) == Some(v))
    }

    /// For every image, the containers using it (one pass).
    pub fn image_users(&self) -> Vec<Vec<usize>> {
        let mut out = vec![Vec::new(); self.images.len()];
        for (ci, c) in self.containers.iter().enumerate() {
            if let Some(i) = self.image_of(c) {
                out[i].push(ci);
            }
        }
        out
    }

    pub fn image_of(&self, c: &Container) -> Option<usize> {
        self.images.iter().position(|i| {
            (!c.image_id.is_empty() && i.id == c.image_id && i.runtime == c.runtime)
                || i.name == c.image
                || i.name == format!("{}:latest", c.image)
                || (!i.id.is_empty()
                    && (c.image == i.id
                        || i.id
                            .trim_start_matches("sha256:")
                            .starts_with(c.image.trim_start_matches("sha256:"))))
        })
    }

    /// Containers using image `i`.
    pub fn users_of_image(&self, i: usize) -> Vec<usize> {
        (0..self.containers.len())
            .filter(|&c| self.image_of(&self.containers[c]) == Some(i))
            .collect()
    }

    /// Containers mounting volume `v`.
    pub fn users_of_volume(&self, v: usize) -> Vec<usize> {
        (0..self.containers.len())
            .filter(|&c| self.mounts_volume(&self.containers[c], v))
            .collect()
    }

    /// Compose stacks with their containers, images and volumes, biggest first.
    pub fn stacks(&self) -> Vec<Stack> {
        let mut m: BTreeMap<String, Stack> = BTreeMap::new();
        for (ci, c) in self.containers.iter().enumerate() {
            let name = c.project.clone().unwrap_or_else(|| STANDALONE.into());
            let st = m.entry(name.clone()).or_insert_with(|| Stack {
                name,
                ..Stack::default()
            });
            if st.working_dir.is_none() {
                st.working_dir = c.working_dir.clone();
            }
            for f in &c.config_files {
                if !st.config_files.contains(f) {
                    st.config_files.push(f.clone());
                }
            }
            st.containers.push(ci);
            st.running += usize::from(c.running());
            st.writable += c.writable;
            st.logs += c.log_bytes;
            if let Some(ii) = self.image_of(c) {
                if !st.images.contains(&ii) {
                    st.images.push(ii);
                    st.image_bytes += self.images[ii].unique;
                }
            }
        }
        for (vi, v) in self.volumes.iter().enumerate() {
            let owner = v.project.clone().or_else(|| {
                self.users_of_volume(vi).first().map(|&c| {
                    self.containers[c]
                        .project
                        .clone()
                        .unwrap_or_else(|| STANDALONE.into())
                })
            });
            if let Some(o) = owner {
                let st = m.entry(o.clone()).or_insert_with(|| Stack {
                    name: o,
                    ..Stack::default()
                });
                st.volumes.push(vi);
                st.volume_bytes += v.size;
            }
        }
        let mut v: Vec<Stack> = m.into_values().collect();
        v.sort_by_key(|s| std::cmp::Reverse(s.total()));
        v
    }

    /// In use first, then unused; biggest first in each group. Views and
    /// actions address rows by position, so this runs once, at collection.
    pub fn organise(&mut self) {
        let users = self.image_users();
        let mut im: Vec<(bool, Image)> = self
            .images
            .drain(..)
            .enumerate()
            .map(|(i, x)| (users[i].is_empty() && x.containers == 0, x))
            .collect();
        im.sort_by_key(|(unused, x)| (*unused, std::cmp::Reverse(x.unique)));
        self.images = im.into_iter().map(|(_, x)| x).collect();
        let vu: Vec<bool> = (0..self.volumes.len())
            .map(|i| self.volumes[i].links == 0 && self.users_of_volume(i).is_empty())
            .collect();
        let mut vo: Vec<(bool, Volume)> = vu.into_iter().zip(self.volumes.drain(..)).collect();
        vo.sort_by_key(|(unused, x)| (*unused, std::cmp::Reverse(x.size)));
        self.volumes = vo.into_iter().map(|(_, x)| x).collect();
    }

    pub fn images_total(&self) -> u64 {
        // Shared layers count once: sum of unique sizes plus the largest shared size.
        self.images.iter().map(|i| i.unique).sum::<u64>()
            + self.images.iter().map(|i| i.shared).max().unwrap_or(0)
    }
    pub fn unused_images(&self) -> impl Iterator<Item = &Image> {
        self.images.iter().filter(|i| i.containers == 0)
    }
    pub fn logs_total(&self) -> u64 {
        self.containers.iter().map(|c| c.log_bytes).sum()
    }
    pub fn writable_total(&self) -> u64 {
        self.containers.iter().map(|c| c.writable).sum()
    }
    pub fn volumes_total(&self) -> u64 {
        self.volumes.iter().map(|v| v.size).sum()
    }
    pub fn unused_volumes(&self) -> impl Iterator<Item = &Volume> {
        self.volumes.iter().filter(|v| v.links == 0)
    }

    /// Usage per compose project: (images, writable, logs, volumes).
    pub fn by_project(&self) -> BTreeMap<String, [u64; 4]> {
        let mut m: BTreeMap<String, [u64; 4]> = BTreeMap::new();
        // An image's unique size counts once per project, however many
        // containers (replicas) run from it.
        let mut counted: std::collections::HashSet<(String, String)> = Default::default();
        for c in &self.containers {
            let p = c
                .project
                .clone()
                .unwrap_or_else(|| "(no compose project)".into());
            let e = m.entry(p.clone()).or_default();
            e[1] += c.writable;
            e[2] += c.log_bytes;
            let img = self
                .images
                .iter()
                .find(|i| i.name == c.image || i.name == format!("{}:latest", c.image));
            if let Some(img) = img {
                if counted.insert((p, img.name.clone())) {
                    e[0] += img.unique;
                }
            }
        }
        for v in &self.volumes {
            let p = v
                .project
                .clone()
                .unwrap_or_else(|| "(no compose project)".into());
            m.entry(p).or_default()[3] += v.size;
        }
        m
    }
}

/// Parse `docker system df -v --format '{{json .}}'`.
pub fn parse_df(text: &str) -> Result<Report> {
    let v: Value = serde_json::from_str(text.trim()).context("parsing docker system df output")?;
    let s = |x: &Value, k: &str| x[k].as_str().unwrap_or("").to_string();
    let mut r = Report::default();
    for i in v["Images"].as_array().into_iter().flatten() {
        let repo = s(i, "Repository");
        let tag = s(i, "Tag");
        r.images.push(Image {
            dangling: false,
            arrived: 0,
            runtime: "docker",
            name: if repo == "<none>" || repo.is_empty() {
                format!(
                    "<none> {}",
                    s(i, "ID")
                        .trim_start_matches("sha256:")
                        .chars()
                        .take(12)
                        .collect::<String>()
                )
            } else {
                format!("{repo}:{tag}")
            },
            id: s(i, "ID"),
            size: parse_si(&s(i, "Size")),
            unique: parse_si(&s(i, "UniqueSize")),
            shared: parse_si(&s(i, "SharedSize")),
            containers: s(i, "Containers").parse().unwrap_or(0),
            created: s(i, "CreatedSince"),
        });
    }
    for c in v["Containers"].as_array().into_iter().flatten() {
        let labels = labels_map(&s(c, "Labels"));
        let lab = |k: &str| labels.get(k).filter(|v| !v.is_empty()).cloned();
        r.containers.push(Container {
            runtime: "docker",
            name: s(c, "Names"),
            id: s(c, "ID"),
            image: s(c, "Image"),
            state: s(c, "State"),
            status: s(c, "Status"),
            writable: parse_si(s(c, "Size").split_whitespace().next().unwrap_or("0B")),
            project: lab("com.docker.compose.project"),
            service: lab("com.docker.compose.service"),
            working_dir: lab("com.docker.compose.project.working_dir"),
            config_files: lab("com.docker.compose.project.config_files")
                .map(|f| list(&f))
                .unwrap_or_default(),
            command: s(c, "Command").trim_matches('"').to_string(),
            created: s(c, "CreatedAt"),
            running_for: s(c, "RunningFor"),
            ports: s(c, "Ports"),
            networks: list(&s(c, "Networks")),
            mounts: list(&s(c, "Mounts")),
            health: s(c, "HealthStatus"),
            ..Container::default()
        });
    }
    for vol in v["Volumes"].as_array().into_iter().flatten() {
        let labels = s(vol, "Labels");
        r.volumes.push(Volume {
            created: 0,
            runtime: "docker",
            name: s(vol, "Name"),
            size: parse_si(&s(vol, "Size")),
            links: s(vol, "Links").parse().unwrap_or(0),
            project: label(&labels, "com.docker.compose.project"),
            compose_name: label(&labels, "com.docker.compose.volume"),
            driver: s(vol, "Driver"),
            mountpoint: s(vol, "Mountpoint"),
        });
    }
    for b in v["BuildCache"].as_array().into_iter().flatten() {
        let size = parse_si(&s(b, "Size"));
        r.build_cache += size;
        if s(b, "InUse") != "true" && b["InUse"].as_bool() != Some(true) {
            r.build_cache_reclaimable += size;
        }
    }
    Ok(r)
}

fn docker(args: &[&str]) -> Result<String> {
    let out = Command::new("docker")
        .args(args)
        .output()
        .context("running docker (is it installed?)")?;
    anyhow::ensure!(
        out.status.success(),
        "docker {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// What takes the space inside a folder (a container's writable layer or a
/// volume): the biggest top-level folders, each with its biggest sub-folders,
/// as paths relative to the folder ("/tmp", "/var/log/app"), biggest first.
pub fn inside(path: &str, top: usize) -> Result<Vec<(String, u64)>> {
    let t = crate::walker::walk(path, 2)?;
    let mut first: Vec<&crate::walker::DirNode> = t.root.children.iter().collect();
    first.sort_by_key(|c| std::cmp::Reverse(c.total_bytes));
    let mut out = Vec::new();
    for c in first.into_iter().filter(|c| c.total_bytes > 0).take(top) {
        out.push((format!("/{}", c.name), c.total_bytes));
        let mut kids: Vec<&crate::walker::DirNode> = c.children.iter().collect();
        kids.sort_by_key(|k| std::cmp::Reverse(k.total_bytes));
        for k in kids
            .into_iter()
            .filter(|k| k.total_bytes >= c.total_bytes / 10 && k.total_bytes > 0)
            .take(3)
        {
            out.push((format!("/{}/{}", c.name, k.name), k.total_bytes));
        }
    }
    Ok(out)
}

/// The folder holding a container's writable layer (overlay "upper" dir).
pub fn upper_dir(runtime: &str, id: &str) -> Result<String> {
    let args = ["inspect", "--format", "{{.GraphDriver.Data.UpperDir}}", id];
    let classic = if runtime == "podman" {
        podman(&args)
    } else {
        docker(&args)
    };
    if let Ok(out) = classic {
        let d = out.trim().to_string();
        if !d.is_empty() && d != "<no value>" {
            return Ok(d);
        }
    }
    // Docker with the containerd image store has no GraphDriver: ask
    // containerd for the container's snapshot. `ctr ... mounts` only prints
    // the mount command, it mounts nothing.
    anyhow::ensure!(
        runtime != "podman",
        "Podman did not report a writable folder"
    );
    let out = Command::new("ctr")
        .args([
            "-n",
            "moby",
            "snapshots",
            "--snapshotter",
            "overlayfs",
            "mounts",
            "/nonexistent",
            id,
        ])
        .output()
        .context("running ctr (containerd image store)")?;
    anyhow::ensure!(
        out.status.success(),
        "containerd has no snapshot for this container"
    );
    upperdir_from_mount(&String::from_utf8_lossy(&out.stdout))
        .context("the storage driver does not expose a writable folder")
}

/// The `upperdir=` of an overlay mount command line.
pub(crate) fn upperdir_from_mount(cmd: &str) -> Option<String> {
    cmd.split(['=', ','])
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0].ends_with("upperdir"))
        .map(|w| w[1].trim().to_string())
        .filter(|d| d.starts_with('/'))
}

/// Plain advice for a container that writes a lot into its own layer.
pub fn layer_advice(c: &Container, inside: &[(String, u64)]) -> Option<String> {
    let (top, bytes) = inside.iter().find(|(p, _)| p.matches('/').count() == 1)?;
    // Name the sub-folder when it holds nearly all of it (/root/.cache, not /root).
    let top = inside
        .iter()
        .find(|(p, b)| p.starts_with(&format!("{top}/")) && *b * 10 >= *bytes * 8)
        .map_or(top, |(p, _)| p);
    (c.writable >= 50 << 20 && *bytes >= 20 << 20).then(|| {
        format!(
            "{} writes {} into its own layer under {top}. That data is lost when the container is recreated and cannot be cleaned safely from outside. Mount a volume at {top} (or send logs to stdout or a log driver).",
            c.name,
            crate::util::human(*bytes as i64)
        )
    })
}

/// The command line tool for a runtime ("podman" or "docker").
pub fn program(runtime: &str) -> &'static str {
    if runtime == "podman" {
        "podman"
    } else {
        "docker"
    }
}

fn has(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}

/// Parse `podman ps -a --size`, `podman images` and `podman volume ls`
/// (all `--format json`) into report entries tagged "podman".
pub fn parse_podman(ps: &str, images: &str, volumes: &str) -> Result<Report> {
    let mut r = Report::default();
    let arr = |t: &str| -> Result<Vec<Value>> {
        let v: Value = serde_json::from_str(t.trim()).context("parsing podman output")?;
        Ok(v.as_array().cloned().unwrap_or_default())
    };
    let st = |x: &Value, k: &str| x[k].as_str().unwrap_or("").to_string();
    for i in arr(images)? {
        let name = i["Names"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!(
                    "<none> {}",
                    st(&i, "Id").chars().take(12).collect::<String>()
                )
            });
        let size = i["Size"].as_u64().unwrap_or(0);
        let shared = i["SharedSize"].as_u64().unwrap_or(0);
        r.images.push(Image {
            dangling: false,
            arrived: 0,
            runtime: "podman",
            name,
            id: st(&i, "Id"),
            size,
            unique: size.saturating_sub(shared),
            shared,
            containers: i["Containers"].as_u64().unwrap_or(0) as u32,
            created: i["CreatedAt"].as_str().unwrap_or("").to_string(),
        });
    }
    for c in arr(ps)? {
        let labels: BTreeMap<String, String> = c["Labels"]
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let lab = |k: &str| labels.get(k).filter(|v| !v.is_empty()).cloned();
        let strs = |k: &str| -> Vec<String> {
            c[k].as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };
        r.containers.push(Container {
            runtime: "podman",
            name: strs("Names").first().cloned().unwrap_or_default(),
            id: st(&c, "Id"),
            image: st(&c, "Image"),
            image_id: st(&c, "ImageID"),
            state: st(&c, "State"),
            status: st(&c, "Status"),
            writable: c["Size"]["rwSize"].as_u64().unwrap_or(0),
            project: lab("com.docker.compose.project").or_else(|| lab("io.podman.compose.project")),
            service: lab("com.docker.compose.service"),
            working_dir: lab("com.docker.compose.project.working_dir"),
            config_files: lab("com.docker.compose.project.config_files")
                .map(|f| list(&f))
                .unwrap_or_default(),
            command: strs("Command").join(" "),
            created: st(&c, "CreatedAt"),
            running_for: st(&c, "CreatedAt"),
            ports: c["Ports"]
                .as_array()
                .map(|a| format!("{} port mapping(s)", a.len()))
                .unwrap_or_default(),
            networks: strs("Networks"),
            mounts: strs("Mounts"),
            ..Container::default()
        });
    }
    for v in arr(volumes)? {
        let labels = v["Labels"].as_object();
        let lab = |k: &str| {
            labels
                .and_then(|m| m.get(k))
                .and_then(|x| x.as_str())
                .filter(|x| !x.is_empty())
                .map(str::to_string)
        };
        r.volumes.push(Volume {
            created: 0,
            runtime: "podman",
            name: st(&v, "Name"),
            size: 0,
            links: v["MountCount"].as_u64().unwrap_or(0) as u32,
            project: lab("com.docker.compose.project"),
            compose_name: lab("com.docker.compose.volume"),
            driver: st(&v, "Driver"),
            mountpoint: st(&v, "Mountpoint"),
        });
    }
    Ok(r)
}

fn podman(args: &[&str]) -> Result<String> {
    let out = Command::new("podman")
        .args(args)
        .output()
        .context("running podman")?;
    anyhow::ensure!(
        out.status.success(),
        "podman {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Collect Podman's containers, images and volumes (root storage).
fn collect_podman() -> Result<Report> {
    let mut r = parse_podman(
        &podman(&["ps", "-a", "--size", "--format", "json"])?,
        &podman(&["images", "--format", "json"])?,
        &podman(&["volume", "ls", "--format", "json"])?,
    )?;
    for v in &mut r.volumes {
        if let Ok(t) = crate::walker::walk(&v.mountpoint, 2) {
            v.size = t.root.total_bytes;
        }
    }
    // Containers are "used" by a volume when it is listed in their mounts.
    for c in &mut r.containers {
        let log = format!(
            "/var/lib/containers/storage/overlay-containers/{}/userdata/ctr.log",
            c.id
        );
        if let Ok(m) = std::fs::symlink_metadata(&log) {
            use std::os::unix::fs::MetadataExt;
            c.log_bytes = m.blocks() * 512;
            c.log_path = log;
        }
    }
    Ok(r)
}

/// Collect the full report from Docker and, when installed, Podman.
pub fn collect() -> Result<Report> {
    let docker_report =
        docker(&["system", "df", "-v", "--format", "{{json .}}"]).and_then(|t| parse_df(&t));
    let mut r = match docker_report {
        Ok(r) => r,
        Err(e) if !has("podman") => return Err(e),
        Err(_) => Report::default(),
    };
    for c in &mut r.containers {
        c.runtime = "docker";
    }
    for i in &mut r.images {
        i.runtime = "docker";
    }
    for v in &mut r.volumes {
        v.runtime = "docker";
    }
    // Dangling as Docker means it: with the containerd image store an
    // untagged name is not the same thing, and pruning it frees nothing.
    if let Ok(t) = docker(&["images", "-f", "dangling=true", "-q", "--no-trunc"]) {
        let ids: Vec<&str> = t
            .lines()
            .map(|l| l.trim().trim_start_matches("sha256:"))
            .filter(|l| !l.is_empty())
            .collect();
        for i in &mut r.images {
            let id = i.id.trim_start_matches("sha256:");
            i.dangling =
                !id.is_empty() && ids.iter().any(|d| d.starts_with(id) || id.starts_with(d));
        }
    }
    // Arrival time of each image on this host (LastTagTime), for
    // `image prune --filter until=` which uses it, not the build date.
    if !r.images.is_empty() {
        let ids: Vec<String> = r
            .images
            .iter()
            .filter(|i| !i.id.is_empty())
            .map(|i| i.id.clone())
            .collect();
        let mut a = vec![
            "image".to_string(),
            "inspect".into(),
            "--format".into(),
            "{{.Id}} {{.Metadata.LastTagTime}} {{.Created}}".into(),
        ];
        a.extend(ids);
        let args: Vec<&str> = a.iter().map(String::as_str).collect();
        if let Ok(t) = docker(&args) {
            for l in t.lines() {
                let mut f = l.split_whitespace();
                let (Some(id), Some(d), Some(tm)) = (f.next(), f.next(), f.next()) else {
                    continue;
                };
                let id = id.trim_start_matches("sha256:");
                // "0001-01-01" means never tagged here: fall back to the build date.
                let when = crate::util::parse_rfc3339(&format!("{d}T{tm}Z"))
                    .filter(|t| *t > 0)
                    .or_else(|| {
                        f.find(|x| x.contains('T'))
                            .and_then(crate::util::parse_rfc3339)
                    })
                    .unwrap_or(0);
                for i in r.images.iter_mut().filter(|i| {
                    let x = i.id.trim_start_matches("sha256:");
                    !x.is_empty() && (id.starts_with(x) || x.starts_with(id))
                }) {
                    i.arrived = when;
                }
            }
        }
    }
    // Volume ages, for "unused for 30+ days" (spec A6.24).
    if !r.volumes.is_empty() {
        let mut a = vec!["volume", "inspect", "--format", "{{.Name}} {{.CreatedAt}}"];
        let names: Vec<String> = r.volumes.iter().map(|v| v.name.clone()).collect();
        a.extend(names.iter().map(String::as_str));
        if let Ok(t) = docker(&a) {
            for l in t.lines() {
                if let Some((n, c)) = l.split_once(' ') {
                    if let Some(v) = r.volumes.iter_mut().find(|v| v.name == n) {
                        v.created = crate::util::parse_rfc3339(c).unwrap_or(0);
                    }
                }
            }
        }
    }
    r.root_dir = docker(&["info", "--format", "{{.DockerRootDir}}"])
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "/var/lib/docker".into());
    for c in &mut r.containers {
        let p = format!("{}/containers/{}/{}-json.log", r.root_dir, c.id, c.id);
        if let Ok(m) = std::fs::symlink_metadata(&p) {
            use std::os::unix::fs::MetadataExt;
            c.log_bytes = m.blocks() * 512;
            c.log_path = p;
        }
    }
    // Real folder sizes on disk under the data roots, one level down.
    for root in [r.root_dir.clone(), "/var/lib/containerd".to_string()] {
        if std::path::Path::new(&root).is_dir() {
            if let Ok(t) = crate::walker::walk(&root, 4) {
                for c in &t.root.children {
                    r.on_disk.push((
                        format!("{}/{}", root.trim_end_matches('/'), c.name),
                        c.total_bytes,
                    ));
                }
            }
        }
    }
    if has("podman") {
        if let Ok(p) = collect_podman() {
            r.containers.extend(p.containers);
            r.images.extend(p.images);
            r.volumes.extend(p.volumes);
            if std::path::Path::new("/var/lib/containers/storage").is_dir() {
                if let Ok(t) = crate::walker::walk("/var/lib/containers/storage", 4) {
                    for c in &t.root.children {
                        r.on_disk.push((
                            format!("/var/lib/containers/storage/{}", c.name),
                            c.total_bytes,
                        ));
                    }
                }
            }
        }
    }
    r.on_disk.sort_by_key(|(_, b)| std::cmp::Reverse(*b));
    r.images.sort_by_key(|i| std::cmp::Reverse(i.unique));
    r.containers
        .sort_by_key(|c| std::cmp::Reverse(c.writable + c.log_bytes));
    r.volumes.sort_by_key(|v| std::cmp::Reverse(v.size));
    r.organise();
    Ok(r)
}

/// Cleanup plan. Images and stopped containers need explicit opt-in;
/// volumes are never removed, only listed.
pub fn plan(r: &Report, include_images: bool, include_containers: bool, big_log: u64) -> Vec<Item> {
    let mut items = Vec::new();
    let run = |args: &[&str]| Action::Run {
        program: "docker".into(),
        args: args.iter().map(|s| s.to_string()).collect(),
    };
    if r.build_cache_reclaimable > 0 {
        items.push(Item {
            level: 3,
            what: "Docker build cache older than 7 days".into(),
            action: run(&["builder", "prune", "-f", "--filter", "until=168h"]),
            frees: r.build_cache_reclaimable,
        });
    }
    let dangling: u64 = r
        .images
        .iter()
        .filter(|i| i.dangling && i.containers == 0)
        .map(|i| i.unique)
        .sum();
    if dangling > 0 {
        items.push(Item {
            level: 3,
            what: "dangling (untagged, unused) images".into(),
            action: run(&["image", "prune", "-f"]),
            frees: dangling,
        });
    }
    for c in &r.containers {
        if c.log_bytes >= big_log && !c.log_path.is_empty() {
            items.push(Item {
                level: 3,
                what: format!(
                    "container log of {} is {}; keep last 10 MiB",
                    c.name,
                    crate::util::human(c.log_bytes as i64)
                ),
                action: Action::TailAndTruncate {
                    path: c.log_path.clone().into(),
                    keep: 10 << 20,
                },
                frees: c.log_bytes.saturating_sub(10 << 20),
            });
        }
    }
    if include_images {
        // Only what `prune --filter until=720h` really removes: unused images
        // that arrived on this host 30+ days ago (not built 30+ days ago).
        let now = crate::util::now_secs();
        let unused: u64 = r
            .unused_images()
            .filter(|i| i.arrived > 0 && now - i.arrived >= 30 * 86_400)
            .map(|i| i.unique)
            .sum();
        if unused > 0 {
            items.push(Item {
                level: 4,
                what: "unused images that arrived here 30+ days ago".into(),
                action: run(&["image", "prune", "-a", "-f", "--filter", "until=720h"]),
                frees: unused,
            });
        }
    }
    if include_containers {
        let stopped: u64 = r
            .containers
            .iter()
            .filter(|c| c.state != "running")
            .map(|c| c.writable)
            .sum();
        if stopped > 0 {
            items.push(Item {
                level: 4,
                what: "stopped containers older than 7 days".into(),
                action: run(&["container", "prune", "-f", "--filter", "until=168h"]),
                frees: stopped,
            });
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"Images":[
      {"Containers":"1","CreatedSince":"3 months ago","ID":"sha256:aaa","Repository":"app","SharedSize":"10MB","Size":"31MB","Tag":"v1","UniqueSize":"21MB"},
      {"Containers":"0","CreatedSince":"5 months ago","ID":"sha256:bbbbbbbbbbbbbbbb","Repository":"<none>","SharedSize":"10MB","Size":"15MB","Tag":"<none>","UniqueSize":"5MB"},
      {"Containers":"0","CreatedSince":"1 year ago","ID":"sha256:ccc","Repository":"old","SharedSize":"0B","Size":"1.2GB","Tag":"latest","UniqueSize":"1.2GB"}],
     "Containers":[
      {"ID":"c1","Image":"app:v1","Labels":"com.docker.compose.project=shop,x=y","Names":"shop-web-1","Size":"4.57MB (virtual 35MB)","State":"running","Status":"Up 2 days"},
      {"ID":"c2","Image":"old:latest","Labels":"","Names":"oldjob","Size":"120MB","State":"exited","Status":"Exited (0) 9 days ago"}],
     "Volumes":[
      {"Labels":"com.docker.compose.project=shop,com.docker.compose.volume=db","Links":"1","Name":"shop_db","Size":"2.5GB"},
      {"Labels":"","Links":"0","Name":"orphan","Size":"300MB"}],
     "BuildCache":[{"ID":"b1","InUse":false,"Size":"400MB"},{"ID":"b2","InUse":true,"Size":"100MB"}]}"#;

    #[test]
    fn upperdir_is_read_from_a_containerd_mount_line() {
        let l = "mount -t overlay overlay /x -o workdir=/var/lib/containerd/s/876/work,upperdir=/var/lib/containerd/s/876/fs,lowerdir=/a:/b";
        assert_eq!(
            upperdir_from_mount(l).as_deref(),
            Some("/var/lib/containerd/s/876/fs")
        );
        assert_eq!(upperdir_from_mount("nothing here"), None);
    }

    #[test]
    fn inside_lists_the_biggest_folders_with_advice() {
        let d = std::env::temp_dir().join(format!("dux-inside-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("tmp/cache")).unwrap();
        std::fs::create_dir_all(d.join("etc")).unwrap();
        std::fs::write(d.join("tmp/cache/blob"), vec![1u8; 30 << 20]).unwrap();
        std::fs::write(d.join("etc/conf"), b"x").unwrap();
        let v = inside(d.to_str().unwrap(), 8).unwrap();
        assert_eq!(v[0].0, "/tmp");
        assert!(v.iter().any(|(p, _)| p == "/tmp/cache"));
        let c = Container {
            name: "app".into(),
            writable: 60 << 20,
            ..Container::default()
        };
        let a = layer_advice(&c, &v).unwrap();
        assert!(a.contains("Mount a volume at /tmp/cache"), "{a}");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn podman_json_is_parsed_and_linked() {
        let ps = r#"[{"Names":["web"],"Id":"abc","Image":"docker.io/library/nginx:alpine","ImageID":"img1","State":"running","Status":"Up","Size":{"rwSize":12252},"Mounts":["pvol"],"Labels":{"com.docker.compose.project":"shop"},"Networks":["podman"],"Command":["nginx","-g"],"CreatedAt":"1 hour ago"}]"#;
        let images = r#"[{"Names":["docker.io/library/nginx:alpine"],"Id":"img1","Size":63713880,"SharedSize":0,"Containers":1,"CreatedAt":"2 weeks ago"}]"#;
        let vols = r#"[{"Name":"pvol","Driver":"local","Mountpoint":"/var/lib/containers/storage/volumes/pvol/_data","Labels":{},"MountCount":1}]"#;
        let r = parse_podman(ps, images, vols).unwrap();
        assert_eq!(r.containers[0].runtime, "podman");
        assert_eq!(r.containers[0].writable, 12252);
        assert_eq!(r.users_of_image(0), vec![0]);
        assert_eq!(r.users_of_volume(0), vec![0]);
        assert_eq!(r.stacks()[0].name, "shop");
        assert_eq!(program("podman"), "podman");
        assert_eq!(program(""), "docker");
    }

    #[test]
    fn labels_with_commas_in_values_and_stacks() {
        let l = "com.docker.compose.project=xgm,com.docker.compose.project.config_files=/opt/x/a.yml,/opt/x/b.yml,com.docker.compose.service=db";
        let m = labels_map(l);
        assert_eq!(
            m["com.docker.compose.project.config_files"],
            "/opt/x/a.yml,/opt/x/b.yml"
        );
        assert_eq!(m["com.docker.compose.service"], "db");
        let mut r = Report::default();
        r.images.push(Image {
            dangling: false,
            arrived: 0,
            name: "pg:16".into(),
            unique: 100,
            ..Image::default()
        });
        r.volumes.push(Volume {
            created: 0,
            name: "xgm_data_0123456789abcdef".into(),
            size: 50,
            ..Volume::default()
        });
        r.containers.push(Container {
            name: "db".into(),
            image: "pg:16".into(),
            state: "running".into(),
            project: Some("xgm".into()),
            working_dir: Some("/opt/x".into()),
            mounts: vec!["xgm_data_0123…".into()],
            writable: 7,
            ..Container::default()
        });
        r.containers.push(Container {
            name: "db2".into(),
            image: "pg:16".into(),
            project: Some("xgm".into()),
            ..Container::default()
        });
        assert_eq!(r.users_of_image(0), vec![0, 1]);
        assert_eq!(r.users_of_volume(0), vec![0]);
        let st = r.stacks();
        assert_eq!(st.len(), 1);
        assert_eq!(
            (st[0].running, st[0].image_bytes, st[0].volume_bytes),
            (1, 100, 50),
            "image counted once"
        );
        assert_eq!(st[0].working_dir.as_deref(), Some("/opt/x"));
        // A shortened mount that two volumes share is not attributed to either.
        r.volumes.push(Volume {
            created: 0,
            name: "xgm_data_0123zzzz".into(),
            ..Volume::default()
        });
        assert_eq!(r.volume_for_mount("xgm_data_0123…"), None);
        assert!(r.users_of_volume(0).is_empty() && r.users_of_volume(1).is_empty());
        assert_eq!(r.image_users()[0], vec![0, 1]);
    }

    #[test]
    fn si_sizes() {
        assert_eq!(parse_si("31MB"), 31_000_000);
        assert_eq!(parse_si("12.92kB"), 12_920);
        assert_eq!(parse_si("1.2GB"), 1_200_000_000);
        assert_eq!(parse_si("0B"), 0);
        assert_eq!(parse_si("N/A"), 0);
    }

    #[test]
    fn report_from_df_json() {
        let r = parse_df(SAMPLE).unwrap();
        assert_eq!(r.images.len(), 3);
        assert_eq!(r.images[1].name, "<none> bbbbbbbbbbbb");
        assert_eq!(
            r.images_total(),
            21_000_000 + 5_000_000 + 1_200_000_000 + 10_000_000
        );
        assert_eq!(r.unused_images().count(), 2);
        assert_eq!(r.containers[0].project.as_deref(), Some("shop"));
        assert_eq!(r.containers[0].writable, 4_570_000);
        assert_eq!(r.volumes_total(), 2_800_000_000);
        assert_eq!(
            r.unused_volumes()
                .map(|v| v.name.as_str())
                .collect::<Vec<_>>(),
            vec!["orphan"]
        );
        assert_eq!(
            (r.build_cache, r.build_cache_reclaimable),
            (500_000_000, 400_000_000)
        );
        let p = r.by_project();
        assert_eq!(p["shop"], [21_000_000, 4_570_000, 0, 2_500_000_000]);
        assert_eq!(p["(no compose project)"][3], 300_000_000);
    }

    #[test]
    fn cleanup_plan_is_opt_in_for_images_and_containers_and_never_volumes() {
        let mut r = parse_df(SAMPLE).unwrap();
        // Docker's own dangling filter marks this one (collect() asks Docker).
        for i in r.images.iter_mut().filter(|i| i.name.starts_with("<none>")) {
            i.dangling = true;
        }
        // Arrived here long ago (what prune --filter until= judges by).
        for i in r.images.iter_mut() {
            i.arrived = 1;
        }
        r.containers[0].log_bytes = 500 << 20;
        r.containers[0].log_path = "/var/lib/docker/containers/c1/c1-json.log".into();
        let safe = plan(&r, false, false, 100 << 20);
        let whats: Vec<&str> = safe.iter().map(|i| i.what.as_str()).collect();
        assert_eq!(safe.len(), 3, "{whats:?}");
        assert!(
            whats[0].contains("build cache")
                && whats[1].contains("dangling")
                && whats[2].contains("shop-web-1")
        );
        let all = plan(&r, true, true, 100 << 20);
        assert_eq!(all.len(), 5);
        assert!(
            !all.iter()
                .any(|i| format!("{:?}", i.action).contains("volume")),
            "volumes are never removed"
        );
    }
}

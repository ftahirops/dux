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
fn labels_map(labels: &str) -> BTreeMap<String, String> {
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
    pub name: String,
    pub id: String,
    pub image: String,
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
    pub name: String,
    pub size: u64,
    pub links: u32,
    pub project: Option<String>,
    pub driver: String,
    pub mountpoint: String,
    /// Compose volume name inside its project.
    pub compose_name: Option<String>,
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
            i.name == c.image
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

/// Collect the full report from the running Docker daemon.
pub fn collect() -> Result<Report> {
    let mut r = parse_df(&docker(&["system", "df", "-v", "--format", "{{json .}}"])?)?;
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
    r.on_disk.sort_by_key(|(_, b)| std::cmp::Reverse(*b));
    r.images.sort_by_key(|i| std::cmp::Reverse(i.unique));
    r.containers
        .sort_by_key(|c| std::cmp::Reverse(c.writable + c.log_bytes));
    r.volumes.sort_by_key(|v| std::cmp::Reverse(v.size));
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
        .filter(|i| i.name.starts_with("<none>") && i.containers == 0)
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
        let unused: u64 = r.unused_images().map(|i| i.unique).sum();
        if unused > 0 {
            items.push(Item {
                level: 4,
                what: "images not used by any container, older than 30 days".into(),
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
            name: "pg:16".into(),
            unique: 100,
            ..Image::default()
        });
        r.volumes.push(Volume {
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

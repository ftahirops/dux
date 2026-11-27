//! The organised views of `dux ui`: Docker (stacks, containers, images,
//! volumes, logs, storage, each with a detail page and actions), Logs grouped
//! by what they belong to, Projects grouped by location, the action runner
//! with live progress, and the History of every action dux took.

use super::{hdr, hl, App, Overlay, DOCKER, HISTORY, LOGS, PROJECTS};
use crate::cleanup::{Action, Item};
use crate::docker::{Report, Stack};
use crate::fix::Safety;
use crate::insight::{LogFile, LogKind, Project};
use crate::util::human;
use crossterm::event::KeyCode;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, Gauge, Paragraph, Row, Table, TableState, Tabs, Wrap},
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{channel, Receiver};
use std::time::Instant;

// ---------------------------------------------------------------- state ----

/// Docker sub-views, switched with ← and →.
pub const DSUB: [&str; 7] = [
    "Overview",
    "Stacks",
    "Containers",
    "Images",
    "Volumes",
    "Logs",
    "Storage",
];
const D_OVERVIEW: usize = 0;
pub(crate) const D_STACKS: usize = 1;
pub(crate) const D_CONTAINERS: usize = 2;
pub(crate) const D_IMAGES: usize = 3;
pub(crate) const D_VOLUMES: usize = 4;
const D_LOGS: usize = 5;
pub(crate) const D_STORAGE: usize = 6;

#[derive(Default)]
pub struct State {
    pub dsub: usize,
    dsel: [usize; 7],
    /// An open Docker detail page: (sub-view, resource name). Keyed by name,
    /// never by row, so a rescan that re-sorts the list cannot retarget it.
    pub ddetail: Option<(usize, String)>,
    /// Expanded log groups and project groups (by key).
    log_open: BTreeSet<String>,
    proj_closed: BTreeSet<String>,
    pub job: Option<Job>,
    history: Vec<HistoryRow>,
    history_at: Option<Instant>,
}

// ------------------------------------------------------------ the runner ---

#[derive(Clone, Debug)]
pub enum Step {
    Pending,
    Running,
    Done(i64),
    Failed(String),
}

/// A batch of actions running in the background, reporting each step.
pub struct Job {
    pub title: String,
    pub items: Vec<Item>,
    pub steps: Vec<Step>,
    rx: Receiver<(usize, Step)>,
    started: Instant,
    finished: Option<Instant>,
}

/// The filesystem an action frees space on, for measuring it.
fn fs_of(item: &Item, docker_root: Option<&str>) -> String {
    let p = match &item.action {
        Action::Remove { path }
        | Action::TailAndTruncate { path, .. }
        | Action::RemoveTree { path }
        | Action::RemoveUserPath { path } => {
            // Measure on the folder: the path itself is gone afterwards.
            path.parent()
                .map(|d| d.to_path_buf())
                .unwrap_or_else(|| "/".into())
        }
        Action::Run { program, .. } => std::path::PathBuf::from(match program.as_str() {
            "docker" => docker_root.unwrap_or("/var/lib/docker"),
            "journalctl" => "/var/log/journal",
            "apt-get" | "apt" => "/var/cache/apt",
            _ => "/",
        }),
    };
    let mut p = p.as_path();
    while !p.exists() {
        match p.parent() {
            Some(q) => p = q,
            None => break,
        }
    }
    p.to_string_lossy().into_owned()
}

fn avail(fs: &str) -> u64 {
    crate::sentinel::statvfs(fs).map(|u| u.avail).unwrap_or(0)
}

impl Job {
    /// `docker_root` is Docker's data root, where Docker actions free space.
    pub fn start(title: String, items: Vec<Item>, docker_root: Option<String>) -> Job {
        let (tx, rx) = channel();
        let work = items.clone();
        let t = title.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let (mut ok, mut failed, mut freed) = (0u64, 0u64, 0i64);
            for (i, item) in work.iter().enumerate() {
                let _ = tx.send((i, Step::Running));
                let fs = fs_of(item, docker_root.as_deref());
                let before = avail(&fs);
                let step = match crate::cleanup::apply(item) {
                    Ok(()) => {
                        let got = avail(&fs) as i64 - before as i64;
                        ok += 1;
                        freed += got.max(0);
                        Step::Done(got)
                    }
                    Err(e) => {
                        failed += 1;
                        Step::Failed(format!("{e:#}"))
                    }
                };
                let _ = tx.send((i, step));
            }
            crate::cleanup::audit(serde_json::json!({
                "t": crate::util::now_secs(),
                "job": t,
                "applied": ok,
                "failed": failed,
                "freed_measured": freed,
                "seconds": started.elapsed().as_secs_f64(),
            }));
        });
        Job {
            title,
            steps: vec![Step::Pending; items.len()],
            items,
            rx,
            started: Instant::now(),
            finished: None,
        }
    }

    pub fn poll(&mut self) {
        while let Ok((i, s)) = self.rx.try_recv() {
            if let Some(slot) = self.steps.get_mut(i) {
                *slot = s;
            }
        }
        if self.finished.is_none()
            && self
                .steps
                .iter()
                .all(|s| matches!(s, Step::Done(_) | Step::Failed(_)))
        {
            self.finished = Some(Instant::now());
        }
    }

    pub fn done(&self) -> bool {
        self.finished.is_some()
    }

    /// JSON for the web dashboard's progress window.
    pub fn to_json(&self) -> serde_json::Value {
        let (ok, failed, freed) = self.counts();
        let elapsed = self
            .finished
            .unwrap_or_else(Instant::now)
            .duration_since(self.started)
            .as_secs_f64();
        let steps: Vec<serde_json::Value> = self
            .items
            .iter()
            .zip(&self.steps)
            .map(|(it, st)| {
                let (state, freed, error) = match st {
                    Step::Pending => ("pending", 0, None),
                    Step::Running => ("running", 0, None),
                    Step::Done(b) => ("done", (*b).max(0), None),
                    Step::Failed(e) => ("failed", 0, Some(e.clone())),
                };
                serde_json::json!({"what": it.what, "expected": it.frees, "state": state, "freed": freed, "error": error})
            })
            .collect();
        serde_json::json!({"title": self.title, "done": self.done(), "ok": ok, "failed": failed, "freed": freed,
            "expected": self.items.iter().map(|i| i.frees).sum::<u64>(), "seconds": elapsed, "steps": steps})
    }

    fn counts(&self) -> (usize, usize, i64) {
        let mut c = (0, 0, 0i64);
        for s in &self.steps {
            match s {
                Step::Done(b) => {
                    c.0 += 1;
                    c.2 += (*b).max(0);
                }
                Step::Failed(_) => c.1 += 1,
                _ => {}
            }
        }
        c
    }
}

pub fn draw_job(f: &mut Frame, job: &Job, area: Rect) {
    let w = area.width.saturating_sub(8).min(120);
    let want = u16::try_from(job.items.len() + 9)
        .unwrap_or(u16::MAX)
        .max(10);
    let h = want
        .min(area.height.saturating_sub(2))
        .max(1)
        .min(area.height);
    let r = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, r);
    let (ok, failed, freed) = job.counts();
    let total = job.items.len().max(1);
    let expected: u64 = job.items.iter().map(|i| i.frees).sum();
    let elapsed = job
        .finished
        .unwrap_or_else(Instant::now)
        .duration_since(job.started)
        .as_secs_f64();
    let title = if job.done() {
        format!(
            " {}: finished in {elapsed:.1}s · Enter or Esc to close ",
            job.title
        )
    } else {
        format!(" {}: running {elapsed:.0}s ", job.title)
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(r);
    f.render_widget(block, r);
    let parts = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
    ])
    .split(inner);
    f.render_widget(
        Gauge::default()
            .gauge_style(Style::default().fg(if failed > 0 {
                Color::Yellow
            } else {
                Color::Green
            }))
            .ratio((ok + failed) as f64 / total as f64)
            .label(format!("{}/{} done", ok + failed, total)),
        parts[0],
    );
    f.render_widget(
        Paragraph::new(format!(
            "{ok} done · {failed} failed · freed {} measured on disk (expected about {})",
            human(freed),
            human(expected as i64)
        )),
        parts[1],
    );
    let rows: Vec<Row> = job
        .items
        .iter()
        .zip(&job.steps)
        .map(|(it, s)| {
            let (mark, note, c) = match s {
                Step::Pending => ("  ", String::new(), Color::DarkGray),
                Step::Running => ("..", "running".into(), Color::Cyan),
                Step::Done(b) => ("OK", format!("freed {}", human((*b).max(0))), Color::Green),
                Step::Failed(e) => ("!!", e.clone(), Color::Red),
            };
            Row::new(vec![
                mark.to_string(),
                human(it.frees as i64),
                it.what.clone(),
                note,
            ])
            .style(Style::default().fg(c))
        })
        .collect();
    f.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(3),
                Constraint::Length(11),
                Constraint::Percentage(55),
                Constraint::Min(10),
            ],
        )
        .header(hdr(&["", "expected", "what", "result"])),
        parts[2],
    );
}

// --------------------------------------------------------------- docker ----

fn docker_rows(r: &Report, sub: usize) -> usize {
    match sub {
        D_OVERVIEW => 0,
        D_STACKS => r.stacks().len(),
        D_CONTAINERS => r.containers.len(),
        D_IMAGES => r.images.len(),
        D_VOLUMES => r.volumes.len(),
        D_LOGS => r.containers.iter().filter(|c| c.log_bytes > 0).count(),
        _ => r.on_disk.len(),
    }
}

fn log_containers(r: &Report) -> Vec<usize> {
    let mut v: Vec<usize> = (0..r.containers.len())
        .filter(|&i| r.containers[i].log_bytes > 0)
        .collect();
    v.sort_by_key(|&i| std::cmp::Reverse(r.containers[i].log_bytes));
    v
}

fn stack_of(c: &crate::docker::Container) -> String {
    c.project.clone().unwrap_or_else(|| "-".into())
}

fn truncate_log(c: &crate::docker::Container) -> Option<Item> {
    (!c.log_path.is_empty() && c.log_bytes > 10 << 20).then(|| Item {
        level: 2,
        what: format!(
            "truncate the log of container {} (keep its last 10 MiB aside)",
            c.name
        ),
        action: Action::TailAndTruncate {
            path: c.log_path.clone().into(),
            keep: 10 << 20,
        },
        frees: c.log_bytes.saturating_sub(10 << 20),
    })
}

fn docker_run(what: String, args: &[&str], frees: u64) -> Item {
    Item {
        level: 3,
        what,
        action: Action::Run {
            program: "docker".into(),
            args: args.iter().map(|s| s.to_string()).collect(),
        },
        frees,
    }
}

fn draw_docker_overview(f: &mut Frame, r: &Report, area: Rect) {
    let running = r.containers.iter().filter(|c| c.running()).count();
    let unused_img: u64 = r.unused_images().map(|i| i.unique).sum();
    let unused_vol: u64 = r.unused_volumes().map(|v| v.size).sum();
    let stopped_w: u64 = r
        .containers
        .iter()
        .filter(|c| !c.running())
        .map(|c| c.writable)
        .sum();
    let total =
        r.images_total() + r.writable_total() + r.logs_total() + r.volumes_total() + r.build_cache;
    let row = |k: &str, b: u64, note: String| {
        Row::new(vec![
            k.to_string(),
            human(b as i64),
            format!("{:>5.1}%", b as f64 * 100.0 / total.max(1) as f64),
            note,
        ])
    };
    let rows = vec![
        row(
            "Images",
            r.images_total(),
            format!(
                "{} images · {} unused, {} reclaimable",
                r.images.len(),
                r.unused_images().count(),
                human(unused_img as i64)
            ),
        ),
        row(
            "Containers (writable layers)",
            r.writable_total(),
            format!(
                "{} running, {} stopped · stopped hold {}",
                running,
                r.containers.len() - running,
                human(stopped_w as i64)
            ),
        ),
        row(
            "Container logs",
            r.logs_total(),
            format!(
                "{} containers log; biggest {}",
                log_containers(r).len(),
                log_containers(r)
                    .first()
                    .map(|&i| r.containers[i].name.clone())
                    .unwrap_or_default()
            ),
        ),
        row(
            "Volumes",
            r.volumes_total(),
            format!(
                "{} volumes · {} unused ({})",
                r.volumes.len(),
                r.unused_volumes().count(),
                human(unused_vol as i64)
            ),
        ),
        row(
            "Build cache",
            r.build_cache,
            format!("{} reclaimable", human(r.build_cache_reclaimable as i64)),
        ),
    ];
    let parts = Layout::vertical([Constraint::Length(9), Constraint::Min(4)]).split(area);
    f.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(30),
                Constraint::Length(11),
                Constraint::Length(7),
                Constraint::Min(20),
            ],
        )
        .header(hdr(&["what", "size", "share", "details"]))
        .block(Block::default().borders(Borders::ALL).title(format!(
            " Docker uses {} · data root {} ",
            human(total as i64),
            r.root_dir
        ))),
        parts[0],
    );
    let stacks = r.stacks();
    let srows: Vec<Row> = stacks
        .iter()
        .take(12)
        .map(|s| {
            Row::new(vec![
                s.name.clone(),
                format!("{}/{}", s.running, s.containers.len()),
                human(s.total() as i64),
                s.working_dir.clone().unwrap_or_default(),
            ])
        })
        .collect();
    f.render_widget(
        Table::new(
            srows,
            [
                Constraint::Length(30),
                Constraint::Length(8),
                Constraint::Length(11),
                Constraint::Min(20),
            ],
        )
        .header(hdr(&["stack", "running", "uses", "source folder"]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Biggest stacks (→ Stacks for details) "),
        ),
        parts[1],
    );
}

fn draw_docker_list(f: &mut Frame, r: &Report, sub: usize, area: Rect, state: &mut TableState) {
    let block = |t: String| Block::default().borders(Borders::ALL).title(t);
    match sub {
        D_STACKS => {
            let st = r.stacks();
            let rows: Vec<Row> = st
                .iter()
                .map(|s| {
                    Row::new(vec![
                        s.name.clone(),
                        format!("{}/{}", s.running, s.containers.len()),
                        human(s.total() as i64),
                        human(s.image_bytes as i64),
                        human(s.writable as i64),
                        human(s.logs as i64),
                        human(s.volume_bytes as i64),
                        s.working_dir.clone().unwrap_or_else(|| "-".into()),
                    ])
                })
                .collect();
            f.render_stateful_widget(
                Table::new(
                    rows,
                    [
                        Constraint::Length(26),
                        Constraint::Length(8),
                        Constraint::Length(11),
                        Constraint::Length(11),
                        Constraint::Length(11),
                        Constraint::Length(11),
                        Constraint::Length(11),
                        Constraint::Min(16),
                    ],
                )
                .header(hdr(&[
                    "stack", "running", "total", "images", "writable", "logs", "volumes", "source",
                ]))
                .block(block(format!(
                    " {} compose stacks · Enter: stack page · t: truncate its big logs ",
                    st.len()
                )))
                .row_highlight_style(hl()),
                area,
                state,
            );
        }
        D_CONTAINERS => {
            let rows: Vec<Row> = r
                .containers
                .iter()
                .map(|c| {
                    let st = if c.running() {
                        Color::Green
                    } else {
                        Color::Yellow
                    };
                    Row::new(vec![
                        c.name.clone(),
                        c.state.clone(),
                        stack_of(c),
                        human((c.writable + c.log_bytes) as i64),
                        human(c.writable as i64),
                        human(c.log_bytes as i64),
                        c.mounts.len().to_string(),
                        c.image.clone(),
                    ])
                    .style(Style::default().fg(st))
                })
                .collect();
            f.render_stateful_widget(
                Table::new(
                    rows,
                    [
                        Constraint::Length(26),
                        Constraint::Length(9),
                        Constraint::Length(16),
                        Constraint::Length(11),
                        Constraint::Length(11),
                        Constraint::Length(11),
                        Constraint::Length(7),
                        Constraint::Min(16),
                    ],
                )
                .header(hdr(&[
                    "container",
                    "state",
                    "stack",
                    "uses",
                    "writable",
                    "log",
                    "mounts",
                    "image",
                ]))
                .block(block(format!(
                    " {} containers · Enter: details · t: truncate log · x: remove (stopped only) ",
                    r.containers.len()
                )))
                .row_highlight_style(hl()),
                area,
                state,
            );
        }
        D_IMAGES => {
            let image_users = r.image_users();
            let rows: Vec<Row> = r
                .images
                .iter()
                .enumerate()
                .map(|(i, im)| {
                    let users: Vec<String> = image_users[i]
                        .iter()
                        .map(|&c| r.containers[c].name.clone())
                        .collect();
                    let status = if users.is_empty() && im.containers == 0 {
                        "UNUSED".to_string()
                    } else {
                        format!("{} container(s)", users.len().max(im.containers as usize))
                    };
                    Row::new(vec![
                        im.name.clone(),
                        human(im.size as i64),
                        human(im.unique as i64),
                        human(im.shared as i64),
                        status,
                        im.created.clone(),
                        users.join(", "),
                    ])
                    .style(Style::default().fg(
                        if users.is_empty() && im.containers == 0 {
                            Color::Yellow
                        } else {
                            Color::Reset
                        },
                    ))
                })
                .collect();
            f.render_stateful_widget(
                Table::new(rows, [Constraint::Length(40), Constraint::Length(10), Constraint::Length(10), Constraint::Length(10), Constraint::Length(14), Constraint::Length(14), Constraint::Min(16)])
                    .header(hdr(&["image", "size", "unique", "shared", "used by", "created", "containers"]))
                    .block(block(format!(" {} images, {} on disk (shared layers once) · Enter: details · x: remove unused ", r.images.len(), human(r.images_total() as i64))))
                    .row_highlight_style(hl()),
                area,
                state,
            );
        }
        D_VOLUMES => {
            let rows: Vec<Row> = r
                .volumes
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let users: Vec<String> = r
                        .users_of_volume(i)
                        .iter()
                        .map(|&c| r.containers[c].name.clone())
                        .collect();
                    let unused = v.links == 0 && users.is_empty();
                    Row::new(vec![
                        v.compose_name
                            .clone()
                            .unwrap_or_else(|| v.name.chars().take(28).collect()),
                        human(v.size as i64),
                        v.project.clone().unwrap_or_else(|| "-".into()),
                        if unused {
                            "UNUSED".into()
                        } else {
                            users.join(", ")
                        },
                        v.mountpoint.clone(),
                    ])
                    .style(Style::default().fg(if unused {
                        Color::Yellow
                    } else {
                        Color::Reset
                    }))
                })
                .collect();
            f.render_stateful_widget(
                Table::new(
                    rows,
                    [
                        Constraint::Length(30),
                        Constraint::Length(11),
                        Constraint::Length(16),
                        Constraint::Length(30),
                        Constraint::Min(16),
                    ],
                )
                .header(hdr(&["volume", "size", "stack", "used by", "on disk"]))
                .block(block(format!(
                    " {} volumes, {} · Enter: details · x: remove unused (RISKY) ",
                    r.volumes.len(),
                    human(r.volumes_total() as i64)
                )))
                .row_highlight_style(hl()),
                area,
                state,
            );
        }
        D_LOGS => {
            let rows: Vec<Row> = log_containers(r)
                .iter()
                .map(|&i| {
                    let c = &r.containers[i];
                    Row::new(vec![
                        human(c.log_bytes as i64),
                        c.name.clone(),
                        stack_of(c),
                        c.state.clone(),
                        c.log_path.clone(),
                    ])
                })
                .collect();
            f.render_stateful_widget(
                Table::new(
                    rows,
                    [
                        Constraint::Length(11),
                        Constraint::Length(26),
                        Constraint::Length(16),
                        Constraint::Length(9),
                        Constraint::Min(20),
                    ],
                )
                .header(hdr(&["size", "container", "stack", "state", "log file"]))
                .block(block(format!(
                    " Container logs: {} · t: truncate (keeps the last 10 MiB aside) ",
                    human(r.logs_total() as i64)
                )))
                .row_highlight_style(hl()),
                area,
                state,
            );
        }
        _ => {
            let explain = |p: &str| -> &str {
                if p.ends_with("/overlay2") {
                    "image layers and container writable layers"
                } else if p.ends_with("/volumes") {
                    "named and anonymous volumes"
                } else if p.ends_with("/containers") {
                    "container config and json logs"
                } else if p.ends_with("/buildkit") {
                    "build cache"
                } else if p.contains("containerd") && p.contains("snapshot") {
                    "containerd snapshots (image layers)"
                } else if p.contains("containerd") && p.contains("content") {
                    "containerd content store (downloaded layers)"
                } else if p.ends_with("/image") {
                    "image metadata"
                } else {
                    ""
                }
            };
            let rows: Vec<Row> = r
                .on_disk
                .iter()
                .map(|(p, b)| Row::new(vec![human(*b as i64), p.clone(), explain(p).to_string()]))
                .collect();
            f.render_stateful_widget(
                Table::new(rows, [Constraint::Length(11), Constraint::Percentage(50), Constraint::Min(20)])
                    .header(hdr(&["size", "folder", "what it holds"]))
                    .block(block(format!(" Docker and containerd storage on disk · build cache {} ({} reclaimable) · b: prune build cache ", human(r.build_cache as i64), human(r.build_cache_reclaimable as i64))))
                    .row_highlight_style(hl()),
                area,
                state,
            );
        }
    }
}

fn kv(k: &str, v: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{k:<12}"), Style::default().fg(Color::Cyan)),
        Span::raw(v),
    ])
}

fn draw_stack_page(f: &mut Frame, r: &Report, s: &Stack, area: Rect) {
    let parts = Layout::vertical([
        Constraint::Length(8),
        Constraint::Min(5),
        Constraint::Length(8),
    ])
    .split(area);
    let info = vec![
        kv("stack", s.name.clone()),
        kv(
            "source",
            s.working_dir
                .clone()
                .unwrap_or_else(|| "- (not started by compose)".into()),
        ),
        kv(
            "compose",
            if s.config_files.is_empty() {
                "-".into()
            } else {
                s.config_files.join("  ")
            },
        ),
        kv(
            "containers",
            format!("{} ({} running)", s.containers.len(), s.running),
        ),
        kv(
            "uses",
            format!(
                "{} total: images {} · writable {} · logs {} · volumes {}",
                human(s.total() as i64),
                human(s.image_bytes as i64),
                human(s.writable as i64),
                human(s.logs as i64),
                human(s.volume_bytes as i64)
            ),
        ),
        Line::from("keys  t truncate this stack's big logs · Esc back")
            .style(Style::default().fg(Color::DarkGray)),
    ];
    f.render_widget(
        Paragraph::new(info).block(Block::default().borders(Borders::ALL).title(" Stack ")),
        parts[0],
    );
    let crow: Vec<Row> = s
        .containers
        .iter()
        .map(|&i| {
            let c = &r.containers[i];
            Row::new(vec![
                c.service.clone().unwrap_or_else(|| c.name.clone()),
                c.name.clone(),
                c.status.clone(),
                human(c.writable as i64),
                human(c.log_bytes as i64),
                c.image.clone(),
                c.ports.clone(),
            ])
        })
        .collect();
    f.render_widget(
        Table::new(
            crow,
            [
                Constraint::Length(16),
                Constraint::Length(24),
                Constraint::Length(22),
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Length(30),
                Constraint::Min(10),
            ],
        )
        .header(hdr(&[
            "service",
            "container",
            "status",
            "writable",
            "log",
            "image",
            "ports",
        ]))
        .block(Block::default().borders(Borders::ALL).title(" Services ")),
        parts[1],
    );
    let bottom = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(parts[2]);
    let irows: Vec<Row> = s
        .images
        .iter()
        .map(|&i| {
            Row::new(vec![
                human(r.images[i].unique as i64),
                r.images[i].name.clone(),
            ])
        })
        .collect();
    f.render_widget(
        Table::new(irows, [Constraint::Length(10), Constraint::Min(10)])
            .header(hdr(&["unique", "image"]))
            .block(Block::default().borders(Borders::ALL).title(" Images ")),
        bottom[0],
    );
    let vrows: Vec<Row> = s
        .volumes
        .iter()
        .map(|&v| {
            Row::new(vec![
                human(r.volumes[v].size as i64),
                r.volumes[v]
                    .compose_name
                    .clone()
                    .unwrap_or_else(|| r.volumes[v].name.clone()),
            ])
        })
        .collect();
    f.render_widget(
        Table::new(vrows, [Constraint::Length(10), Constraint::Min(10)])
            .header(hdr(&["size", "volume"]))
            .block(Block::default().borders(Borders::ALL).title(" Volumes ")),
        bottom[1],
    );
}

fn draw_docker_detail(f: &mut Frame, r: &Report, sub: usize, i: usize, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    let title;
    match sub {
        D_STACKS => {
            if let Some(s) = r.stacks().get(i) {
                draw_stack_page(f, r, s, area);
            }
            return;
        }
        D_CONTAINERS | D_LOGS => {
            let ci = if sub == D_LOGS {
                log_containers(r).get(i).copied()
            } else {
                Some(i)
            };
            let Some(c) = ci.and_then(|ci| r.containers.get(ci)) else {
                return;
            };
            title = format!(" Container {} ", c.name);
            lines.push(kv("name", c.name.clone()));
            lines.push(kv("id", c.id.clone()));
            lines.push(kv(
                "state",
                format!(
                    "{} · {} {}",
                    c.state,
                    c.status,
                    if c.health.is_empty() {
                        String::new()
                    } else {
                        format!("· health {}", c.health)
                    }
                ),
            ));
            lines.push(kv(
                "stack",
                format!(
                    "{} · service {}",
                    stack_of(c),
                    c.service.clone().unwrap_or_else(|| "-".into())
                ),
            ));
            lines.push(kv(
                "source",
                c.working_dir.clone().unwrap_or_else(|| "-".into()),
            ));
            lines.push(kv("image", c.image.clone()));
            lines.push(kv("command", c.command.clone()));
            lines.push(kv("created", format!("{} ({})", c.created, c.running_for)));
            lines.push(kv(
                "ports",
                if c.ports.is_empty() {
                    "-".into()
                } else {
                    c.ports.clone()
                },
            ));
            lines.push(kv("networks", c.networks.join(", ")));
            lines.push(kv(
                "disk",
                format!(
                    "writable layer {} · log {} · total {}",
                    human(c.writable as i64),
                    human(c.log_bytes as i64),
                    human((c.writable + c.log_bytes) as i64)
                ),
            ));
            lines.push(kv(
                "log file",
                if c.log_path.is_empty() {
                    "-".into()
                } else {
                    c.log_path.clone()
                },
            ));
            lines.push(Line::from(""));
            lines.push(Line::from("mounts").style(Style::default().add_modifier(Modifier::BOLD)));
            for m in &c.mounts {
                let vol = r.volume_for_mount(m).map(|v| &r.volumes[v]);
                lines.push(Line::from(match vol {
                    Some(v) => format!(
                        "  volume {}  {}  {}",
                        v.compose_name.clone().unwrap_or_else(|| v.name.clone()),
                        human(v.size as i64),
                        v.mountpoint
                    ),
                    None => format!("  {m}"),
                }));
            }
            lines.push(Line::from(""));
            lines.push(
                Line::from(format!(
                    "next   docker logs --tail 50 {} · docker inspect {}",
                    c.name, c.name
                ))
                .style(Style::default().fg(Color::Cyan)),
            );
            lines.push(
                Line::from("keys   t truncate log · x remove (stopped only) · Esc back")
                    .style(Style::default().fg(Color::DarkGray)),
            );
        }
        D_IMAGES => {
            let Some(im) = r.images.get(i) else { return };
            title = format!(" Image {} ", im.name);
            lines.push(kv("image", im.name.clone()));
            lines.push(kv("id", im.id.clone()));
            lines.push(kv(
                "size",
                format!(
                    "{} · unique {} · shared with other images {}",
                    human(im.size as i64),
                    human(im.unique as i64),
                    human(im.shared as i64)
                ),
            ));
            lines.push(kv("created", im.created.clone()));
            lines.push(Line::from(""));
            let users = r.users_of_image(i);
            lines.push(
                Line::from(format!("used by {} container(s)", users.len()))
                    .style(Style::default().add_modifier(Modifier::BOLD)),
            );
            for c in users {
                let c = &r.containers[c];
                lines.push(Line::from(format!(
                    "  {}  {}  stack {}",
                    c.name,
                    c.state,
                    stack_of(c)
                )));
            }
            lines.push(Line::from(""));
            lines.push(
                Line::from(
                    "removing an unused image frees its unique size; docker pull brings it back",
                )
                .style(Style::default().fg(Color::DarkGray)),
            );
            lines.push(
                Line::from("keys   x remove (unused only) · Esc back")
                    .style(Style::default().fg(Color::DarkGray)),
            );
        }
        D_VOLUMES => {
            let Some(v) = r.volumes.get(i) else { return };
            title = format!(
                " Volume {} ",
                v.compose_name.clone().unwrap_or_else(|| v.name.clone())
            );
            lines.push(kv("name", v.name.clone()));
            lines.push(kv(
                "stack",
                format!(
                    "{} · compose volume {}",
                    v.project.clone().unwrap_or_else(|| "-".into()),
                    v.compose_name.clone().unwrap_or_else(|| "-".into())
                ),
            ));
            lines.push(kv("size", human(v.size as i64)));
            lines.push(kv("driver", v.driver.clone()));
            lines.push(kv("on disk", v.mountpoint.clone()));
            lines.push(Line::from(""));
            let users = r.users_of_volume(i);
            lines.push(
                Line::from(format!(
                    "mounted by {} container(s) (docker counts {} links)",
                    users.len(),
                    v.links
                ))
                .style(Style::default().add_modifier(Modifier::BOLD)),
            );
            for c in users {
                let c = &r.containers[c];
                lines.push(Line::from(format!("  {}  {}", c.name, c.state)));
            }
            lines.push(Line::from(""));
            lines.push(
                Line::from(
                    "RISKY: a volume can hold a database or uploads; removing it cannot be undone",
                )
                .style(Style::default().fg(Color::Red)),
            );
            lines.push(
                Line::from("keys   x remove (unused only, confirm with D) · Esc back")
                    .style(Style::default().fg(Color::DarkGray)),
            );
        }
        _ => return,
    }
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

pub(super) fn draw_docker(f: &mut Frame, app: &App, area: Rect, state: &mut TableState) {
    let Some(r) = &app.data.docker else {
        f.render_widget(
            Paragraph::new("Docker is not available on this host.")
                .block(Block::default().borders(Borders::ALL)),
            area,
        );
        return;
    };
    let parts = Layout::vertical([Constraint::Length(1), Constraint::Min(4)]).split(area);
    f.render_widget(
        Tabs::new(DSUB.to_vec())
            .select(app.v.dsub)
            .highlight_style(Style::default().fg(Color::Black).bg(Color::Yellow))
            .divider("│"),
        parts[0],
    );
    if let Some((sub, key)) = &app.v.ddetail {
        match docker_index(r, *sub, key) {
            Some(i) => draw_docker_detail(f, r, *sub, i, parts[1]),
            None => f.render_widget(
                Paragraph::new(format!(
                    "{key} is no longer there (it changed in the last scan). Esc to go back."
                ))
                .block(Block::default().borders(Borders::ALL)),
                parts[1],
            ),
        }
        return;
    }
    if app.v.dsub == D_OVERVIEW {
        draw_docker_overview(f, r, parts[1]);
    } else {
        draw_docker_list(f, r, app.v.dsub, parts[1], state);
    }
}

/// The stable name of row `i` in a Docker sub-view.
fn docker_key(r: &Report, sub: usize, i: usize) -> Option<String> {
    match sub {
        D_STACKS => r.stacks().get(i).map(|s| s.name.clone()),
        D_CONTAINERS => r.containers.get(i).map(|c| c.name.clone()),
        D_LOGS => log_containers(r)
            .get(i)
            .map(|&c| r.containers[c].name.clone()),
        D_IMAGES => r.images.get(i).map(|x| x.name.clone()),
        D_VOLUMES => r.volumes.get(i).map(|x| x.name.clone()),
        _ => None,
    }
}

/// The current row of the resource named `key` in a Docker sub-view.
fn docker_index(r: &Report, sub: usize, key: &str) -> Option<usize> {
    match sub {
        D_STACKS => r.stacks().iter().position(|s| s.name == key),
        D_CONTAINERS => r.containers.iter().position(|c| c.name == key),
        D_LOGS => log_containers(r)
            .iter()
            .position(|&c| r.containers[c].name == key),
        D_IMAGES => r.images.iter().position(|x| x.name == key),
        D_VOLUMES => r.volumes.iter().position(|x| x.name == key),
        _ => None,
    }
}

/// The action for a key on the Docker views, if any.
pub(crate) fn docker_action(
    r: &Report,
    sub: usize,
    i: usize,
    code: KeyCode,
) -> Result<Option<(Vec<Item>, bool)>, String> {
    let c_at = |i: usize| -> Option<&crate::docker::Container> {
        if sub == D_LOGS {
            log_containers(r).get(i).map(|&c| &r.containers[c])
        } else {
            r.containers.get(i)
        }
    };
    match (sub, code) {
        (D_CONTAINERS | D_LOGS, KeyCode::Char('t')) => {
            let c = c_at(i).ok_or("no container")?;
            Ok(Some((
                vec![truncate_log(c).ok_or(format!("the log of {} is under 10 MiB", c.name))?],
                false,
            )))
        }
        (D_STACKS, KeyCode::Char('t')) => {
            let st = r.stacks();
            let s = st.get(i).ok_or("no stack")?;
            let items: Vec<Item> = s
                .containers
                .iter()
                .filter_map(|&c| truncate_log(&r.containers[c]))
                .collect();
            if items.is_empty() {
                return Err(format!("no log over 10 MiB in stack {}", s.name));
            }
            Ok(Some((items, false)))
        }
        (D_CONTAINERS, KeyCode::Char('x')) => {
            let c = c_at(i).ok_or("no container")?;
            if c.running() {
                return Err(format!(
                    "{} is running; stop it first (docker stop {})",
                    c.name, c.name
                ));
            }
            // Its writable layer may hold the only copy of data: RISKY.
            Ok(Some((
                vec![docker_run(
                    format!(
                        "remove stopped container {} and files written inside it",
                        c.name
                    ),
                    &["rm", &c.id],
                    c.writable + c.log_bytes,
                )],
                true,
            )))
        }
        (D_IMAGES, KeyCode::Char('x')) => {
            let im = r.images.get(i).ok_or("no image")?;
            if im.containers > 0 || !r.users_of_image(i).is_empty() {
                return Err(format!("{} is used by a container", im.name));
            }
            // A tag is removed by name (removing by ID fails when the image
            // has several tags, and would remove all of them); only an
            // untagged image is removed by its ID.
            let id = if im.name.starts_with("<none>") && !im.id.is_empty() {
                im.id.clone()
            } else {
                im.name.clone()
            };
            Ok(Some((
                vec![docker_run(
                    format!("remove unused image {}", im.name),
                    &["image", "rm", &id],
                    im.unique,
                )],
                false,
            )))
        }
        (D_VOLUMES, KeyCode::Char('x')) => {
            let v = r.volumes.get(i).ok_or("no volume")?;
            if v.links > 0 || !r.users_of_volume(i).is_empty() {
                return Err(format!("volume {} is used by a container", v.name));
            }
            Ok(Some((
                vec![docker_run(
                    format!("delete unused volume {} (data is lost)", v.name),
                    &["volume", "rm", &v.name],
                    v.size,
                )],
                true,
            )))
        }
        (D_STORAGE, KeyCode::Char('b')) => {
            if r.build_cache_reclaimable == 0 {
                return Err("no reclaimable build cache".into());
            }
            Ok(Some((
                vec![docker_run(
                    "prune Docker build cache unused for 7 days".into(),
                    &["builder", "prune", "-f", "--filter", "until=168h"],
                    r.build_cache_reclaimable,
                )],
                false,
            )))
        }
        _ => Ok(None),
    }
}

// ----------------------------------------------------------------- logs ----

/// What a log belongs to: (category, owner).
pub fn log_owner(l: &LogFile, projects: &[Project]) -> (&'static str, String) {
    let p = l.path.as_str();
    if l.kind == LogKind::Docker {
        return (
            "Docker container",
            l.container.clone().unwrap_or_else(|| "container".into()),
        );
    }
    if p.contains("/journal/") {
        return ("Journal", "systemd-journald".into());
    }
    if let Some(proj) = projects
        .iter()
        .find(|x| p.starts_with(&format!("{}/", x.path)))
    {
        return ("App", proj.name.clone());
    }
    if let Some(rest) = p.strip_prefix("/var/log/") {
        let first = rest.split('/').next().unwrap_or(rest);
        let base = first.split(['.', '-']).next().unwrap_or(first);
        const WEB: &[&str] = &[
            "nginx", "apache2", "httpd", "caddy", "traefik", "haproxy", "lighttpd", "varnish",
        ];
        const DB: &[&str] = &[
            "mysql",
            "mariadb",
            "postgresql",
            "mongodb",
            "redis",
            "clickhouse-server",
            "clickhouse",
            "elasticsearch",
            "opensearch",
            "rabbitmq",
            "kafka",
        ];
        const OS: &[&str] = &[
            "syslog",
            "messages",
            "kern",
            "auth",
            "secure",
            "dpkg",
            "apt",
            "installer",
            "cloud-init",
            "cloud-init-output",
            "unattended-upgrades",
            "audit",
            "sysstat",
            "btmp",
            "wtmp",
            "lastlog",
            "faillog",
            "boot",
            "dmesg",
            "alternatives",
            "ufw",
            "fail2ban",
            "private",
            "landscape",
            "apport",
            "sssd",
            "cron",
            "maillog",
            "mail",
            "user",
            "daemon",
            "debug",
            "ubuntu-advantage",
            "dist-upgrade",
            "fontconfig",
            "bootstrap",
            "yum",
            "dnf",
            "anaconda",
            "tuned",
            "chrony",
            "README",
        ];
        if WEB.contains(&base) || WEB.contains(&first) {
            return ("Web server", first.to_string());
        }
        if DB.contains(&base) || DB.contains(&first) {
            return ("Database", first.to_string());
        }
        if OS.contains(&base) {
            return ("Operating system", base.to_string());
        }
        return (
            "App",
            if rest.contains('/') {
                first.to_string()
            } else {
                base.to_string()
            },
        );
    }
    let parts: Vec<&str> = p.trim_start_matches('/').split('/').collect();
    let owner = match parts.as_slice() {
        ["opt" | "srv", name, ..] => name.to_string(),
        ["home", user, ..] => format!("home of {user}"),
        ["root", ..] => "home of root".into(),
        ["var", "lib", name, ..] => name.to_string(),
        _ => parts
            .get(parts.len().saturating_sub(2))
            .unwrap_or(&"other")
            .to_string(),
    };
    ("App", owner)
}

struct LogGroup {
    key: String,
    category: &'static str,
    owner: String,
    files: Vec<usize>,
    total: u64,
    rotated: u64,
    newest: i64,
}

fn log_groups(app: &App) -> Vec<LogGroup> {
    let mut m: BTreeMap<String, LogGroup> = BTreeMap::new();
    for (i, l) in app.data.logs.iter().enumerate() {
        let (cat, owner) = log_owner(l, &app.data.projects);
        let key = format!("{cat}\u{1f}{owner}");
        let g = m.entry(key.clone()).or_insert_with(|| LogGroup {
            key,
            category: cat,
            owner,
            files: Vec::new(),
            total: 0,
            rotated: 0,
            newest: 0,
        });
        g.files.push(i);
        g.total += l.bytes;
        if l.rotated {
            g.rotated += l.bytes;
        }
        g.newest = g.newest.max(l.mtime);
    }
    let mut v: Vec<LogGroup> = m.into_values().collect();
    v.sort_by_key(|g| std::cmp::Reverse(g.total));
    v
}

enum LogRow {
    Group(usize),
    File(usize, usize),
}

fn log_rows(app: &App, groups: &[LogGroup]) -> Vec<LogRow> {
    let mut out = Vec::new();
    for (gi, g) in groups.iter().enumerate() {
        out.push(LogRow::Group(gi));
        if app.v.log_open.contains(&g.key) {
            out.extend(g.files.iter().map(|&f| LogRow::File(gi, f)));
        }
    }
    out
}

/// The last few lines of a text log, bounded and made printable.
pub(crate) fn tail_lines(path: &str, n: usize) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};
    if crate::cleanup::is_binary_log(std::path::Path::new(path))
        || path.ends_with(".gz")
        || path.ends_with(".xz")
        || path.ends_with(".zst")
    {
        return vec!["(compressed or binary; not shown)".into()];
    }
    let Ok(mut f) = std::fs::File::open(path) else {
        return vec!["(cannot open)".into()];
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(4096)));
    let mut b = Vec::new();
    let _ = f.take(4096).read_to_end(&mut b);
    let text = String::from_utf8_lossy(&b);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| crate::insight::clean_text(l, 220))
        .collect()
}

pub(super) fn draw_logs(f: &mut Frame, app: &App, area: Rect, state: &mut TableState) {
    let now = crate::util::now_secs();
    let groups = log_groups(app);
    let rows = log_rows(app, &groups);
    let parts = Layout::vertical([Constraint::Min(6), Constraint::Length(12)]).split(area);
    let table_rows: Vec<Row> = rows
        .iter()
        .map(|r| match r {
            LogRow::Group(gi) => {
                let g = &groups[*gi];
                let open = app.v.log_open.contains(&g.key);
                Row::new(vec![
                    format!("{} {}", if open { "▾" } else { "▸" }, g.category),
                    g.owner.clone(),
                    human(g.total as i64),
                    format!("{} files", g.files.len()),
                    if g.rotated > 0 {
                        format!("{} rotated", human(g.rotated as i64))
                    } else {
                        String::new()
                    },
                    crate::insight::age_text(g.newest, now),
                ])
                .style(Style::default().add_modifier(Modifier::BOLD))
            }
            LogRow::File(_, li) => {
                let l = &app.data.logs[*li];
                let state = if l.protected {
                    "protected"
                } else if l.rotated {
                    "rotated"
                } else {
                    "active"
                };
                Row::new(vec![
                    format!("    {}", l.path.rsplit('/').next().unwrap_or(&l.path)),
                    l.writer.clone().unwrap_or_default(),
                    human(l.bytes as i64),
                    state.to_string(),
                    String::new(),
                    crate::insight::age_text(l.mtime, now),
                ])
                .style(Style::default().fg(if l.rotated {
                    Color::DarkGray
                } else {
                    Color::Reset
                }))
            }
        })
        .collect();
    let total: u64 = app.data.logs.iter().map(|l| l.bytes).sum();
    f.render_stateful_widget(
        Table::new(table_rows, [Constraint::Length(34), Constraint::Length(26), Constraint::Length(11), Constraint::Length(10), Constraint::Length(16), Constraint::Min(5)])
            .header(hdr(&["belongs to", "owner / writer", "size", "files", "rotated", "changed"]))
            .block(Block::default().borders(Borders::ALL).title(format!(
                " Logs: {} in {} files, {} groups · journal {} · Enter open/close · t truncate · r remove rotated · v vacuum journal ",
                human(total as i64),
                app.data.logs.len(),
                groups.len(),
                app.data.journal_bytes.map(|j| human(j as i64)).unwrap_or_else(|| "-".into())
            )))
            .row_highlight_style(hl()),
        parts[0],
        state,
    );
    let sel = app.sel[LOGS].min(rows.len().saturating_sub(1));
    let mut lines: Vec<Line> = Vec::new();
    match rows.get(sel) {
        Some(LogRow::Group(gi)) => {
            let g = &groups[*gi];
            lines.push(kv("group", format!("{} · {}", g.category, g.owner)));
            lines.push(kv(
                "size",
                format!(
                    "{} in {} files, {} of it rotated copies",
                    human(g.total as i64),
                    g.files.len(),
                    human(g.rotated as i64)
                ),
            ));
            let dirs: BTreeSet<&str> = g
                .files
                .iter()
                .map(|&i| {
                    app.data.logs[i]
                        .path
                        .rsplit_once('/')
                        .map_or("", |(d, _)| d)
                })
                .collect();
            lines.push(kv(
                "folders",
                dirs.into_iter().take(4).collect::<Vec<_>>().join("  "),
            ));
            let writers: BTreeSet<String> = g
                .files
                .iter()
                .filter_map(|&i| app.data.logs[i].writer.clone())
                .collect();
            lines.push(kv(
                "written by",
                if writers.is_empty() {
                    "no process has these open now".into()
                } else {
                    writers.into_iter().collect::<Vec<_>>().join(", ")
                },
            ));
            lines.push(
                Line::from("Enter to see its files · r remove its rotated copies")
                    .style(Style::default().fg(Color::DarkGray)),
            );
        }
        Some(LogRow::File(gi, li)) => {
            let l = &app.data.logs[*li];
            let g = &groups[*gi];
            lines.push(kv("file", l.path.clone()));
            lines.push(kv("belongs to", format!("{} · {}", g.category, g.owner)));
            lines.push(kv(
                "size",
                format!(
                    "{} · changed {} ago · {}",
                    human(l.bytes as i64),
                    crate::insight::age_text(l.mtime, now),
                    if l.rotated {
                        "rotated copy"
                    } else {
                        "active log"
                    }
                ),
            ));
            lines.push(kv(
                "writer",
                l.writer
                    .clone()
                    .unwrap_or_else(|| "no process has it open now".into()),
            ));
            lines.push(kv(
                "safety",
                if l.protected {
                    "security or accounting log: dux never touches it".into()
                } else if crate::cleanup::is_binary_log(std::path::Path::new(&l.path)) {
                    "binary journal: shrink with v (journalctl --vacuum-size)".into()
                } else if l.rotated {
                    "SAFE to remove (r)".to_string()
                } else {
                    "CAUTION: t truncates it in place and keeps the last 10 MiB aside".to_string()
                },
            ));
            lines.push(
                Line::from("last lines").style(Style::default().add_modifier(Modifier::BOLD)),
            );
            for t in tail_lines(&l.path, 5) {
                lines.push(Line::from(format!("  {t}")).style(Style::default().fg(Color::Gray)));
            }
        }
        None => lines.push(Line::from("No logs over 1 MiB were found.")),
    }
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" Details ")),
        parts[1],
    );
}

/// Truncate an active text log, keeping its last 10 MiB aside.
pub(crate) fn log_truncate_item(l: &LogFile) -> Result<Item, String> {
    if l.protected || l.rotated || crate::cleanup::is_binary_log(std::path::Path::new(&l.path)) {
        return Err("only active text logs can be truncated".into());
    }
    Ok(Item {
        level: 2,
        what: format!("truncate {} (keep its last 10 MiB aside)", l.path),
        action: Action::TailAndTruncate {
            path: l.path.clone().into(),
            keep: 10 << 20,
        },
        frees: l.bytes.saturating_sub(10 << 20),
    })
}

/// Remove the rotated, unprotected, unopened copies among `logs`.
pub(crate) fn log_remove_items<'a>(
    logs: impl Iterator<Item = &'a LogFile>,
) -> Result<Vec<Item>, String> {
    let items: Vec<Item> = logs
        .filter(|l| l.rotated && !l.protected && l.writer.is_none())
        .map(|l| Item {
            level: 2,
            what: format!("remove rotated log {}", l.path),
            action: Action::Remove {
                path: l.path.clone().into(),
            },
            frees: l.bytes,
        })
        .collect();
    if items.is_empty() {
        return Err("no removable rotated copies here".into());
    }
    Ok(items)
}

pub(crate) fn journal_item(journal: Option<u64>) -> Result<Item, String> {
    match journal.filter(|j| *j > 1 << 30) {
        Some(j) => Ok(Item {
            level: 1,
            what: format!(
                "shrink the systemd journal from {} to 1 GiB",
                human(j as i64)
            ),
            action: Action::Run {
                program: "journalctl".into(),
                args: vec!["--vacuum-size=1G".into()],
            },
            frees: j - (1 << 30),
        }),
        None => Err("the journal is under 1 GiB".into()),
    }
}

fn log_action(app: &App, code: KeyCode) -> Result<Option<(Vec<Item>, bool)>, String> {
    let groups = log_groups(app);
    let rows = log_rows(app, &groups);
    let sel = rows.get(app.sel[LOGS]);
    match code {
        KeyCode::Char('t') => {
            let Some(LogRow::File(_, li)) = sel else {
                return Err("select a file (Enter opens a group)".into());
            };
            Ok(Some((vec![log_truncate_item(&app.data.logs[*li])?], false)))
        }
        KeyCode::Char('r') => {
            let files: Vec<usize> = match sel {
                Some(LogRow::Group(gi)) => groups[*gi].files.clone(),
                Some(LogRow::File(_, li)) => vec![*li],
                None => Vec::new(),
            };
            Ok(Some((
                log_remove_items(files.iter().map(|&i| &app.data.logs[i]))?,
                false,
            )))
        }
        KeyCode::Char('v') => Ok(Some((vec![journal_item(app.data.journal_bytes)?], false))),
        _ => Ok(None),
    }
}

// ------------------------------------------------------------- projects ----

/// Where a project lives: /home/<user>, /var/www, /srv, /opt, /root...
pub(crate) fn proj_location(path: &str) -> String {
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match parts.as_slice() {
        ["home", u, ..] => format!("/home/{u}"),
        ["var", d, ..] => format!("/var/{d}"),
        [first, ..] => format!("/{first}"),
        [] => "/".into(),
    }
}

enum ProjRow {
    Group(String, Vec<usize>),
    Project(usize),
}

fn proj_rows(app: &App) -> Vec<ProjRow> {
    let mut m: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, p) in app.data.projects.iter().enumerate() {
        m.entry(proj_location(&p.path)).or_default().push(i);
    }
    let mut groups: Vec<(String, Vec<usize>)> = m.into_iter().collect();
    groups.sort_by_key(|(_, v)| {
        std::cmp::Reverse(v.iter().map(|&i| app.data.projects[i].total).sum::<u64>())
    });
    let mut out = Vec::new();
    for (k, v) in groups {
        let closed = app.v.proj_closed.contains(&k);
        out.push(ProjRow::Group(k, v.clone()));
        if !closed {
            out.extend(v.into_iter().map(ProjRow::Project));
        }
    }
    out
}

pub(super) fn draw_projects(f: &mut Frame, app: &App, area: Rect, state: &mut TableState) {
    let now = crate::util::now_secs();
    let rows = proj_rows(app);
    let parts = Layout::vertical([Constraint::Min(6), Constraint::Length(9)]).split(area);
    let trows: Vec<Row> = rows
        .iter()
        .map(|r| match r {
            ProjRow::Group(k, v) => {
                let ps = || v.iter().map(|&i| &app.data.projects[i]);
                Row::new(vec![
                    format!(
                        "{} {k}",
                        if app.v.proj_closed.contains(k) {
                            "▸"
                        } else {
                            "▾"
                        }
                    ),
                    format!("{} projects", v.len()),
                    human(ps().map(|p| p.total).sum::<u64>() as i64),
                    human(ps().map(|p| p.artifacts).sum::<u64>() as i64),
                    human(ps().map(|p| p.git).sum::<u64>() as i64),
                    String::new(),
                    String::new(),
                ])
                .style(Style::default().add_modifier(Modifier::BOLD))
            }
            ProjRow::Project(i) => {
                let p = &app.data.projects[*i];
                let idle = (now - p.last_activity).max(0) / 86_400;
                Row::new(vec![
                    format!("    {}", p.name),
                    p.type_label(),
                    human(p.total as i64),
                    human(p.artifacts as i64),
                    human(p.git as i64),
                    format!("{idle}d"),
                    p.path.clone(),
                ])
                .style(Style::default().fg(
                    if p.artifacts > 100 << 20 && idle > 30 {
                        Color::Yellow
                    } else {
                        Color::Reset
                    },
                ))
            }
        })
        .collect();
    let reclaim: u64 = app.data.projects.iter().map(|p| p.artifacts).sum();
    f.render_stateful_widget(
        Table::new(trows, [Constraint::Length(30), Constraint::Length(26), Constraint::Length(11), Constraint::Length(12), Constraint::Length(10), Constraint::Length(6), Constraint::Min(16)])
            .header(hdr(&["project", "type", "total", "build/cache", "git", "idle", "path"]))
            .block(Block::default().borders(Borders::ALL).title(format!(
                " {} projects in {} places · {} rebuildable build output · Enter: open group / project page ",
                app.data.projects.len(),
                rows.iter().filter(|r| matches!(r, ProjRow::Group(..))).count(),
                human(reclaim as i64)
            )))
            .row_highlight_style(hl()),
        parts[0],
        state,
    );
    let lines: Vec<Line> = match rows.get(app.sel[PROJECTS]) {
        Some(ProjRow::Project(i)) => {
            let p = &app.data.projects[*i];
            let mut v = vec![
                kv("project", format!("{} · {}", p.name, p.type_label())),
                kv(
                    "about",
                    if p.description.is_empty() {
                        "-".into()
                    } else {
                        p.description.clone()
                    },
                ),
                kv(
                    "git",
                    format!(
                        "branch {} · remote {}",
                        p.branch.as_deref().unwrap_or("-"),
                        p.remote.as_deref().unwrap_or("-")
                    ),
                ),
                kv(
                    "size",
                    format!(
                        "source {} · build output {} · git {}",
                        human(p.source() as i64),
                        human(p.artifacts as i64),
                        human(p.git as i64)
                    ),
                ),
            ];
            let top: Vec<String> = p
                .artifact_dirs
                .iter()
                .take(3)
                .map(|(d, b)| {
                    format!(
                        "{} {}",
                        d.strip_prefix(&p.path).unwrap_or(d).trim_start_matches('/'),
                        human(*b as i64)
                    )
                })
                .collect();
            v.push(kv(
                "cleanable",
                if top.is_empty() {
                    "nothing".into()
                } else {
                    top.join(" · ")
                },
            ));
            v
        }
        Some(ProjRow::Group(k, v)) => vec![
            kv("place", k.clone()),
            kv(
                "projects",
                v.iter()
                    .map(|&i| app.data.projects[i].name.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        ],
        None => vec![Line::from("No projects found.")],
    };
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title(" Details ")),
        parts[1],
    );
}

// -------------------------------------------------------------- history ----

pub(crate) struct HistoryRow {
    pub t: i64,
    pub ok: Option<bool>,
    pub what: String,
    pub bytes: i64,
    pub note: String,
}

pub(crate) fn load_history() -> Vec<HistoryRow> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(crate::cleanup::AUDIT_LOG) else {
        return Vec::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(1 << 20)));
    let mut text = String::new();
    let _ = f.take(1 << 20).read_to_string(&mut text);
    let mut v: Vec<HistoryRow> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .map(|x| {
            if let Some(job) = x["job"].as_str() {
                HistoryRow {
                    t: x["t"].as_i64().unwrap_or(0),
                    ok: None,
                    what: format!(
                        "== {job}: {} done, {} failed",
                        x["applied"].as_u64().unwrap_or(0),
                        x["failed"].as_u64().unwrap_or(0)
                    ),
                    bytes: x["freed_measured"].as_i64().unwrap_or(0),
                    note: format!("measured · {:.1}s", x["seconds"].as_f64().unwrap_or(0.0)),
                }
            } else {
                HistoryRow {
                    t: x["t"].as_i64().unwrap_or(0),
                    ok: x["ok"].as_bool(),
                    what: x["what"].as_str().unwrap_or("").to_string(),
                    bytes: x["frees"].as_i64().unwrap_or(0),
                    note: x["error"].as_str().unwrap_or("").to_string(),
                }
            }
        })
        .collect();
    v.reverse();
    v
}

pub(super) fn draw_history(f: &mut Frame, app: &mut App, area: Rect, state: &mut TableState) {
    if app.v.history_at.is_none_or(|t| t.elapsed().as_secs() >= 5) {
        app.v.history = load_history();
        app.v.history_at = Some(Instant::now());
    }
    let now = crate::util::now_secs();
    let freed: i64 = app
        .v
        .history
        .iter()
        .filter(|h| h.ok.is_none())
        .map(|h| h.bytes)
        .sum();
    let rows: Vec<Row> = app
        .v
        .history
        .iter()
        .map(|h| {
            let (mark, c) = match h.ok {
                Some(true) => ("done", Color::Green),
                Some(false) => ("FAILED", Color::Red),
                None => ("batch", Color::Cyan),
            };
            Row::new(vec![
                format!("{} ago", crate::insight::age_text(h.t, now)),
                mark.to_string(),
                human(h.bytes),
                h.what.clone(),
                h.note.clone(),
            ])
            .style(Style::default().fg(c))
        })
        .collect();
    f.render_stateful_widget(
        Table::new(
            rows,
            [
                Constraint::Length(10),
                Constraint::Length(7),
                Constraint::Length(11),
                Constraint::Percentage(55),
                Constraint::Min(10),
            ],
        )
        .header(hdr(&["when", "result", "space", "action", "note"]))
        .block(Block::default().borders(Borders::ALL).title(format!(
            " History: every action dux took ({}) · batches freed {} measured on disk ",
            crate::cleanup::AUDIT_LOG,
            human(freed)
        )))
        .row_highlight_style(hl()),
        area,
        state,
    );
}

// ----------------------------------------------------------------- keys ----

pub(super) fn rows(app: &App) -> usize {
    match app.tab {
        DOCKER => app
            .data
            .docker
            .as_ref()
            .map_or(0, |r| docker_rows(r, app.v.dsub)),
        LOGS => log_rows(app, &log_groups(app)).len(),
        PROJECTS => proj_rows(app).len(),
        HISTORY => app.v.history.len(),
        _ => 0,
    }
}

/// Open a confirmation for these items (RISKY ones need D).
fn confirm(app: &mut App, items: Vec<Item>, force_risky: bool) {
    let risky = force_risky
        || items
            .iter()
            .any(|i| crate::fix::assess(i.clone()).safety == Safety::Risky);
    let bytes = items.iter().map(|i| i.frees).sum();
    app.overlay = Overlay::ApplyFixes {
        items,
        bytes,
        risky,
    };
}

/// Keys for the organised views. Returns true when the key was handled.
pub(super) fn key(app: &mut App, code: KeyCode) -> bool {
    // While a scan runs the view is hidden and its data is about to be
    // replaced: no actions until it finishes.
    if app.loading
        && matches!(app.tab, DOCKER | LOGS | PROJECTS)
        && matches!(
            code,
            KeyCode::Char('t' | 'x' | 'b' | 'r' | 'v') | KeyCode::Enter
        )
    {
        app.message = "Scanning; actions are available when the scan finishes.".into();
        return true;
    }
    match app.tab {
        DOCKER => {
            let Some(r) = app.data.docker.clone() else {
                return false;
            };
            if app.v.ddetail.is_some() {
                match code {
                    KeyCode::Esc | KeyCode::Backspace | KeyCode::Left => app.v.ddetail = None,
                    KeyCode::Char('t' | 'x' | 'b') => {
                        let (sub, key) = app.v.ddetail.clone().unwrap_or_default();
                        match docker_index(&r, sub, &key) {
                            Some(i) => run_or_say(app, docker_action(&r, sub, i, code)),
                            None => app.message = format!("{key} is no longer there; nothing done"),
                        }
                    }
                    _ => {}
                }
                return true;
            }
            match code {
                KeyCode::Right | KeyCode::Left => {
                    let n = DSUB.len();
                    app.v.dsel[app.v.dsub] = app.sel[DOCKER];
                    app.v.dsub = if code == KeyCode::Right {
                        (app.v.dsub + 1) % n
                    } else {
                        (app.v.dsub + n - 1) % n
                    };
                    app.sel[DOCKER] = app.v.dsel[app.v.dsub];
                    true
                }
                KeyCode::Enter if app.v.dsub != D_OVERVIEW && app.v.dsub != D_STORAGE => {
                    if let Some(k) = docker_key(&r, app.v.dsub, app.sel[DOCKER]) {
                        app.v.ddetail = Some((app.v.dsub, k));
                    }
                    true
                }
                KeyCode::Char('t' | 'x' | 'b') => {
                    run_or_say(app, docker_action(&r, app.v.dsub, app.sel[DOCKER], code));
                    true
                }
                _ => false,
            }
        }
        LOGS => match code {
            KeyCode::Enter | KeyCode::Char(' ') => {
                let groups = log_groups(app);
                let rows = log_rows(app, &groups);
                let gi = match rows.get(app.sel[LOGS]) {
                    Some(LogRow::Group(gi)) | Some(LogRow::File(gi, _)) => Some(*gi),
                    None => None,
                };
                if let Some(gi) = gi {
                    let k = groups[gi].key.clone();
                    if !app.v.log_open.remove(&k) {
                        app.v.log_open.insert(k);
                    }
                    // Keep the cursor on the group row.
                    let rows = log_rows(app, &groups);
                    app.sel[LOGS] = rows
                        .iter()
                        .position(|r| matches!(r, LogRow::Group(g) if *g == gi))
                        .unwrap_or(0);
                }
                true
            }
            KeyCode::Char('t' | 'r' | 'v') => {
                let a = log_action(app, code);
                run_or_say(app, a);
                true
            }
            _ => false,
        },
        PROJECTS => match code {
            KeyCode::Enter | KeyCode::Right | KeyCode::Char(' ') => {
                match proj_rows(app).get(app.sel[PROJECTS]) {
                    Some(ProjRow::Group(k, _)) => {
                        let k = k.clone();
                        if !app.v.proj_closed.remove(&k) {
                            app.v.proj_closed.insert(k);
                        }
                    }
                    Some(ProjRow::Project(i)) if code != KeyCode::Char(' ') => {
                        let i = *i;
                        app.open_project(i);
                    }
                    _ => {}
                }
                true
            }
            _ => false,
        },
        _ => false,
    }
}

fn run_or_say(app: &mut App, a: Result<Option<(Vec<Item>, bool)>, String>) {
    match a {
        Ok(Some((items, risky))) => confirm(app, items, risky),
        Ok(None) => {}
        Err(e) => app.message = e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(path: &str) -> LogFile {
        LogFile {
            path: path.into(),
            bytes: 1,
            mtime: 0,
            kind: crate::insight::classify_log(path),
            rotated: false,
            protected: false,
            per_min: None,
            writer: None,
            container: None,
        }
    }

    /// Runs real docker commands on throwaway `duxtest-*` resources. Only
    /// on a test host: `cargo test -- --ignored docker_actions_live`.
    #[test]
    #[ignore]
    fn docker_actions_live() {
        let sh = |c: &str| {
            let o = std::process::Command::new("sh")
                .args(["-c", c])
                .output()
                .unwrap();
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        let base =
            sh("docker image ls --format '{{.Repository}}:{{.Tag}}' | grep -m1 -E 'alpine' ");
        assert!(!base.is_empty(), "needs a local alpine-based image");
        sh("docker rm -f duxtest-stopped duxtest-running duxtest-log >/dev/null 2>&1; docker volume rm duxtest-vol >/dev/null 2>&1; docker image rm duxtest-img:1 >/dev/null 2>&1");
        sh(&format!(
            "docker create --name duxtest-stopped {base} true >/dev/null"
        ));
        sh(&format!(
            "docker run -d --name duxtest-running --entrypoint sleep {base} 300 >/dev/null"
        ));
        sh(&format!("docker run --name duxtest-log --entrypoint sh {base} -c 'head -c 24000000 /dev/urandom | base64' >/dev/null"));
        sh(&format!("docker tag {base} duxtest-img:1"));
        sh("docker volume create duxtest-vol >/dev/null");
        let r = crate::docker::collect().unwrap();
        let ci = |n: &str| r.containers.iter().position(|c| c.name == n).unwrap();
        let run = |items: Vec<Item>| {
            for i in &items {
                crate::cleanup::apply(i).unwrap_or_else(|e| panic!("{}: {e:#}", i.what));
            }
        };
        // Truncate a big container log.
        let log = &r.containers[ci("duxtest-log")];
        assert!(log.log_bytes > 10 << 20, "log {}", log.log_bytes);
        let (items, risky) = docker_action(&r, D_CONTAINERS, ci("duxtest-log"), KeyCode::Char('t'))
            .unwrap()
            .unwrap();
        assert!(!risky);
        run(items);
        assert_eq!(
            std::fs::metadata(&log.log_path).unwrap().len(),
            0,
            "log truncated"
        );
        // A running container is refused; a stopped one is removed.
        assert!(
            docker_action(&r, D_CONTAINERS, ci("duxtest-running"), KeyCode::Char('x')).is_err()
        );
        let (items, _) = docker_action(&r, D_CONTAINERS, ci("duxtest-stopped"), KeyCode::Char('x'))
            .unwrap()
            .unwrap();
        run(items);
        assert!(
            sh("docker ps -a --format '{{.Names}}' | grep -x duxtest-stopped || true").is_empty()
        );
        // The extra tag of a used image is removable; the base image is not.
        let ii = |n: &str| r.images.iter().position(|i| i.name == n);
        // Docker may count containers per image ID, so the extra tag can be
        // refused as "in use" (conservative); if offered, only the tag goes.
        if let Some(Ok(Some((items, _)))) =
            ii("duxtest-img:1").map(|i| docker_action(&r, D_IMAGES, i, KeyCode::Char('x')))
        {
            run(items);
            assert!(sh("docker image ls --format '{{.Repository}}:{{.Tag}}' | grep -x duxtest-img:1 || true").is_empty());
            assert!(!sh(&format!("docker image ls --format '{{{{.Repository}}}}:{{{{.Tag}}}}' | grep -x '{base}' || true")).is_empty(), "the base tag must survive");
        }
        if let Some(b) = ii(&base) {
            assert!(
                docker_action(&r, D_IMAGES, b, KeyCode::Char('x')).is_err(),
                "image in use is refused"
            );
        }
        // An unused volume is removable, and marked RISKY.
        let vi = r
            .volumes
            .iter()
            .position(|v| v.name == "duxtest-vol")
            .unwrap();
        let (items, risky) = docker_action(&r, D_VOLUMES, vi, KeyCode::Char('x'))
            .unwrap()
            .unwrap();
        assert!(risky);
        run(items);
        assert!(sh("docker volume ls -q | grep -x duxtest-vol || true").is_empty());
        sh("docker rm -f duxtest-running duxtest-log >/dev/null 2>&1; docker image rm duxtest-img:1 >/dev/null 2>&1");
    }

    #[test]
    fn a_job_runs_each_step_measures_and_finishes() {
        let d = std::env::temp_dir().join(format!("dux-job-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("old.log.1");
        std::fs::write(&f, vec![7u8; 4 << 20]).unwrap();
        let items = vec![
            Item {
                level: 2,
                what: "remove a test file".into(),
                action: Action::Remove { path: f.clone() },
                frees: 4 << 20,
            },
            Item {
                level: 2,
                what: "remove a missing file".into(),
                action: Action::Remove {
                    path: d.join("missing"),
                },
                frees: 1,
            },
        ];
        assert_eq!(
            fs_of(&items[0], None),
            d.to_string_lossy(),
            "measured on the folder"
        );
        let mut j = Job::start("test".into(), items, None);
        for _ in 0..200 {
            j.poll();
            if j.done() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(j.done());
        assert!(matches!(j.steps[0], Step::Done(_)), "{:?}", j.steps[0]);
        assert!(matches!(j.steps[1], Step::Failed(_)), "{:?}", j.steps[1]);
        assert!(!f.exists());
        let (ok, failed, _) = j.counts();
        assert_eq!((ok, failed), (1, 1));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn logs_are_attributed_to_what_they_belong_to() {
        let p = vec![];
        assert_eq!(
            log_owner(&log("/var/log/nginx/access.log"), &p),
            ("Web server", "nginx".into())
        );
        assert_eq!(
            log_owner(&log("/var/log/syslog.1"), &p),
            ("Operating system", "syslog".into())
        );
        assert_eq!(
            log_owner(&log("/var/log/postgresql/postgresql-16-main.log"), &p).0,
            "Database"
        );
        assert_eq!(
            log_owner(&log("/var/log/journal/abc/system.journal"), &p).0,
            "Journal"
        );
        assert_eq!(
            log_owner(&log("/var/log/xgs/agent.log"), &p),
            ("App", "xgs".into())
        );
        assert_eq!(
            log_owner(&log("/opt/forgejo/log/gitea.log"), &p),
            ("App", "forgejo".into())
        );
        assert_eq!(proj_location("/home/alice/code/app"), "/home/alice");
        assert_eq!(proj_location("/srv/x"), "/srv");
    }
}

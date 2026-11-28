//! `dux` / `dux ui`: the terminal UI.
//!
//! 1 Browse (ncdu style, the first screen): folders and files of the current
//!   directory by size; Enter/→ open, ←/Backspace up, s/n/C/M sort, i info,
//!   d delete (with safety checks), N never touch.
//! 2 Disks · 3 Activity (live I/O and who is writing) · 4 Docker · 5 Logs
//! 6 Projects: Enter opens a project page with details and its cleanable
//!   folders, each marked r remove / k keep / n never touch.
//! 7 Fixes: Space toggles, s safe, a all, n never touch, Enter apply.

use crate::cleanup::{Action, Item};
use crate::fix::Safety;
use crate::gather::{self, Gathered};
use crate::util::human;
use crate::walker::DirNode;
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, Gauge, Paragraph, Row, Table, TableState, Tabs, Wrap},
};
use std::io::stdout;
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(crate) mod views;

const TABS: [&str; 8] = [
    "1 Browse",
    "2 Disks",
    "3 Activity",
    "4 Docker",
    "5 Logs",
    "6 Projects",
    "7 Fixes",
    "8 History",
];
const BROWSE: usize = 0;
const DISKS: usize = 1;
const ACTIVITY: usize = 2;
const DOCKER: usize = 3;
const LOGS: usize = 4;
const PROJECTS: usize = 5;
const FIXES: usize = 6;
const HISTORY: usize = 7;

#[derive(Clone, Copy, PartialEq)]
enum Sort {
    Size,
    Name,
    Count,
    Mtime,
}

#[derive(Clone, Debug)]
struct Entry {
    /// Display name (lossy for names that are not UTF-8).
    name: String,
    /// The exact path on disk: every action uses this, never the display name.
    path: std::path::PathBuf,
    dev: u64,
    ino: u64,
    is_dir: bool,
    bytes: u64,
    items: u64,
    mtime: i64,
    uid: u32,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mark {
    Keep,
    Remove,
    Never,
}

/// Each confirmation holds exactly what it will act on, captured when it
/// opened, so a scan finishing (which re-sorts lists) cannot change it.
#[derive(Debug)]
enum Overlay {
    None,
    ApplyFixes {
        items: Vec<Item>,
        bytes: u64,
        risky: bool,
    },
    ApplyProject {
        items: Vec<Item>,
        bytes: u64,
    },
    Delete(Entry),
    Info(Entry),
}

struct App {
    root: String,
    tab: usize,
    data: Gathered,
    loading: bool,
    progress: Arc<AtomicU64>,
    rx: Option<Receiver<Gathered>>,
    sel: [usize; 8],
    cwd: String,
    entries: Vec<Entry>,
    sort: Sort,
    picked: Vec<bool>,
    project_open: Option<usize>,
    project_marks: Vec<Mark>,
    project_sel: usize,
    sampler: crate::live::Sampler,
    live: crate::live::Live,
    last_sample: Option<Instant>,
    changes: Vec<crate::walker::Change>,
    changes_at: Option<i64>,
    protected: Vec<String>,
    overlay: Overlay,
    message: String,
    /// State of the organised views (Docker, Logs, Projects, History) and
    /// the running action job.
    v: views::State,
}

impl App {
    fn new(root: &str) -> App {
        App {
            root: root.to_string(),
            tab: BROWSE,
            data: Gathered::default(),
            loading: true,
            progress: Arc::new(AtomicU64::new(0)),
            rx: None,
            sel: [0; 8],
            cwd: root.to_string(),
            entries: Vec::new(),
            sort: Sort::Size,
            picked: Vec::new(),
            project_open: None,
            project_marks: Vec::new(),
            project_sel: 0,
            sampler: crate::live::Sampler::default(),
            live: crate::live::Live::default(),
            last_sample: None,
            changes: Vec::new(),
            changes_at: None,
            protected: crate::protect::load(),
            overlay: Overlay::None,
            message: String::new(),
            v: views::State::default(),
        }
    }

    fn reload(&mut self) {
        let (tx, rx) = channel();
        let root = self.root.clone();
        let p = self.progress.clone();
        p.store(0, Ordering::Relaxed);
        std::thread::spawn(move || {
            let _ = tx.send(gather::gather(&root, 4, Some(p)));
        });
        self.rx = Some(rx);
        self.loading = true;
        self.data.disks = gather::disks();
        self.list_dir();
    }

    fn poll(&mut self) {
        let Some(rx) = &self.rx else { return };
        let Ok(g) = rx.try_recv() else { return };
        self.picked = vec![false; g.fixes.len()];
        self.changes.clear();
        self.changes_at = None;
        if let (Some(t), Ok(Some(prev))) = (
            g.tree.as_ref(),
            crate::snapshot::load(
                std::path::Path::new(crate::snapshot::DEFAULT_DIR),
                &self.root,
            ),
        ) {
            self.changes = crate::walker::diff(&prev.root, &t.root, 16 << 20);
            self.changes_at = Some(prev.started_at);
        }
        self.data = g;
        self.loading = false;
        self.rx = None;
        // The project list was replaced: marks made on the old one are void.
        if self.project_open.take().is_some() {
            self.project_marks.clear();
            self.message = "Scan finished; reopen the project to mark folders.".into();
        }
        // Follow the item the user moved to; an untouched cursor stays on top.
        let keep = (self.sel[BROWSE] > 0)
            .then(|| self.selected_entry().map(|e| e.name.clone()))
            .flatten();
        self.list_dir();
        if let Some(k) = keep {
            self.sel[BROWSE] = self.entries.iter().position(|e| e.name == k).unwrap_or(0);
        }
    }

    fn node(&self, path: &str) -> Option<&DirNode> {
        let t = self.data.tree.as_ref()?;
        let base = t.root_path.trim_end_matches('/');
        let rel = path.strip_prefix(base).unwrap_or(path);
        t.root.find(rel)
    }

    /// Folders get their sizes from the walk tree; files come from a live
    /// listing of this one directory.
    fn list_dir(&mut self) {
        let mut v = Vec::new();
        let node = self.node(&self.cwd).cloned();
        if let Ok(rd) = std::fs::read_dir(&self.cwd) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let path = e.path();
                let Ok(m) = path.symlink_metadata() else {
                    continue;
                };
                let (dev, ino) = (m.dev(), m.ino());
                if m.is_dir() {
                    let child = node
                        .as_ref()
                        .and_then(|n| n.children.iter().find(|c| c.name == name));
                    v.push(Entry {
                        name,
                        path,
                        dev,
                        ino,
                        is_dir: true,
                        bytes: child.map_or(0, |c| c.total_bytes),
                        items: child.map_or(0, |c| c.total_entries),
                        mtime: child.map_or(m.mtime(), |c| c.newest_change),
                        uid: m.uid(),
                    });
                } else {
                    v.push(Entry {
                        name,
                        path,
                        dev,
                        ino,
                        is_dir: false,
                        bytes: m.blocks() * 512,
                        items: 1,
                        mtime: m.mtime(),
                        uid: m.uid(),
                    });
                }
            }
        }
        self.entries = v;
        self.sort_entries();
    }

    fn sort_entries(&mut self) {
        match self.sort {
            Sort::Size => self
                .entries
                .sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.name.cmp(&b.name))),
            Sort::Name => self
                .entries
                .sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name))),
            Sort::Count => self.entries.sort_by_key(|e| std::cmp::Reverse(e.items)),
            Sort::Mtime => self.entries.sort_by_key(|e| std::cmp::Reverse(e.mtime)),
        }
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.entries.get(self.sel[BROWSE])
    }

    fn rows(&self) -> usize {
        match self.tab {
            BROWSE => self.entries.len(),
            DISKS => self.data.disks.len(),
            ACTIVITY => self.live.writing.len(),
            DOCKER | LOGS | PROJECTS | HISTORY => views::rows(self),
            _ => self.data.fixes.len(),
        }
    }

    fn picked_total(&self) -> (usize, u64, bool) {
        let mut out = (0, 0, false);
        for (i, x) in self.data.fixes.iter().enumerate() {
            if self.picked.get(i).copied().unwrap_or(false) {
                out.0 += 1;
                out.1 += x.frees;
                out.2 |= x.safety == Safety::Risky;
            }
        }
        out
    }

    fn open_project(&mut self, i: usize) {
        let Some(p) = self.data.projects.get(i) else {
            return;
        };
        self.project_marks = p
            .artifact_dirs
            .iter()
            .map(|(d, _)| {
                if crate::protect::covered(&self.protected, d) {
                    Mark::Never
                } else {
                    Mark::Keep
                }
            })
            .collect();
        self.project_open = Some(i);
        self.project_sel = 0;
    }

    /// Returns true only when the path is really on the saved list.
    fn never_touch(&mut self, path: &str) -> bool {
        match crate::protect::add(path) {
            Ok(added) => {
                self.protected = crate::protect::load();
                self.message = if added {
                    format!("{path} is now on the never-touch list")
                } else {
                    format!("{path} was already on the never-touch list")
                };
                true
            }
            Err(e) => {
                self.message = format!("NOT protected: {e:#} (run as root)");
                false
            }
        }
    }

    fn is_root(&mut self) -> bool {
        // SAFETY: geteuid has no preconditions.
        let root = unsafe { libc::geteuid() } == 0;
        if !root {
            self.message = "Changing files needs root: run sudo dux".into();
        }
        root
    }

    /// Run the items in the background with a live progress window; each
    /// step and the measured space freed are logged to the History.
    fn apply_items(&mut self, items: Vec<Item>) {
        if !self.is_root() {
            return;
        }
        let title = if items.len() == 1 {
            "Applying 1 action".to_string()
        } else {
            format!("Applying {} actions", items.len())
        };
        let root = self.data.docker.as_ref().map(|d| d.root_dir.clone());
        self.v.job = Some(views::Job::start(title, items, root));
    }
}

fn bar(frac: f64, width: usize) -> String {
    let n = (frac.clamp(0.0, 1.0) * width as f64).round() as usize;
    format!("{}{}", "#".repeat(n), " ".repeat(width - n))
}

fn color(s: Safety) -> Color {
    match s {
        Safety::Safe => Color::Green,
        Safety::Caution => Color::Yellow,
        Safety::Risky => Color::Red,
    }
}

fn hdr(cells: &[&str]) -> Row<'static> {
    Row::new(cells.iter().map(|c| c.to_string()).collect::<Vec<_>>()).style(
        Style::default()
            .add_modifier(Modifier::BOLD)
            .fg(Color::Cyan),
    )
}

fn hl() -> Style {
    Style::default()
        .bg(Color::Blue)
        .fg(Color::White)
        .add_modifier(Modifier::BOLD)
}

fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(3),
    ])
    .split(area);
    let rootpct = app
        .data
        .disks
        .iter()
        .find(|d| d.path == "/")
        .map(|d| d.usage.used_pct())
        .unwrap_or(0.0);
    f.render_widget(
        Tabs::new(TABS.to_vec())
            .select(app.tab)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" dux · {} · disk {:.1}% used ", app.root, rootpct)),
            )
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        chunks[0],
    );
    let body = chunks[1];
    let mut state = TableState::default();
    state.select(Some(app.sel[app.tab].min(app.rows().saturating_sub(1))));
    let needs_walk = matches!(app.tab, DOCKER | LOGS | PROJECTS | FIXES);
    if app.loading && needs_walk {
        f.render_widget(
            Paragraph::new(format!(
                "Scanning {} ... {} entries so far.\nBrowse, Disks and Activity work already.",
                app.root,
                app.progress.load(Ordering::Relaxed)
            ))
            .block(Block::default().borders(Borders::ALL)),
            body,
        );
        draw_footer(f, app, chunks[2]);
        draw_overlay(f, app, area);
        if let Some(job) = &app.v.job {
            views::draw_job(f, job, area);
        }
        return;
    }
    match app.tab {
        BROWSE => draw_browse(f, app, body, &mut state),
        DISKS => draw_disks(f, app, body, &mut state),
        ACTIVITY => draw_activity(f, app, body, &mut state),
        DOCKER => views::draw_docker(f, app, body, &mut state),
        LOGS => views::draw_logs(f, app, body, &mut state),
        PROJECTS => match app.project_open {
            Some(i) => draw_project(f, app, i, body),
            None => views::draw_projects(f, app, body, &mut state),
        },
        HISTORY => views::draw_history(f, app, body, &mut state),
        _ => draw_fixes(f, app, body, &mut state),
    }
    draw_footer(f, app, chunks[2]);
    draw_overlay(f, app, area);
    if let Some(job) = &app.v.job {
        views::draw_job(f, job, area);
    }
}

fn draw_browse(f: &mut Frame, app: &App, body: Rect, state: &mut TableState) {
    let sum: u64 = app.entries.iter().map(|e| e.bytes).sum::<u64>();
    let total = sum.max(1);
    let biggest = app
        .entries
        .iter()
        .map(|e| e.bytes)
        .max()
        .unwrap_or(1)
        .max(1);
    let now = crate::util::now_secs();
    // Build rows only for the visible window: a folder can hold a million
    // entries, and building them all on every frame made the UI crawl.
    let visible = usize::from(body.height.saturating_sub(3)).max(1);
    let sel = app.sel[BROWSE].min(app.entries.len().saturating_sub(1));
    let start = sel
        .saturating_sub(visible / 2)
        .min(app.entries.len().saturating_sub(visible));
    state.select((!app.entries.is_empty()).then_some(sel - start));
    let rows: Vec<Row> = app
        .entries
        .iter()
        .skip(start)
        .take(visible)
        .map(|e| {
            let path = e.path.to_string_lossy();
            let never = crate::protect::covered(&app.protected, &path);
            let size = if e.is_dir && e.bytes == 0 && app.loading {
                "…".to_string()
            } else {
                human(e.bytes as i64)
            };
            let style = if never {
                Style::default().fg(Color::Magenta)
            } else if e.is_dir {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Row::new(vec![
                size,
                format!("[{}]", bar(e.bytes as f64 / biggest as f64, 12)),
                format!("{:>5.1}%", e.bytes as f64 * 100.0 / total as f64),
                if e.is_dir {
                    e.items.to_string()
                } else {
                    String::new()
                },
                crate::insight::age_text(e.mtime, now),
                format!(
                    "{}{}{}",
                    crate::insight::clean_text(&e.name, 200),
                    if e.is_dir { "/" } else { "" },
                    if never { "  [never touch]" } else { "" }
                ),
            ])
            .style(style)
        })
        .collect();
    let sort = match app.sort {
        Sort::Size => "size",
        Sort::Name => "name",
        Sort::Count => "items",
        Sort::Mtime => "changed",
    };
    let scan = if app.loading {
        format!(
            " · scanning {} entries…",
            app.progress.load(Ordering::Relaxed)
        )
    } else {
        String::new()
    };
    f.render_stateful_widget(
        Table::new(
            rows,
            [
                Constraint::Length(11),
                Constraint::Length(14),
                Constraint::Length(7),
                Constraint::Length(9),
                Constraint::Length(5),
                Constraint::Min(20),
            ],
        )
        .header(hdr(&["size", "", "share", "items", "age", "name"]))
        .block(Block::default().borders(Borders::ALL).title(format!(
            " {}  {} in {} entries · sorted by {sort}{scan} ",
            app.cwd,
            human(sum as i64),
            app.entries.len()
        )))
        .row_highlight_style(hl()),
        body,
        state,
    );
}

fn draw_disks(f: &mut Frame, app: &App, body: Rect, state: &mut TableState) {
    let rows: Vec<Row> = app
        .data
        .disks
        .iter()
        .map(|d| {
            let p = d.usage.used_pct();
            let c = if p >= 95.0 {
                Color::Red
            } else if p >= 85.0 {
                Color::Yellow
            } else {
                Color::Green
            };
            Row::new(vec![
                d.path.clone(),
                d.fstype.clone(),
                human(d.usage.total as i64),
                human(d.usage.used as i64),
                human(d.usage.avail as i64),
                format!("[{}] {p:>5.1}%", bar(p / 100.0, 20)),
                d.usage
                    .inode_pct()
                    .map(|x| format!("{x:.1}%"))
                    .unwrap_or_else(|| "-".into()),
            ])
            .style(Style::default().fg(c))
        })
        .collect();
    f.render_stateful_widget(
        Table::new(
            rows,
            [
                Constraint::Percentage(25),
                Constraint::Length(10),
                Constraint::Length(11),
                Constraint::Length(11),
                Constraint::Length(11),
                Constraint::Length(30),
                Constraint::Length(8),
            ],
        )
        .header(hdr(&[
            "mount", "type", "size", "used", "free", "use", "inodes",
        ]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Disks (live) "),
        )
        .row_highlight_style(hl()),
        body,
        state,
    );
}

fn draw_project(f: &mut Frame, app: &App, i: usize, body: Rect) {
    let Some(p) = app.data.projects.get(i) else {
        return;
    };
    let now = crate::util::now_secs();
    let parts = Layout::vertical([Constraint::Length(9), Constraint::Min(5)]).split(body);
    let info = vec![
        Line::from(vec![Span::styled(p.name.clone(), Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan)), Span::raw(format!("   {}", p.type_label()))]),
        Line::from(if p.description.is_empty() { "(no description in the manifest)".to_string() } else { p.description.clone() }),
        Line::from(format!("path    {}", p.path)),
        Line::from(format!("git     branch {}   remote {}", p.branch.as_deref().unwrap_or("-"), p.remote.as_deref().unwrap_or("-"))),
        Line::from(format!("size    total {}   source {}   build output and caches {}   git history {}", human(p.total as i64), human(p.source() as i64), human(p.artifacts as i64), human(p.git as i64))),
        Line::from(format!("active  last change {} ago", crate::insight::age_text(p.last_activity, now))),
        Line::from("keys    r remove · k keep · n never touch · a mark all remove · Enter apply · Esc back").style(Style::default().fg(Color::DarkGray)),
    ];
    f.render_widget(
        Paragraph::new(info).block(Block::default().borders(Borders::ALL).title(" Project ")),
        parts[0],
    );
    let rows: Vec<Row> = p
        .artifact_dirs
        .iter()
        .enumerate()
        .map(|(k, (d, b))| {
            let (label, c) = match app.project_marks.get(k).copied().unwrap_or(Mark::Keep) {
                Mark::Remove => ("REMOVE", Color::Yellow),
                Mark::Keep => ("keep", Color::Gray),
                Mark::Never => ("NEVER TOUCH", Color::Magenta),
            };
            Row::new(vec![
                label.to_string(),
                human(*b as i64),
                d.strip_prefix(&p.path)
                    .unwrap_or(d)
                    .trim_start_matches('/')
                    .to_string(),
            ])
            .style(Style::default().fg(c))
        })
        .collect();
    let remove: u64 = p
        .artifact_dirs
        .iter()
        .zip(&app.project_marks)
        .filter(|(_, m)| **m == Mark::Remove)
        .map(|((_, b), _)| *b)
        .sum();
    let mut st = TableState::default();
    st.select(Some(app.project_sel));
    let title = if p.artifact_dirs.is_empty() {
        " No build output or cache folders in this project ".to_string()
    } else {
        format!(
            " Cleanable folders: {} marked for removal (rebuilt by the project's own tools) ",
            human(remove as i64)
        )
    };
    f.render_stateful_widget(
        Table::new(
            rows,
            [
                Constraint::Length(13),
                Constraint::Length(11),
                Constraint::Min(20),
            ],
        )
        .header(hdr(&["action", "size", "build output / cache folder"]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(hl()),
        parts[1],
        &mut st,
    );
}

fn draw_fixes(f: &mut Frame, app: &App, body: Rect, state: &mut TableState) {
    let parts = Layout::vertical([Constraint::Min(5), Constraint::Length(5)]).split(body);
    let rows: Vec<Row> = app
        .data
        .fixes
        .iter()
        .enumerate()
        .map(|(i, x)| {
            let mark = if x.item.is_none() {
                " - "
            } else if app.picked[i] {
                "[x]"
            } else {
                "[ ]"
            };
            Row::new(vec![
                mark.to_string(),
                x.safety.label().to_string(),
                human(x.frees as i64),
                x.title.clone(),
            ])
            .style(Style::default().fg(color(x.safety)))
        })
        .collect();
    let (n, b, _) = app.picked_total();
    f.render_stateful_widget(
        Table::new(
            rows,
            [
                Constraint::Length(4),
                Constraint::Length(8),
                Constraint::Length(11),
                Constraint::Min(30),
            ],
        )
        .header(hdr(&["", "rating", "frees", "fix"]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Fixes: {n} picked, {} ", human(b as i64))),
        )
        .row_highlight_style(hl()),
        parts[0],
        state,
    );
    let detail = app
        .data
        .fixes
        .get(app.sel[FIXES])
        .map(|x| {
            format!(
                "{}\nUndo: {}{}",
                x.why,
                x.undo,
                x.manual
                    .as_ref()
                    .map(|m| format!("\nBy hand: {m}"))
                    .unwrap_or_default()
            )
        })
        .unwrap_or_default();
    f.render_widget(
        Paragraph::new(detail).wrap(Wrap { trim: true }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" What it does "),
        ),
        parts[1],
    );
}

fn draw_activity(f: &mut Frame, app: &App, body: Rect, state: &mut TableState) {
    let l = &app.live;
    let rate = crate::live::rate_text;
    let rows4 = Layout::vertical([
        Constraint::Length(9),
        Constraint::Min(6),
        Constraint::Length(8),
        Constraint::Length(10),
    ])
    .split(body);
    let top = Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(rows4[0]);
    let (tr, tw) = l
        .disks
        .iter()
        .fold((0.0, 0.0), |a, x| (a.0 + x.read_bps, a.1 + x.write_bps));
    let drows: Vec<Row> = l
        .disks
        .iter()
        .map(|x| Row::new(vec![x.name.clone(), rate(x.read_bps), rate(x.write_bps)]))
        .collect();
    f.render_widget(
        Table::new(
            drows,
            [
                Constraint::Min(8),
                Constraint::Length(13),
                Constraint::Length(13),
            ],
        )
        .header(hdr(&["disk", "read", "write"]))
        .block(Block::default().borders(Borders::ALL).title(format!(
            " Disk I/O now: read {} · write {} ",
            rate(tr),
            rate(tw)
        ))),
        top[0],
    );
    let prows: Vec<Row> = l
        .procs
        .iter()
        .map(|p| {
            Row::new(vec![
                rate(p.write_bps),
                rate(p.read_bps),
                p.pid.to_string(),
                p.name.clone(),
                p.unit.clone(),
            ])
        })
        .collect();
    f.render_widget(
        Table::new(
            prows,
            [
                Constraint::Length(12),
                Constraint::Length(12),
                Constraint::Length(8),
                Constraint::Length(16),
                Constraint::Min(10),
            ],
        )
        .header(hdr(&[
            "writes",
            "reads",
            "pid",
            "process",
            "service / container",
        ]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Who is writing and reading now "),
        ),
        top[1],
    );
    let mid = Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(rows4[1]);
    let wrows: Vec<Row> = l
        .writing
        .iter()
        .map(|x| {
            Row::new(vec![
                rate(x.write_bps),
                if x.growth_bps > 0.0 {
                    format!("+{}", rate(x.growth_bps))
                } else {
                    "-".into()
                },
                human(x.size as i64),
                format!("{} {}", x.pid, x.process),
                x.path.clone(),
            ])
        })
        .collect();
    f.render_stateful_widget(
        Table::new(
            wrows,
            [
                Constraint::Length(12),
                Constraint::Length(13),
                Constraint::Length(11),
                Constraint::Length(18),
                Constraint::Min(20),
            ],
        )
        .header(hdr(&[
            "write",
            "growth",
            "size",
            "pid process",
            "file being written",
        ]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Writing now (↑↓ select for writer details) "),
        )
        .row_highlight_style(hl()),
        mid[0],
        state,
    );
    let rrows: Vec<Row> = l
        .reading
        .iter()
        .map(|x| {
            Row::new(vec![
                rate(x.read_bps),
                format!("{} {}", x.pid, x.process),
                x.path.clone(),
            ])
        })
        .collect();
    f.render_widget(
        Table::new(
            rrows,
            [
                Constraint::Length(12),
                Constraint::Length(18),
                Constraint::Min(20),
            ],
        )
        .header(hdr(&["read", "pid process", "file being read"]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Reading now "),
        ),
        mid[1],
    );
    let detail: Vec<Line> = match l
        .writing
        .get(app.sel[ACTIVITY])
        .and_then(|w| crate::live::details(w.pid).map(|d| (w, d)))
    {
        Some((w, d)) => vec![
            Line::from(format!(
                "file    {}  ({}, writing {}, growing {})",
                w.path,
                human(w.size as i64),
                rate(w.write_bps),
                rate(w.growth_bps)
            )),
            Line::from(format!(
                "writer  pid {} {}  user {}  running {}  threads {}  memory {}",
                d.pid,
                d.name,
                d.user,
                crate::insight::age_text(
                    crate::util::now_secs() - d.running_secs as i64,
                    crate::util::now_secs()
                ),
                d.threads,
                human(d.rss_bytes as i64)
            )),
            Line::from(format!("command {}", d.cmdline)),
            Line::from(format!(
                "parent  pid {} {}   service/container {}   cwd {}",
                d.ppid,
                d.parent,
                if w.unit.is_empty() {
                    d.unit.clone()
                } else {
                    w.unit.clone()
                },
                d.cwd
            )),
            Line::from(format!("next    {}", crate::live::next_steps(&d)))
                .style(Style::default().fg(Color::Cyan)),
        ],
        None => vec![Line::from(if l.seconds == 0.0 {
            "Sampling... (refreshes every second)"
        } else {
            "No file is being written right now."
        })],
    };
    f.render_widget(
        Paragraph::new(detail).wrap(Wrap { trim: true }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Writer details "),
        ),
        rows4[2],
    );
    let bot = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows4[3]);
    let grows: Vec<Row> = l
        .grown_minute
        .iter()
        .map(|(p, g, s)| Row::new(vec![format!("+{}", human(*g)), human(*s as i64), p.clone()]))
        .collect();
    f.render_widget(
        Table::new(
            grows,
            [
                Constraint::Length(12),
                Constraint::Length(11),
                Constraint::Min(20),
            ],
        )
        .header(hdr(&["grew", "size", "file"]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Growing now (last minute) "),
        ),
        bot[0],
    );
    let crows: Vec<Row> = app
        .changes
        .iter()
        .take(30)
        .map(|c| {
            let dl = c.delta();
            Row::new(vec![
                format!(
                    "{}{}",
                    if dl >= 0 { "+" } else { "-" },
                    crate::util::human_magnitude(dl)
                ),
                human(c.after as i64),
                c.path.clone(),
            ])
            .style(Style::default().fg(if dl >= 0 {
                Color::Yellow
            } else {
                Color::Green
            }))
        })
        .collect();
    let since = match app.changes_at {
        Some(t) => format!(
            " Biggest changes since the snapshot {} ago ",
            crate::util::ago(t)
        ),
        None if app.loading => " Biggest changes: scanning... ".to_string(),
        None => " Biggest changes: no sentinel snapshot yet ".to_string(),
    };
    f.render_widget(
        Table::new(
            crows,
            [
                Constraint::Length(12),
                Constraint::Length(11),
                Constraint::Min(20),
            ],
        )
        .header(hdr(&["change", "now", "folder"]))
        .block(Block::default().borders(Borders::ALL).title(since)),
        bot[1],
    );
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let help = match app.tab {
        BROWSE => "↑↓ move · Enter/→ open · ←/Bksp up · s/n/C/M sort · i info · d delete · N never touch · 2-8 views · q quit",
        DOCKER if app.v.ddetail.is_some() => "t truncate log · x remove · Esc back",
        DOCKER => "←→ Overview/Stacks/Containers/Images/Volumes/Logs/Storage · ↑↓ move · Enter details · t truncate · x remove · b prune cache",
        LOGS => "↑↓ move · Enter open/close group · t truncate · r remove rotated · v vacuum journal · 1-8 views",
        PROJECTS if app.project_open.is_some() => "r remove · k keep · n never touch · a all · Enter apply · Esc back",
        PROJECTS => "↑↓ move · Enter open group / project page · 1-8 views · q quit",
        FIXES => "Space toggle · s safe · a all · c clear · n never touch · Enter apply · q quit",
        HISTORY => "↑↓ move · every action dux took, newest first · 1-8 views · q quit",
        _ => "↑↓ move · 1-8 views · r rescan · q quit",
    };
    let msg = if app.message.is_empty() {
        help.to_string()
    } else {
        format!("{} · {help}", app.message)
    };
    let root = app
        .data
        .disks
        .iter()
        .find(|d| d.path == "/")
        .map(|d| d.usage.used_pct())
        .unwrap_or(0.0);
    f.render_widget(
        Gauge::default()
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" {msg} ")),
            )
            .gauge_style(Style::default().fg(if root >= 95.0 {
                Color::Red
            } else if root >= 85.0 {
                Color::Yellow
            } else {
                Color::Green
            }))
            .label(format!("/ {root:.1}% used"))
            .ratio((root / 100.0).clamp(0.0, 1.0)),
        area,
    );
}

/// A centred box tall enough for its wrapped text, so the key to press is
/// never cut off by a long path.
fn popup(f: &mut Frame, area: Rect, title: &str, text: String) {
    let w = area.width.min(100);
    let inner = usize::from(w.saturating_sub(2)).max(1);
    let lines: usize = text
        .lines()
        .map(|l| l.chars().count().div_ceil(inner).max(1))
        .sum();
    let height = u16::try_from(lines + 2)
        .unwrap_or(u16::MAX)
        .min(area.height);
    let r = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + area.height.saturating_sub(height) / 3,
        width: w,
        height,
    };
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(text).wrap(Wrap { trim: true }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {title} ")),
        ),
        r,
    );
}

fn draw_overlay(f: &mut Frame, app: &App, area: Rect) {
    match &app.overlay {
        Overlay::None => {}
        Overlay::ApplyFixes {
            items,
            bytes,
            risky,
        } => {
            let list: String = items
                .iter()
                .take(8)
                .map(|x| format!("\n  - {}", x.what))
                .collect();
            let more = if items.len() > 8 {
                format!("\n  ... and {} more", items.len() - 8)
            } else {
                String::new()
            };
            let t = if *risky {
                format!("Apply {} fixes, freeing about {}?{list}{more}\n\nThe selection includes RISKY items that can lose data.\nPress D to apply, Esc to cancel.", items.len(), human(*bytes as i64))
            } else {
                format!("Apply {} fixes, freeing about {}?{list}{more}\n\nPress y to apply, Esc to cancel.", items.len(), human(*bytes as i64))
            };
            popup(f, area, "Confirm", t);
        }
        Overlay::ApplyProject { items, bytes } => {
            let list: String = items.iter().map(|x| format!("\n  - {}", x.what)).collect();
            popup(f, area, "Confirm", format!("Remove these folders, freeing about {}?{list}\nThey are rebuilt by the project's own tools (npm install, cargo build...).\n\nPress y to remove, Esc to cancel.", human(*bytes as i64)));
        }
        Overlay::Delete(e) => {
            popup(
                f,
                area,
                "Delete",
                format!(
                    "RISKY: permanently delete {}{}?\nSize {}{}. This cannot be undone.\n\nPress D to delete, Esc to cancel.",
                    crate::insight::clean_text(&e.path.to_string_lossy(), 400),
                    if e.is_dir { "/ and everything in it" } else { "" },
                    human(e.bytes as i64),
                    if e.is_dir { format!(", {} items", e.items) } else { String::new() }
                ),
            );
        }
        Overlay::Info(e) => {
            let path = e.path.to_string_lossy();
            let users = crate::insight::users();
            let culprit = crate::attrib::culprit_for(&path)
                .map(|c| format!("{}: {}", c.label, c.fix))
                .unwrap_or_else(|| "-".into());
            let safety = if crate::protect::is_protected(&e.path) {
                "on (or containing a path on) the never-touch list"
            } else if crate::cleanup::system_path(&path) {
                "system or top-level path: dux will not delete it"
            } else {
                "you can delete it with d (RISKY)"
            };
            popup(
                f,
                area,
                "Info",
                format!(
                    "{}\ntype      {}\nsize      {}\nitems     {}\nchanged   {} ago\nowner     {}\nknown as  {culprit}\ndelete    {safety}\n\nany key to close",
                    crate::insight::clean_text(&path, 400),
                    if e.is_dir { "folder" } else { "file" },
                    human(e.bytes as i64),
                    e.items,
                    crate::insight::age_text(e.mtime, crate::util::now_secs()),
                    users.get(&e.uid).cloned().unwrap_or_else(|| e.uid.to_string())
                ),
            );
        }
    }
}

/// Keys while an overlay is open. Returns true if the key was consumed.
fn overlay_key(app: &mut App, code: KeyCode) -> bool {
    match std::mem::replace(&mut app.overlay, Overlay::None) {
        Overlay::None => return false,
        Overlay::Info(_) => {}
        Overlay::ApplyFixes { items, risky, .. } => {
            let go = (code == KeyCode::Char('D') && risky)
                || (matches!(code, KeyCode::Char('y') | KeyCode::Char('Y')) && !risky);
            if go {
                app.apply_items(items);
            } else {
                app.message = "Nothing applied.".into();
            }
        }
        Overlay::ApplyProject { items, .. } => {
            if matches!(code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                app.project_open = None;
                app.apply_items(items);
            } else {
                app.message = "Nothing removed.".into();
            }
        }
        Overlay::Delete(e) => {
            if code != KeyCode::Char('D') {
                app.message = "Nothing deleted.".into();
                return true;
            }
            // Still the same file or folder that was shown?
            let same = e
                .path
                .symlink_metadata()
                .is_ok_and(|m| m.dev() == e.dev && m.ino() == e.ino);
            if !same {
                app.message = format!(
                    "{} changed since the prompt opened; nothing deleted.",
                    e.path.display()
                );
                return true;
            }
            if app.is_root() {
                let item = Item {
                    level: 4,
                    what: format!("delete {}", e.path.display()),
                    action: Action::RemoveUserPath {
                        path: e.path.clone(),
                    },
                    frees: e.bytes,
                };
                match crate::cleanup::apply(&item) {
                    Ok(()) => {
                        app.message =
                            format!("Deleted {} ({})", e.path.display(), human(e.bytes as i64));
                        app.entries.retain(|x| x.path != e.path);
                    }
                    Err(err) => app.message = format!("Not deleted: {err:#}"),
                }
            }
        }
    }
    true
}

/// Keys on an open project page. Returns true to quit.
fn project_key(app: &mut App, code: KeyCode) -> bool {
    let n = app.project_marks.len();
    let set = |app: &mut App, m: Mark| {
        if let Some(slot) = app.project_marks.get_mut(app.project_sel) {
            *slot = m;
        }
    };
    match code {
        KeyCode::Esc | KeyCode::Left | KeyCode::Backspace => app.project_open = None,
        KeyCode::Down | KeyCode::Char('j') => {
            app.project_sel = (app.project_sel + 1).min(n.saturating_sub(1))
        }
        KeyCode::Up => app.project_sel = app.project_sel.saturating_sub(1),
        KeyCode::Char('r') => set(app, Mark::Remove),
        KeyCode::Char('k') => set(app, Mark::Keep),
        KeyCode::Char('n') => {
            let dir = app
                .project_open
                .and_then(|i| app.data.projects.get(i))
                .and_then(|p| p.artifact_dirs.get(app.project_sel))
                .map(|(d, _)| d.clone());
            if let Some(d) = dir {
                if app.never_touch(&d) {
                    set(app, Mark::Never);
                }
            }
        }
        KeyCode::Char('a') => app
            .project_marks
            .iter_mut()
            .filter(|m| **m != Mark::Never)
            .for_each(|m| *m = Mark::Remove),
        KeyCode::Enter => {
            let items: Vec<Item> = app
                .project_open
                .and_then(|i| app.data.projects.get(i))
                .map(|p| {
                    p.artifact_dirs
                        .iter()
                        .zip(&app.project_marks)
                        .filter(|(_, m)| **m == Mark::Remove)
                        .map(|((d, b), _)| Item {
                            level: 4,
                            what: format!("remove {d}"),
                            action: Action::RemoveTree { path: d.into() },
                            frees: *b,
                        })
                        .collect()
                })
                .unwrap_or_default();
            if items.is_empty() {
                app.message = "Mark folders with r first.".into();
            } else {
                let bytes = items.iter().map(|i| i.frees).sum();
                app.overlay = Overlay::ApplyProject { items, bytes };
            }
        }
        KeyCode::Char('q') => return true,
        _ => {}
    }
    false
}

/// Returns true to quit.
fn key(app: &mut App, code: KeyCode, mods: KeyModifiers) -> bool {
    if code == KeyCode::Char('c') && mods.contains(KeyModifiers::CONTROL) {
        return true;
    }
    // A running or finished job owns the screen until it is closed.
    if let Some(job) = &app.v.job {
        if job.done() && matches!(code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')) {
            app.v.job = None;
            app.message = "Rescanning after the cleanup...".into();
            app.reload();
        }
        return false;
    }
    if overlay_key(app, code) {
        return false;
    }
    if app.tab == PROJECTS && app.project_open.is_some() {
        return project_key(app, code);
    }
    if views::key(app, code) {
        return false;
    }
    let rows = app.rows();
    let t = app.tab;
    match code {
        KeyCode::Char('q') => return true,
        KeyCode::Char(c @ '1'..='8') => app.tab = (c as usize) - ('1' as usize),
        KeyCode::Tab => app.tab = (app.tab + 1) % TABS.len(),
        KeyCode::BackTab => app.tab = (app.tab + TABS.len() - 1) % TABS.len(),
        KeyCode::Down | KeyCode::Char('j') => {
            app.sel[t] = (app.sel[t] + 1).min(rows.saturating_sub(1))
        }
        KeyCode::Up | KeyCode::Char('k') => app.sel[t] = app.sel[t].saturating_sub(1),
        KeyCode::PageDown => app.sel[t] = (app.sel[t] + 20).min(rows.saturating_sub(1)),
        KeyCode::PageUp => app.sel[t] = app.sel[t].saturating_sub(20),
        KeyCode::Home => app.sel[t] = 0,
        KeyCode::End => app.sel[t] = rows.saturating_sub(1),
        KeyCode::Char('r') if t != BROWSE => {
            app.message = "Rescanning...".into();
            app.reload();
        }
        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') if t == BROWSE => {
            if let Some(e) = app.selected_entry().filter(|e| e.is_dir).cloned() {
                app.cwd = e.path.to_string_lossy().into_owned();
                app.sel[BROWSE] = 0;
                app.list_dir();
            }
        }
        KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') if t == BROWSE => {
            if app.cwd != app.root && app.cwd != "/" {
                let child = app.cwd.rsplit('/').next().unwrap_or("").to_string();
                app.cwd = match app.cwd.rsplit_once('/') {
                    Some(("", _)) | None => "/".to_string(),
                    Some((p, _)) => p.to_string(),
                };
                app.list_dir();
                app.sel[BROWSE] = app
                    .entries
                    .iter()
                    .position(|e| e.name == child)
                    .unwrap_or(0);
            }
        }
        KeyCode::Char(c @ ('s' | 'n' | 'C' | 'M')) if t == BROWSE => {
            app.sort = match c {
                's' => Sort::Size,
                'n' => Sort::Name,
                'C' => Sort::Count,
                _ => Sort::Mtime,
            };
            app.sort_entries();
        }
        KeyCode::Char('i') if t == BROWSE => {
            if let Some(e) = app.selected_entry().cloned() {
                app.overlay = Overlay::Info(e);
            }
        }
        KeyCode::Char('d' | 'N') if t == BROWSE && !app.is_root() => {}
        KeyCode::Char('d') if t == BROWSE => {
            if let Some(e) = app.selected_entry().cloned() {
                let path = e.path.to_string_lossy().into_owned();
                if crate::protect::is_protected(&e.path) {
                    app.message =
                        format!("{path} is on (or contains a path on) the never-touch list");
                } else if crate::cleanup::system_path(&path) {
                    app.message =
                        format!("{path} is a system or top-level path; dux will not delete it");
                } else {
                    app.overlay = Overlay::Delete(e);
                }
            }
        }
        KeyCode::Char('N') if t == BROWSE => {
            if let Some(e) = app.selected_entry().cloned() {
                match e.path.to_str() {
                    Some(p) => {
                        app.never_touch(p);
                    }
                    None => {
                        app.message =
                            "This name is not valid UTF-8; the never-touch list cannot store it."
                                .into()
                    }
                }
            }
        }
        KeyCode::Char(' ') if t == FIXES => {
            let i = app.sel[FIXES];
            if app.data.fixes.get(i).is_some_and(|x| x.item.is_some()) {
                app.picked[i] = !app.picked[i];
            }
        }
        KeyCode::Char('s') if t == FIXES => {
            for (i, x) in app.data.fixes.iter().enumerate() {
                app.picked[i] = x.item.is_some() && x.safety == Safety::Safe;
            }
        }
        KeyCode::Char('a') if t == FIXES => {
            for (i, x) in app.data.fixes.iter().enumerate() {
                app.picked[i] = x.item.is_some() && x.safety != Safety::Risky;
            }
        }
        KeyCode::Char('c') if t == FIXES => app.picked.iter_mut().for_each(|p| *p = false),
        KeyCode::Char('n') if t == FIXES => {
            let i = app.sel[FIXES];
            if let Some(p) = app.data.fixes.get(i).and_then(crate::fix::target_path) {
                if app.never_touch(&p) {
                    app.data.fixes.remove(i);
                    app.picked.remove(i);
                }
            } else {
                app.message = "This fix has no single path to protect.".into();
            }
        }
        KeyCode::Enter if t == FIXES && app.loading => {
            app.message = "Scanning; apply fixes when the scan finishes.".into();
        }
        KeyCode::Enter if t == FIXES => {
            let (n, bytes, risky) = app.picked_total();
            if n == 0 {
                app.message = "Pick fixes first: Space, s (safe) or a (all).".into();
            } else {
                let items: Vec<Item> = app
                    .data
                    .fixes
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| app.picked.get(*i).copied().unwrap_or(false))
                    .filter_map(|(_, x)| x.item.clone())
                    .collect();
                app.overlay = Overlay::ApplyFixes {
                    items,
                    bytes,
                    risky,
                };
            }
        }
        _ => {}
    }
    false
}

pub fn run(root: &str) -> Result<()> {
    let mut app = App::new(root);
    app.reload();
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
        prev(info);
    }));
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = disable_raw_mode();
            let _ = execute!(stdout(), LeaveAlternateScreen);
        }
    }
    let _guard = Guard;
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    let mut term = Terminal::new(CrosstermBackend::new(stdout()))?;
    loop {
        app.poll();
        if let Some(j) = app.v.job.as_mut() {
            j.poll();
        }
        if app.tab == ACTIVITY
            && app
                .last_sample
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
        {
            app.live = app.sampler.sample();
            app.last_sample = Some(Instant::now());
        }
        term.draw(|f| draw(f, &mut app))?;
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press && key(&mut app, k.code, k.modifiers) {
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    #[test]
    fn browse_projects_and_fixes_work_end_to_end() {
        let d = std::env::temp_dir().join(format!("dux-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("shop/web/node_modules/x")).unwrap();
        std::fs::write(
            d.join("shop/web/package.json"),
            br#"{"name":"web","description":"shop front"}"#,
        )
        .unwrap();
        std::fs::write(d.join("shop/web/node_modules/x/a.js"), vec![1u8; 2 << 20]).unwrap();
        std::fs::create_dir_all(d.join("logs")).unwrap();
        std::fs::write(d.join("logs/a.log.1"), vec![1u8; 2 << 20]).unwrap();
        std::fs::write(d.join("big.bin"), vec![1u8; 8 << 20]).unwrap();
        let root = d.to_string_lossy().into_owned();
        let mut app = App::new(&root);
        app.data = gather::gather(&root, 2, None);
        app.loading = false;
        app.picked = vec![false; app.data.fixes.len()];
        app.list_dir();
        let names: Vec<&str> = app.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names[0], "big.bin",
            "files and folders, biggest first: {names:?}"
        );
        assert!(names.contains(&"shop") && names.contains(&"logs"));
        let mut term = Terminal::new(TestBackend::new(140, 45)).unwrap();
        for tab in 0..8 {
            app.tab = tab;
            term.draw(|f| draw(f, &mut app)).unwrap();
        }
        app.tab = BROWSE;
        app.sel[BROWSE] = app.entries.iter().position(|e| e.name == "shop").unwrap();
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.cwd.ends_with("/shop"));
        key(&mut app, KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(app.cwd, root);
        assert_eq!(
            app.selected_entry().map(|e| e.name.as_str()),
            Some("shop"),
            "back to the folder we came from"
        );
        key(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
        assert!(matches!(app.overlay, Overlay::Info(_)));
        term.draw(|f| draw(f, &mut app)).unwrap();
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.overlay, Overlay::None));
        app.tab = PROJECTS;
        assert_eq!(app.data.projects.len(), 1);
        app.sel[PROJECTS] = 1; // row 0 is the location group
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.project_open.is_some());
        key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
        assert_eq!(app.project_marks[0], Mark::Remove);
        term.draw(|f| draw(f, &mut app)).unwrap();
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.overlay, Overlay::ApplyProject { .. }));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.message, "Nothing removed.");
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.project_open.is_none());
        app.tab = FIXES;
        key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(app.picked_total().0 >= 2);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.overlay, Overlay::ApplyFixes { .. }));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.message, "Nothing applied.");
        app.tab = BROWSE;
        app.cwd = "/".into();
        app.list_dir();
        app.sel[BROWSE] = app
            .entries
            .iter()
            .position(|e| e.name == "etc")
            .unwrap_or(0);
        key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(
            matches!(app.overlay, Overlay::None),
            "no delete prompt for a top-level folder"
        );
        assert!(key(&mut app, KeyCode::Char('q'), KeyModifiers::NONE));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_delete_prompt_stays_bound_to_what_it_showed() {
        let d = std::env::temp_dir().join(format!("dux-ui-bind-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("sub/small.txt"), b"x").unwrap();
        std::fs::create_dir_all(d.join("sub/bigdir")).unwrap();
        std::fs::write(d.join("sub/bigdir/data"), vec![1u8; 1 << 20]).unwrap();
        let sub = d.join("sub").to_string_lossy().into_owned();
        let mut app = App::new(&sub);
        app.loading = false;
        app.list_dir();
        app.sel[BROWSE] = app
            .entries
            .iter()
            .position(|e| e.name == "small.txt")
            .unwrap();
        key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(matches!(&app.overlay, Overlay::Delete(e) if e.name == "small.txt"));
        // A scan finishing re-sorts the list under the open prompt.
        app.entries.reverse();
        assert!(matches!(&app.overlay, Overlay::Delete(e) if e.name == "small.txt"));
        // Replaced on disk since the prompt opened: nothing is deleted.
        std::fs::remove_file(d.join("sub/small.txt")).unwrap();
        std::fs::write(d.join("sub/other"), b"y").unwrap();
        std::fs::write(d.join("sub/small.txt"), b"new").unwrap();
        key(&mut app, KeyCode::Char('D'), KeyModifiers::NONE);
        assert!(
            app.message.contains("changed since the prompt opened"),
            "{}",
            app.message
        );
        assert!(d.join("sub/small.txt").exists() && d.join("sub/bigdir/data").exists());
        std::fs::remove_dir_all(&d).unwrap();
    }
    #[test]
    fn every_view_renders_at_every_size_with_huge_and_odd_data() {
        use std::os::unix::ffi::OsStrExt;
        let d = std::env::temp_dir().join(format!("dux-ui-sizes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("many")).unwrap();
        for i in 0..1500 {
            std::fs::write(d.join(format!("many/f{i:04}")), b"x").unwrap();
        }
        // Names that are not valid UTF-8, with control characters.
        let bad = std::ffi::OsStr::from_bytes(b"bad\xff\xfename\n\x1b[31mred");
        std::fs::write(d.join("many").join(bad), b"y").unwrap();
        std::fs::create_dir_all(d.join("p/node_modules/x")).unwrap();
        std::fs::write(
            d.join("p/package.json"),
            br#"{"name":"\u001b[2Jevil\nname"}"#,
        )
        .unwrap();
        let root = d.to_string_lossy().into_owned();
        let mut app = App::new(&root);
        app.data = gather::gather(&root, 2, None);
        let mut r = crate::docker::Report::default();
        for i in 0..1000 {
            r.containers.push(crate::docker::Container {
                runtime: if i % 3 == 0 { "podman" } else { "docker" },
                name: format!("c{i}-{}", "x".repeat(i % 50)),
                image: format!("img{}", i % 40),
                state: if i % 2 == 0 {
                    "running".into()
                } else {
                    "exited".into()
                },
                project: (i % 5 != 0).then(|| format!("stack{}", i % 17)),
                mounts: vec![format!("vol{}…", i % 30)],
                writable: i as u64 * 1000,
                log_bytes: i as u64 * 7000,
                ..Default::default()
            });
        }
        for i in 0..40 {
            r.images.push(crate::docker::Image {
                name: format!("img{i}"),
                unique: i * 100,
                ..Default::default()
            });
        }
        for i in 0..300 {
            r.volumes.push(crate::docker::Volume {
                name: format!("vol{i}-{}", "y".repeat(60)),
                size: i * 10,
                ..Default::default()
            });
        }
        r.on_disk.push(("/var/lib/docker/overlay2".into(), 5));
        app.data.docker = Some(r);
        app.loading = false;
        app.picked = vec![false; app.data.fixes.len()];
        app.cwd = d.join("many").to_string_lossy().into_owned();
        app.list_dir();
        assert!(app.entries.len() >= 1500);
        let sizes = [
            (1u16, 1u16),
            (2, 2),
            (10, 3),
            (20, 5),
            (40, 10),
            (60, 15),
            (80, 24),
            (100, 30),
            (132, 43),
            (200, 60),
            (250, 80),
            (300, 4),
            (5, 100),
        ];
        for (w, h) in sizes {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            for tab in 0..8 {
                app.tab = tab;
                for sub in 0..views::DSUB.len() {
                    app.v.dsub = sub;
                    app.sel[DOCKER] = 999;
                    term.draw(|f| draw(f, &mut app)).unwrap();
                    if tab != DOCKER {
                        break;
                    }
                }
            }
            // Overlays and the job window at this size.
            app.tab = BROWSE;
            app.sel[BROWSE] = 0;
            for o in 0..3 {
                app.overlay = match o {
                    0 => Overlay::Info(app.entries[0].clone()),
                    1 => Overlay::Delete(app.entries[0].clone()),
                    _ => Overlay::ApplyFixes {
                        items: vec![
                            Item {
                                level: 1,
                                what: "w".repeat(500),
                                action: Action::Run {
                                    program: "true".into(),
                                    args: vec![]
                                },
                                frees: 1
                            };
                            30
                        ],
                        bytes: 1,
                        risky: true,
                    },
                };
                term.draw(|f| draw(f, &mut app)).unwrap();
            }
            app.overlay = Overlay::None;
        }
        std::fs::remove_dir_all(&d).unwrap();
    }
}

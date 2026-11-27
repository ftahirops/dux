//! `dux ui`: terminal UI on the 0.7 design.
//!
//! Tabs: 1 Disks, 2 Folders, 3 Docker, 4 Logs, 5 Projects, 6 Fixes.
//! Data comes from one background `gather` (disks show at once; the rest
//! fills in when the walk finishes). In Fixes: Space toggles, s selects all
//! SAFE, a selects SAFE+CAUTION, c clears, Enter applies after a confirm
//! screen; RISKY items need D to confirm.

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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::Duration;

const TABS: [&str; 6] = [
    "1 Disks",
    "2 Folders",
    "3 Docker",
    "4 Logs",
    "5 Projects",
    "6 Fixes",
];

#[derive(PartialEq)]
enum Confirm {
    None,
    Apply,
}

struct App {
    root: String,
    tab: usize,
    data: Gathered,
    loading: bool,
    progress: Arc<AtomicU64>,
    rx: Option<Receiver<Gathered>>,
    /// Folder navigation: child indexes from the tree root.
    path: Vec<usize>,
    sel: [usize; 6],
    picked: Vec<bool>,
    confirm: Confirm,
    message: String,
}

impl App {
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
    }

    fn poll(&mut self) {
        if let Some(rx) = &self.rx {
            if let Ok(g) = rx.try_recv() {
                self.picked = vec![false; g.fixes.len()];
                self.data = g;
                self.loading = false;
                self.rx = None;
                self.path.clear();
            }
        }
    }

    fn folder(&self) -> Option<&DirNode> {
        let mut n = &self.data.tree.as_ref()?.root;
        for &i in &self.path {
            n = n.children.get(i)?;
        }
        Some(n)
    }

    fn folder_children(&self) -> Vec<(usize, &DirNode)> {
        let mut v: Vec<(usize, &DirNode)> = self
            .folder()
            .map(|n| n.children.iter().enumerate().collect())
            .unwrap_or_default();
        v.sort_by_key(|(_, c)| std::cmp::Reverse(c.total_bytes));
        v
    }

    fn folder_path(&self) -> String {
        let Some(t) = &self.data.tree else {
            return self.root.clone();
        };
        let mut n = &t.root;
        let mut p = t.root_path.trim_end_matches('/').to_string();
        for &i in &self.path {
            n = &n.children[i];
            p.push('/');
            p.push_str(&n.name);
        }
        if p.is_empty() {
            "/".into()
        } else {
            p
        }
    }

    fn rows(&self) -> usize {
        match self.tab {
            0 => self.data.disks.len(),
            1 => self.folder_children().len(),
            2 => self
                .data
                .docker
                .as_ref()
                .map_or(0, |d| d.containers.len() + d.images.len() + d.volumes.len()),
            3 => self.data.logs.len(),
            4 => self.data.projects.len() + self.data.apps.len(),
            _ => self.data.fixes.len(),
        }
    }

    fn picked_total(&self) -> (usize, u64, bool) {
        let mut n = 0;
        let mut b = 0;
        let mut risky = false;
        for (i, x) in self.data.fixes.iter().enumerate() {
            if self.picked.get(i).copied().unwrap_or(false) {
                n += 1;
                b += x.frees;
                risky |= x.safety == Safety::Risky;
            }
        }
        (n, b, risky)
    }

    fn apply(&mut self) {
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            self.message = "Applying fixes needs root: run sudo dux ui".into();
            return;
        }
        let (mut ok, mut failed, mut freed) = (0, 0, 0u64);
        for (i, x) in self.data.fixes.iter().enumerate() {
            if !self.picked[i] {
                continue;
            }
            if let Some(item) = &x.item {
                match crate::cleanup::apply(item) {
                    Ok(()) => {
                        ok += 1;
                        freed += x.frees;
                    }
                    Err(_) => failed += 1,
                }
            }
        }
        self.message = format!(
            "Applied {ok} fixes, about {} freed, {failed} skipped. Reloading...",
            human(freed as i64)
        );
        self.reload();
    }
}

fn bar(frac: f64, width: usize) -> String {
    let n = ((frac.clamp(0.0, 1.0)) * width as f64).round() as usize;
    format!("{}{}", "█".repeat(n), "░".repeat(width - n))
}

fn color(s: Safety) -> Color {
    match s {
        Safety::Safe => Color::Green,
        Safety::Caution => Color::Yellow,
        Safety::Risky => Color::Red,
    }
}

fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(3),
    ])
    .split(area);
    let tabs = Tabs::new(TABS.to_vec())
        .select(app.tab)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" dux · {} ", app.root)),
        )
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan));
    f.render_widget(tabs, chunks[0]);

    let mut state = TableState::default();
    state.select(Some(app.sel[app.tab].min(app.rows().saturating_sub(1))));
    let body = chunks[1];
    let hdr = |cells: &[&str]| {
        Row::new(cells.iter().map(|c| c.to_string()).collect::<Vec<_>>())
            .style(Style::default().add_modifier(Modifier::BOLD))
    };
    let hl = Style::default().bg(Color::DarkGray);
    if app.loading && app.tab != 0 {
        let msg = format!(
            "Walking {} ... {} entries so far. Disks are ready on tab 1.",
            app.root,
            app.progress.load(Ordering::Relaxed)
        );
        f.render_widget(
            Paragraph::new(msg).block(Block::default().borders(Borders::ALL)),
            body,
        );
    } else {
        match app.tab {
            0 => {
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
                            human(d.usage.avail as i64),
                            format!("{} {p:>5.1}%", bar(p / 100.0, 20)),
                            d.usage
                                .inode_pct()
                                .map(|x| format!("{x:.1}%"))
                                .unwrap_or_else(|| "-".into()),
                        ])
                        .style(Style::default().fg(c))
                    })
                    .collect();
                let t = Table::new(
                    rows,
                    [
                        Constraint::Percentage(28),
                        Constraint::Length(10),
                        Constraint::Length(11),
                        Constraint::Length(11),
                        Constraint::Length(28),
                        Constraint::Length(8),
                    ],
                )
                .header(hdr(&["mount", "type", "size", "free", "used", "inodes"]))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Disks (live) "),
                )
                .row_highlight_style(hl);
                f.render_stateful_widget(t, body, &mut state);
            }
            1 => {
                let parts =
                    Layout::vertical([Constraint::Min(5), Constraint::Length(8)]).split(body);
                let cur = app.folder().map(|n| n.total_bytes).unwrap_or(1).max(1);
                let rows: Vec<Row> = app
                    .folder_children()
                    .iter()
                    .map(|(_, c)| {
                        Row::new(vec![
                            human(c.total_bytes as i64),
                            format!(
                                "{} {:>5.1}%",
                                bar(c.total_bytes as f64 / cur as f64, 16),
                                c.total_bytes as f64 * 100.0 / cur as f64
                            ),
                            c.total_entries.to_string(),
                            format!("{}{}", c.name, if c.children.is_empty() { "" } else { "/" }),
                        ])
                    })
                    .collect();
                let t = Table::new(
                    rows,
                    [
                        Constraint::Length(11),
                        Constraint::Length(24),
                        Constraint::Length(10),
                        Constraint::Min(10),
                    ],
                )
                .header(hdr(&["size", "share", "entries", "name"]))
                .block(Block::default().borders(Borders::ALL).title(format!(
                    " {}  (Enter/→ open, ←/Backspace up) ",
                    app.folder_path()
                )))
                .row_highlight_style(hl);
                f.render_stateful_widget(t, parts[0], &mut state);
                let tops: Vec<Line> = app
                    .folder()
                    .map(|n| {
                        n.top_files
                            .iter()
                            .map(|t| {
                                Line::from(format!("{:>11}  {}", human(t.bytes as i64), t.name))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                f.render_widget(
                    Paragraph::new(tops).block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Largest files here "),
                    ),
                    parts[1],
                );
            }
            2 => {
                let Some(d) = &app.data.docker else {
                    f.render_widget(
                        Paragraph::new("Docker is not available on this host.")
                            .block(Block::default().borders(Borders::ALL)),
                        body,
                    );
                    return draw_footer(f, app, chunks[2]);
                };
                let mut rows: Vec<Row> = Vec::new();
                for c in &d.containers {
                    rows.push(Row::new(vec![
                        "container".into(),
                        human((c.writable + c.log_bytes) as i64),
                        format!("{} ({} log)", c.name, human(c.log_bytes as i64)),
                        format!("{} {}", c.state, c.project.clone().unwrap_or_default()),
                    ]));
                }
                for i in &d.images {
                    rows.push(Row::new(vec![
                        "image".into(),
                        human(i.unique as i64),
                        i.name.clone(),
                        if i.containers == 0 {
                            "UNUSED".into()
                        } else {
                            format!("{} container(s)", i.containers)
                        },
                    ]));
                }
                for v in &d.volumes {
                    rows.push(Row::new(vec![
                        "volume".into(),
                        human(v.size as i64),
                        v.name.clone(),
                        if v.links == 0 {
                            "UNUSED".into()
                        } else {
                            v.project.clone().unwrap_or_default()
                        },
                    ]));
                }
                let title = format!(
                    " Docker: images {} · containers {} · logs {} · volumes {} · build cache {} ",
                    human(d.images_total() as i64),
                    human(d.writable_total() as i64),
                    human(d.logs_total() as i64),
                    human(d.volumes_total() as i64),
                    human(d.build_cache as i64)
                );
                let t = Table::new(
                    rows,
                    [
                        Constraint::Length(10),
                        Constraint::Length(11),
                        Constraint::Min(20),
                        Constraint::Length(30),
                    ],
                )
                .header(hdr(&["kind", "size", "name", "status"]))
                .block(Block::default().borders(Borders::ALL).title(title))
                .row_highlight_style(hl);
                f.render_stateful_widget(t, body, &mut state);
            }
            3 => {
                let now = crate::util::now_secs();
                let rows: Vec<Row> = app
                    .data
                    .logs
                    .iter()
                    .map(|l| {
                        let mut note = String::new();
                        if l.rotated {
                            note.push_str("rotated ");
                        }
                        if l.protected {
                            note.push_str("protected ");
                        }
                        if let Some(c) = &l.container {
                            note.push_str(c);
                        }
                        if let Some(w) = &l.writer {
                            note.push_str(&format!(" by {w}"));
                        }
                        Row::new(vec![
                            human(l.bytes as i64),
                            crate::insight::age_text(l.mtime, now),
                            l.kind.label().to_string(),
                            l.path.clone(),
                            note,
                        ])
                    })
                    .collect();
                let total: u64 = app.data.logs.iter().map(|l| l.bytes).sum();
                let t = Table::new(
                    rows,
                    [
                        Constraint::Length(11),
                        Constraint::Length(5),
                        Constraint::Length(7),
                        Constraint::Min(30),
                        Constraint::Length(30),
                    ],
                )
                .header(hdr(&["size", "age", "kind", "path", "note"]))
                .block(Block::default().borders(Borders::ALL).title(format!(
                        " Logs: {} in {} files; journal {} ",
                        human(total as i64),
                        app.data.logs.len(),
                        app.data
                            .journal_bytes
                            .map(|j| human(j as i64))
                            .unwrap_or_else(|| "-".into())
                    )))
                .row_highlight_style(hl);
                f.render_stateful_widget(t, body, &mut state);
            }
            4 => {
                let now = crate::util::now_secs();
                let mut rows: Vec<Row> = app
                    .data
                    .projects
                    .iter()
                    .map(|p| {
                        Row::new(vec![
                            human(p.total as i64),
                            human(p.artifacts as i64),
                            human(p.git as i64),
                            format!("{}d", (now - p.last_activity).max(0) / 86_400),
                            p.kinds.join(","),
                            p.path.clone(),
                        ])
                    })
                    .collect();
                rows.extend(app.data.apps.iter().map(|(n, p, b, t)| {
                    Row::new(vec![
                        human(*b as i64),
                        "".into(),
                        "".into(),
                        crate::insight::age_text(*t, now),
                        format!("app: {n}"),
                        p.clone(),
                    ])
                    .style(Style::default().fg(Color::Cyan))
                }));
                let t = Table::new(
                    rows,
                    [
                        Constraint::Length(11),
                        Constraint::Length(11),
                        Constraint::Length(10),
                        Constraint::Length(6),
                        Constraint::Length(22),
                        Constraint::Min(20),
                    ],
                )
                .header(hdr(&["total", "build-out", "git", "idle", "kind", "path"]))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Projects and apps "),
                )
                .row_highlight_style(hl);
                f.render_stateful_widget(t, body, &mut state);
            }
            _ => {
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
                let parts =
                    Layout::vertical([Constraint::Min(5), Constraint::Length(5)]).split(body);
                let (n, b, _) = app.picked_total();
                let t = Table::new(rows, [Constraint::Length(4), Constraint::Length(8), Constraint::Length(11), Constraint::Min(30)])
                    .header(hdr(&["", "rating", "frees", "fix"]))
                    .block(Block::default().borders(Borders::ALL).title(format!(" Fixes: {n} picked, {} · Space toggle · s safe · a all · c clear · Enter apply ", human(b as i64))))
                    .row_highlight_style(hl);
                f.render_stateful_widget(t, parts[0], &mut state);
                let detail = app
                    .data
                    .fixes
                    .get(app.sel[5])
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
        }
    }
    draw_footer(f, app, chunks[2]);
    if app.confirm == Confirm::Apply {
        let (n, b, risky) = app.picked_total();
        let w = area.width.min(70);
        let r = Rect {
            x: area.x + (area.width - w) / 2,
            y: area.y + area.height / 3,
            width: w,
            height: 7,
        };
        f.render_widget(Clear, r);
        let text = if risky {
            format!("Apply {n} fixes, freeing about {}?\n\nThe selection includes RISKY items that can lose data.\nPress D to apply, Esc to cancel.", human(b as i64))
        } else {
            format!(
                "Apply {n} fixes, freeing about {}?\n\nPress y to apply, Esc to cancel.",
                human(b as i64)
            )
        };
        f.render_widget(
            Paragraph::new(text).block(Block::default().borders(Borders::ALL).title(" Confirm ")),
            r,
        );
    }
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let root = app
        .data
        .disks
        .iter()
        .find(|d| d.path == "/")
        .map(|d| d.usage.used_pct())
        .unwrap_or(0.0);
    let msg = if app.message.is_empty() {
        "q quit · 1-6 or Tab switch · ↑↓ move · r reload".to_string()
    } else {
        app.message.clone()
    };
    let g = Gauge::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" / {root:.1}% used · {msg} ")),
        )
        .gauge_style(Style::default().fg(if root >= 95.0 {
            Color::Red
        } else if root >= 85.0 {
            Color::Yellow
        } else {
            Color::Green
        }))
        .ratio((root / 100.0).clamp(0.0, 1.0));
    f.render_widget(g, area);
}

/// Returns true to quit.
fn key(app: &mut App, code: KeyCode, mods: KeyModifiers) -> bool {
    if code == KeyCode::Char('c') && mods.contains(KeyModifiers::CONTROL) {
        return true;
    }
    if app.confirm == Confirm::Apply {
        let (_, _, risky) = app.picked_total();
        match code {
            KeyCode::Char('D') if risky => {
                app.confirm = Confirm::None;
                app.apply();
            }
            KeyCode::Char('y') | KeyCode::Char('Y') if !risky => {
                app.confirm = Confirm::None;
                app.apply();
            }
            KeyCode::Esc | KeyCode::Char('n') => {
                app.confirm = Confirm::None;
                app.message = "Nothing applied.".into();
            }
            _ => {}
        }
        return false;
    }
    let rows = app.rows();
    let t = app.tab;
    match code {
        KeyCode::Char('q') | KeyCode::Esc => return true,
        KeyCode::Char(c @ '1'..='6') => app.tab = (c as usize) - ('1' as usize),
        KeyCode::Tab => app.tab = (app.tab + 1) % TABS.len(),
        KeyCode::BackTab => app.tab = (app.tab + TABS.len() - 1) % TABS.len(),
        KeyCode::Down | KeyCode::Char('j') => {
            app.sel[t] = (app.sel[t] + 1).min(rows.saturating_sub(1))
        }
        KeyCode::Up | KeyCode::Char('k') => app.sel[t] = app.sel[t].saturating_sub(1),
        KeyCode::PageDown => app.sel[t] = (app.sel[t] + 20).min(rows.saturating_sub(1)),
        KeyCode::PageUp => app.sel[t] = app.sel[t].saturating_sub(20),
        KeyCode::Char('r') => {
            app.message = "Reloading...".into();
            app.reload();
        }
        KeyCode::Enter | KeyCode::Right if app.tab == 1 => {
            let kids = app.folder_children();
            if let Some(&(i, c)) = kids.get(app.sel[1]) {
                if !c.children.is_empty() {
                    app.path.push(i);
                    app.sel[1] = 0;
                }
            }
        }
        KeyCode::Left | KeyCode::Backspace if app.tab == 1 => {
            app.path.pop();
            app.sel[1] = 0;
        }
        KeyCode::Char(' ') if app.tab == 5 => {
            let i = app.sel[5];
            if app.data.fixes.get(i).is_some_and(|x| x.item.is_some()) {
                app.picked[i] = !app.picked[i];
            }
        }
        KeyCode::Char('s') if app.tab == 5 => {
            for (i, x) in app.data.fixes.iter().enumerate() {
                app.picked[i] = x.item.is_some() && x.safety == Safety::Safe;
            }
        }
        KeyCode::Char('a') if app.tab == 5 => {
            for (i, x) in app.data.fixes.iter().enumerate() {
                app.picked[i] = x.item.is_some() && x.safety != Safety::Risky;
            }
        }
        KeyCode::Char('c') if app.tab == 5 => app.picked.iter_mut().for_each(|p| *p = false),
        KeyCode::Enter if app.tab == 5 => {
            if app.picked_total().0 == 0 {
                app.message = "Pick fixes first: Space, s (safe) or a (all).".into();
            } else {
                app.confirm = Confirm::Apply;
            }
        }
        _ => {}
    }
    false
}

pub fn run(root: &str) -> Result<()> {
    let mut app = App {
        root: root.to_string(),
        tab: 0,
        data: Gathered::default(),
        loading: true,
        progress: Arc::new(AtomicU64::new(0)),
        rx: None,
        path: Vec::new(),
        sel: [0; 6],
        picked: Vec::new(),
        confirm: Confirm::None,
        message: String::new(),
    };
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

    fn app_with(g: Gathered) -> App {
        let n = g.fixes.len();
        App {
            root: "/".into(),
            tab: 0,
            data: g,
            loading: false,
            progress: Arc::new(AtomicU64::new(0)),
            rx: None,
            path: Vec::new(),
            sel: [0; 6],
            picked: vec![false; n],
            confirm: Confirm::None,
            message: String::new(),
        }
    }

    #[test]
    fn every_tab_renders_and_fix_keys_work() {
        let d = std::env::temp_dir().join(format!("dux-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("p/node_modules/x")).unwrap();
        std::fs::write(d.join("p/package.json"), b"{}").unwrap();
        std::fs::write(d.join("p/node_modules/x/a.js"), vec![1u8; 2 << 20]).unwrap();
        std::fs::create_dir_all(d.join("logs")).unwrap();
        std::fs::write(d.join("logs/a.log.1"), vec![1u8; 2 << 20]).unwrap();
        let mut app = app_with(gather::gather(d.to_str().unwrap(), 2, None));
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        for tab in 0..6 {
            app.tab = tab;
            term.draw(|f| draw(f, &mut app)).unwrap();
        }
        // Folders: open the biggest child and come back.
        app.tab = 1;
        assert!(!key(&mut app, KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.path.len(), 1);
        key(&mut app, KeyCode::Left, KeyModifiers::NONE);
        assert!(app.path.is_empty());
        // Fixes: select all, confirm screen, cancel.
        app.tab = 5;
        key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(app.picked_total().0 >= 2);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.confirm == Confirm::Apply);
        term.draw(|f| draw(f, &mut app)).unwrap();
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.confirm == Confirm::None && app.message == "Nothing applied.");
        key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
        assert_eq!(app.picked_total().0, 0);
        assert!(key(&mut app, KeyCode::Char('q'), KeyModifiers::NONE));
        std::fs::remove_dir_all(&d).unwrap();
    }
}

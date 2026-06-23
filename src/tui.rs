use crate::store::{PathResolver, Store};
use crate::util::{ago, human};
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    prelude::*,
    widgets::{Block, BorderType, Borders, Paragraph},
};
use rusqlite::params;
use std::io::stdout;
use std::time::{Duration, Instant};

/// What the tree graph visualizes.
#[derive(Clone, Copy, PartialEq)]
enum Metric {
    Size,
    Inodes,
}

/// One visible row in the expandable tree.
struct Row {
    dev: i64,
    inode: i64,
    name: String,
    kind: char,
    size: i64,   // recursive bytes for dirs, own for files
    inodes: i64, // recursive inode count for dirs, 1 for files
    depth: usize,
    expanded: bool,
    has_children: bool,
    ratio_size: f64,   // share of largest sibling, by bytes
    ratio_inodes: f64, // share of largest sibling, by inode count
    growth: i64,
}

struct App {
    rows: Vec<Row>,
    expanded: std::collections::HashSet<(i64, i64)>,
    sel: usize,
    root_dev: i64,
    root_inode: i64,
    top_growth: Vec<(String, i64)>,
    top_files: Vec<(String, i64)>,
    total_size: i64,
    last_scan: i64,
    window_secs: i64,
    last_reload: Instant,
    scroll: usize,
    metric: Metric,
    root_path: String,
    fs: crate::util::FsStat,
    daemon_live: bool,
    // recursive write-rate per node (bytes in the last hour), summed up the tree
    growth_map: std::collections::HashMap<(i64, i64), i64>,
    growth_calc: Instant,
    items: i64,          // total indexed nodes (files + dirs)
    growth_per_day: i64, // extrapolated from the last hour of change log
}

/// WinDirStat-style live tree: folders expand inline beneath their parent (the
/// parent stays visible), indented, with a per-row heat bar (RED = hot). Opens
/// at `start` (dev,inode) — the scoped path or the index root.
pub fn run(store: &Store, start: Option<(i64, i64)>) -> Result<()> {
    let root = start.or_else(|| root_node(store));
    let (dev, inode) = match root {
        Some(v) => v,
        None => {
            println!("empty index — run `dux scan <PATH>` first");
            return Ok(());
        }
    };

    let mut app = App {
        rows: Vec::new(),
        expanded: std::collections::HashSet::new(),
        sel: 0,
        root_dev: dev,
        root_inode: inode,
        top_growth: Vec::new(),
        top_files: Vec::new(),
        total_size: 0,
        last_scan: 0,
        window_secs: 3600,
        last_reload: Instant::now(),
        scroll: 0,
        metric: Metric::Size,
        root_path: store.path_of(dev, inode).unwrap_or_else(|_| "/".into()),
        fs: crate::util::FsStat::default(),
        daemon_live: false,
        growth_map: std::collections::HashMap::new(),
        growth_calc: Instant::now() - Duration::from_secs(60),
        items: 0,
        growth_per_day: 0,
    };
    app.init_root(store)?;
    app.refresh_panels(store)?;

    // Restore the terminal even on panic (panic=abort still runs hooks).
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
        prev_hook(info);
    }));
    // RAII guard restores the terminal on any return path.
    struct TermGuard;
    impl Drop for TermGuard {
        fn drop(&mut self) {
            let _ = disable_raw_mode();
            let _ = execute!(stdout(), LeaveAlternateScreen);
        }
    }

    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    let _guard = TermGuard;
    let mut term = Terminal::new(CrosstermBackend::new(stdout()))?;
    let res = event_loop(&mut term, &mut app, store);
    term.show_cursor().ok();
    res
}

/// Resolve the indexed root from stored meta, else a self-parent fallback.
fn root_node(store: &Store) -> Option<(i64, i64)> {
    let dev: Option<i64> = store
        .get_meta("root_dev")
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok());
    let ino: Option<i64> = store
        .get_meta("root_inode")
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok());
    if let (Some(d), Some(i)) = (dev, ino) {
        return Some((d, i));
    }
    store
        .conn
        .query_row(
            "SELECT dev_id, inode FROM nodes WHERE inode=parent_inode
             ORDER BY recursive_bytes DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()
}

impl App {
    fn init_root(&mut self, store: &Store) -> Result<()> {
        self.expanded.insert((self.root_dev, self.root_inode)); // root starts open
        self.rebuild(store)?;
        self.sel = 0;
        Ok(())
    }

    /// Rebuild the entire visible row list from the `expanded` set, re-querying
    /// fresh sizes/order/children/growth. This is the live-refresh primitive:
    /// new files appear, deleted vanish, bars/%/order all update. Selection is
    /// preserved by (dev,inode).
    /// Recursive write-rate per node (bytes in the last hour) propagated up the
    /// tree from the change log, in ONE query. Cached ~3s so expanding stays
    /// snappy. This is why a directory shows its subtree's activity, not 0.
    fn refresh_growth_map(&mut self, store: &Store) {
        if self.growth_calc.elapsed() < Duration::from_secs(3) {
            return;
        }
        let cutoff = crate::util::now_secs() - 3600;
        let mut map = std::collections::HashMap::new();
        if let Ok(mut stmt) = store.conn.prepare(
            "WITH RECURSIVE
               chg(dev,ino,d) AS (
                 SELECT dev_id,inode,SUM(delta) FROM changes WHERE ts>=?1 GROUP BY dev_id,inode
               ),
               anc(dev,ino,d) AS (
                 SELECT dev,ino,d FROM chg
                 UNION ALL
                 SELECT n.parent_dev,n.parent_inode,a.d FROM anc a
                 JOIN nodes n ON n.dev_id=a.dev AND n.inode=a.ino
                 WHERE NOT (n.dev_id=n.parent_dev AND n.inode=n.parent_inode)
               )
             SELECT dev,ino,SUM(d) FROM anc GROUP BY dev,ino",
        ) {
            if let Ok(rows) = stmt.query_map(params![cutoff], |r| {
                Ok((
                    (r.get::<_, i64>(0)?, r.get::<_, i64>(1)?),
                    r.get::<_, i64>(2)?,
                ))
            }) {
                for x in rows.flatten() {
                    map.insert(x.0, x.1);
                }
            }
        }
        self.growth_map = map;
        self.growth_calc = Instant::now();
    }

    fn rebuild(&mut self, store: &Store) -> Result<()> {
        self.refresh_growth_map(store);
        let sel_id = self.rows.get(self.sel).map(|r| (r.dev, r.inode));
        let mut out: Vec<Row> = Vec::new();

        // root row
        let (name, size, inodes): (String, i64, i64) = store
            .conn
            .query_row(
                "SELECT name, recursive_bytes, recursive_inodes FROM nodes WHERE dev_id=?1 AND inode=?2",
                params![self.root_dev, self.root_inode],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap_or_else(|_| (self.root_path.clone(), 0, 0));
        let root_expanded = self.expanded.contains(&(self.root_dev, self.root_inode));
        out.push(Row {
            dev: self.root_dev,
            inode: self.root_inode,
            name,
            kind: 'd',
            size,
            inodes,
            depth: 0,
            expanded: root_expanded,
            has_children: true,
            ratio_size: 1.0,
            ratio_inodes: 1.0,
            growth: self
                .growth_map
                .get(&(self.root_dev, self.root_inode))
                .copied()
                .unwrap_or(0),
        });
        if root_expanded {
            self.append_children(store, &mut out, self.root_dev, self.root_inode, 1)?;
        }

        self.rows = out;
        // restore selection by identity
        if let Some(id) = sel_id {
            self.sel = self
                .rows
                .iter()
                .position(|r| (r.dev, r.inode) == id)
                .unwrap_or_else(|| self.sel.min(self.rows.len().saturating_sub(1)));
        }
        if self.sel >= self.rows.len() {
            self.sel = self.rows.len().saturating_sub(1);
        }
        Ok(())
    }

    /// Recursively append a directory's children (and any expanded descendants)
    /// to `out`, computing per-sibling-group ratios and growth.
    fn append_children(
        &self,
        store: &Store,
        out: &mut Vec<Row>,
        dev: i64,
        inode: i64,
        depth: usize,
    ) -> Result<()> {
        let order = if self.metric == Metric::Inodes {
            "recursive_inodes"
        } else {
            "CASE WHEN kind='d' THEN recursive_bytes ELSE size END"
        };
        let sql = format!(
            "SELECT dev_id, inode, name, kind, size, recursive_bytes, recursive_inodes
             FROM nodes WHERE parent_dev=?1 AND parent_inode=?2
               AND NOT (dev_id=?1 AND inode=?2) AND deleted=0
             ORDER BY {order} DESC LIMIT 5000"
        );
        let mut stmt = store.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![dev, inode], |r| {
            let kind: String = r.get(3)?;
            let k = kind.chars().next().unwrap_or('?');
            let size = if k == 'd' {
                r.get::<_, i64>(5)?
            } else {
                r.get::<_, i64>(4)?
            };
            let inodes = if k == 'd' { r.get::<_, i64>(6)? } else { 1 };
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                k,
                size,
                inodes,
            ))
        })?;
        let mut kids: Vec<(i64, i64, String, char, i64, i64)> = Vec::new();
        for row in rows {
            kids.push(row?);
        }
        if kids.is_empty() {
            return Ok(());
        }
        let maxs = kids.iter().map(|k| k.4).max().unwrap_or(1).max(1);
        let maxi = kids.iter().map(|k| k.5).max().unwrap_or(1).max(1);

        for (cdev, cino, name, kind, size, inodes) in kids {
            let is_expanded = kind == 'd' && self.expanded.contains(&(cdev, cino));
            out.push(Row {
                dev: cdev,
                inode: cino,
                name,
                kind,
                size,
                inodes,
                depth,
                expanded: is_expanded,
                has_children: kind == 'd',
                ratio_size: size as f64 / maxs as f64,
                ratio_inodes: inodes as f64 / maxi as f64,
                // recursive write-rate (subtree), from the cached growth map
                growth: self.growth_map.get(&(cdev, cino)).copied().unwrap_or(0),
            });
            if is_expanded {
                self.append_children(store, out, cdev, cino, depth + 1)?;
            }
        }
        Ok(())
    }

    fn toggle(&mut self, store: &Store) -> Result<()> {
        let r = &self.rows[self.sel];
        if r.kind != 'd' {
            return Ok(());
        }
        let id = (r.dev, r.inode);
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
        self.rebuild(store)
    }

    /// Move to the parent of the selection and collapse it.
    fn ascend(&mut self, store: &Store) -> Result<()> {
        let depth = self.rows[self.sel].depth;
        if depth == 0 {
            return Ok(());
        }
        let mut i = self.sel;
        while i > 0 && self.rows[i].depth >= depth {
            i -= 1;
        }
        let id = (self.rows[i].dev, self.rows[i].inode);
        self.expanded.remove(&id);
        self.rebuild(store)?;
        self.sel = self
            .rows
            .iter()
            .position(|r| (r.dev, r.inode) == id)
            .unwrap_or(0);
        Ok(())
    }

    fn refresh_panels(&mut self, store: &Store) -> Result<()> {
        self.total_size = store
            .conn
            .query_row(
                "SELECT recursive_bytes FROM nodes WHERE dev_id=?1 AND inode=?2",
                params![self.root_dev, self.root_inode],
                |r| r.get(0),
            )
            .unwrap_or(0);
        self.last_scan = store
            .get_meta("last_scan_ts")?
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        // live filesystem capacity (refreshes as disk fills/empties)
        if let Some(fs) = crate::util::fs_stat(std::path::Path::new(&self.root_path)) {
            self.fs = fs;
        }
        self.daemon_live = crate::query::daemon_live(store);

        // status-bar aggregates
        self.items = store
            .conn
            .query_row(
                "SELECT recursive_inodes FROM nodes WHERE dev_id=?1 AND inode=?2",
                params![self.root_dev, self.root_inode],
                |r| r.get(0),
            )
            .unwrap_or(0);
        // growth/day = bytes written in the last hour, extrapolated (×24).
        // Only meaningful with a running daemon + history; 0 otherwise.
        let hour_ago = crate::util::now_secs() - 3600;
        let last_hour: i64 = store
            .conn
            .query_row(
                "SELECT COALESCE(SUM(delta),0) FROM changes WHERE ts>=?1 AND delta>0",
                params![hour_ago],
                |r| r.get(0),
            )
            .unwrap_or(0);
        self.growth_per_day = last_hour * 24;

        let cutoff = crate::util::now_secs() - self.window_secs;
        let mut pr = PathResolver::new(&store.conn);
        // pull extra rows; we drop unresolved/duplicate paths then take 6
        let mut gs = store.conn.prepare(
            "SELECT dev_id, inode, SUM(delta) d FROM changes WHERE ts>=?1
             GROUP BY dev_id, inode HAVING d>0 ORDER BY d DESC LIMIT 60",
        )?;
        let g = gs.query_map(params![cutoff], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        let mut seen_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
        self.top_growth = g
            .filter_map(|x| x.ok())
            .filter_map(|(d, i, delta)| {
                let p = pr.resolve(d, i);
                // skip rows whose node is gone (path can't be resolved) and dups
                if p.starts_with("inode:") || !seen_paths.insert(p.clone()) {
                    None
                } else {
                    Some((p, delta))
                }
            })
            .take(6)
            .collect();

        let mut fs = store.conn.prepare(
            "SELECT dev_id, inode, size FROM nodes WHERE deleted=0 AND kind!='d'
             ORDER BY size DESC LIMIT 6",
        )?;
        let f = fs.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        self.top_files = f
            .filter_map(|x| x.ok())
            .map(|(d, i, s)| (pr.resolve(d, i), s))
            .collect();
        Ok(())
    }
}

fn event_loop<B: Backend>(term: &mut Terminal<B>, app: &mut App, store: &Store) -> Result<()> {
    let mut panel_tick = Instant::now();
    let mut last_input = Instant::now() - Duration::from_secs(10);
    loop {
        term.draw(|f| draw(f, app))?;

        // Block up to 120ms for input. If keys arrive, drain the WHOLE burst and
        // redraw once — navigation never waits on a background refresh.
        if event::poll(Duration::from_millis(120))? {
            loop {
                if let Event::Key(k) = event::read()? {
                    if k.kind == KeyEventKind::Press && handle_key(app, store, k.code)? {
                        return Ok(()); // quit
                    }
                }
                if !event::poll(Duration::from_millis(0))? {
                    break; // burst drained
                }
            }
            last_input = Instant::now();
            continue; // redraw immediately; do NOT rebuild while actively browsing
        }

        // Idle: refresh the live data only when the user has paused (so the
        // periodic rebuild never causes input lag).
        if last_input.elapsed() >= Duration::from_millis(250) {
            if app.last_reload.elapsed() >= Duration::from_millis(1200) {
                app.rebuild(store).ok();
                app.last_reload = Instant::now();
            }
            if panel_tick.elapsed() >= Duration::from_secs(3) {
                app.refresh_panels(store).ok();
                panel_tick = Instant::now();
            }
        }
    }
}

/// Handle one keypress. Returns Ok(true) to quit.
fn handle_key(app: &mut App, store: &Store, code: KeyCode) -> Result<bool> {
    match code {
        KeyCode::Char('q') => return Ok(true),
        KeyCode::Down | KeyCode::Char('j') if app.sel + 1 < app.rows.len() => app.sel += 1,
        KeyCode::Up | KeyCode::Char('k') => app.sel = app.sel.saturating_sub(1),
        KeyCode::Enter | KeyCode::Char(' ') => app.toggle(store)?,
        KeyCode::Right | KeyCode::Char('l') => {
            let r = &app.rows[app.sel];
            if r.kind == 'd' && !r.expanded {
                app.expanded.insert((r.dev, r.inode));
                app.rebuild(store)?;
            }
            if app.sel + 1 < app.rows.len() {
                app.sel += 1;
            }
        }
        KeyCode::Left | KeyCode::Char('h') => {
            if app.rows[app.sel].expanded {
                let id = (app.rows[app.sel].dev, app.rows[app.sel].inode);
                app.expanded.remove(&id);
                app.rebuild(store)?;
            } else {
                app.ascend(store)?;
            }
        }
        KeyCode::Char('r') => {
            app.refresh_panels(store).ok();
            app.rebuild(store).ok();
        }
        KeyCode::Char('i') => {
            app.metric = if app.metric == Metric::Size {
                Metric::Inodes
            } else {
                Metric::Size
            };
            app.rebuild(store)?;
        }
        KeyCode::Home | KeyCode::Char('g') => app.sel = 0,
        KeyCode::End | KeyCode::Char('G') => app.sel = app.rows.len().saturating_sub(1),
        _ => {}
    }
    Ok(false)
}

/// Single UI accent for titles/borders, so the chrome is consistent.
const ACCENT: Color = Color::Cyan;

/// SIZE is shown with ONE calm color everywhere (the bar's LENGTH conveys size,
/// so the color doesn't need to too). This keeps the only varying color in the
/// UI = the write-rate channel.
const SIZE_COLOR: Color = Color::Rgb(110, 160, 210);

/// WRITE-RATE channel color (bytes written in the last hour) — the ONLY color
/// that varies in the UI: blue=static, green=low, yellow=moderate, orange=high,
/// red=extreme.
fn activity_color(rate_per_h: i64) -> Color {
    const MIB: i64 = 1024 * 1024;
    match rate_per_h {
        r if r <= 0 => Color::Rgb(90, 130, 230),  // static (blue)
        r if r < MIB => Color::Rgb(120, 200, 70), // low (green)
        r if r < 50 * MIB => Color::Rgb(235, 200, 0), // moderate (yellow)
        r if r < 500 * MIB => Color::Rgb(255, 140, 0), // high (orange)
        _ => Color::Rgb(255, 45, 45),             // extreme (red)
    }
}

/// Human-readable write rate from bytes-in-the-last-hour.
fn rate_str(per_h: i64) -> String {
    if per_h == 0 {
        return "stable".to_string();
    }
    let arrow = if per_h > 0 { '▲' } else { '▼' };
    let a = per_h.abs();
    const MIB: i64 = 1024 * 1024;
    if a >= MIB {
        format!("{arrow}{}/h", human(a))
    } else {
        // small per-hour: express per day for readability
        format!("{arrow}{}/d", human(a * 24))
    }
}

fn bar(ratio: f64, width: usize) -> String {
    let filled = ((ratio * width as f64).round() as usize).min(width);
    let mut s = String::with_capacity(width);
    for _ in 0..filled {
        s.push('█');
    }
    for _ in filled..width {
        s.push('░');
    }
    s
}

/// Compact count formatting for inode counts: 1234 -> "1.2k", 2.3M -> "2.3M".
fn count_human(n: i64) -> String {
    let n = n.max(0) as f64;
    if n < 1000.0 {
        format!("{n}")
    } else if n < 1_000_000.0 {
        format!("{:.1}k", n / 1000.0)
    } else if n < 1_000_000_000.0 {
        format!("{:.1}M", n / 1_000_000.0)
    } else {
        format!("{:.1}B", n / 1_000_000_000.0)
    }
}

fn short(p: &str, max: usize) -> String {
    if p.chars().count() <= max {
        p.to_string()
    } else {
        let tail: String = p
            .chars()
            .rev()
            .take(max - 1)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("…{tail}")
    }
}

fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Length(1), // capacity gauge
            Constraint::Min(3),    // tree (primary view, on top)
            Constraint::Length(8), // panels (growth | largest), below
            Constraint::Length(1), // footer
        ])
        .split(area);

    // ---- title line ----
    let header = Line::from(vec![
        Span::styled(
            " dux ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            &app.root_path,
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("   index {} old   ", ago(app.last_scan)),
            Style::default().fg(Color::DarkGray),
        ),
        if app.daemon_live {
            Span::styled(
                "● live",
                Style::default()
                    .fg(Color::Rgb(120, 200, 70))
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(
                "○ snapshot (daemon off — growth/ETA need the daemon)",
                Style::default().fg(Color::Rgb(255, 140, 0)),
            )
        },
    ]);
    f.render_widget(Paragraph::new(header), rows[0]);

    // ---- status bar: disk gauge + used/free + growth/day + ETA + items + inodes ----
    let fs = &app.fs;
    let pct = fs.use_pct();
    let bar_w = 18usize;
    let filled = ((pct / 100.0) * bar_w as f64).round() as usize;
    let full_color = if pct >= 95.0 {
        Color::Rgb(255, 45, 45)
    } else if pct >= 85.0 {
        Color::Rgb(255, 140, 0)
    } else if pct >= 70.0 {
        Color::Rgb(235, 200, 0)
    } else {
        Color::Rgb(120, 200, 70)
    };
    let gbar: String = (0..bar_w)
        .map(|i| if i < filled { '█' } else { '░' })
        .collect();

    // growth/day + ETA-to-full (linear, honest "—" when no live history)
    let (growth_str, eta_str) = if app.daemon_live && app.growth_per_day > 0 {
        let days = fs.avail as f64 / app.growth_per_day as f64;
        let eta = if days >= 365.0 {
            format!("{:.0}y", days / 365.0)
        } else if days >= 1.0 {
            format!("{:.0}d", days)
        } else {
            format!("{:.0}h", days * 24.0)
        };
        (format!("▲{}/day", human(app.growth_per_day)), eta)
    } else {
        ("stable".to_string(), "—".to_string())
    };

    let sep = || Span::styled("   ", Style::default());
    let label = |s: &str| Span::styled(format!("{s} "), Style::default().fg(Color::DarkGray));
    let status = Line::from(vec![
        Span::styled(" DISK ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{:>3.0}% ", pct),
            Style::default().fg(full_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(gbar, Style::default().fg(full_color)),
        sep(),
        label("Used"),
        Span::styled(
            human(fs.used),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" / {}", human(fs.total)),
            Style::default().fg(Color::Gray),
        ),
        sep(),
        label("Free"),
        Span::styled(human(fs.avail), Style::default()),
        sep(),
        label("Growth"),
        Span::styled(
            growth_str,
            Style::default().fg(activity_color(app.growth_per_day / 24)),
        ),
        sep(),
        label("ETA full"),
        Span::styled(eta_str, Style::default().add_modifier(Modifier::BOLD)),
        sep(),
        label("Items"),
        Span::styled(count_human(app.items), Style::default()),
        sep(),
        label("Inodes"),
        Span::styled(format!("{:.0}%", fs.inode_pct()), Style::default()),
    ]);
    f.render_widget(Paragraph::new(status), rows[1]);

    // top panels
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[3]);
    let growth_items: Vec<Line> = if app.top_growth.is_empty() {
        vec![Line::from(Span::styled(
            "  (no growth yet — run the daemon)",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        app.top_growth
            .iter()
            .map(|(p, d)| {
                // write-rate palette — same channel/colors as the tree dots
                Line::from(vec![
                    Span::styled("● ", Style::default().fg(activity_color(*d))),
                    Span::styled(
                        format!("{:<10}", rate_str(*d)),
                        Style::default()
                            .fg(activity_color(*d))
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(short(p, 40)),
                ])
            })
            .collect()
    };
    f.render_widget(
        Paragraph::new(growth_items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(Span::styled(
                    " 🔥 Fastest Growth (1h) ",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                )),
        ),
        top[0],
    );
    let file_items: Vec<Line> = app
        .top_files
        .iter()
        .map(|(p, s)| {
            // size = the one calm SIZE_COLOR, same as the tree
            Line::from(vec![
                Span::styled(
                    format!(" {:<11}", human(*s)),
                    Style::default().fg(SIZE_COLOR).add_modifier(Modifier::BOLD),
                ),
                Span::raw(short(p, 42)),
            ])
        })
        .collect();
    f.render_widget(
        Paragraph::new(file_items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(Span::styled(
                    " 📦 Largest Files ",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                )),
        ),
        top[1],
    );

    // tree body
    let body = rows[2];
    let inner_h = body.height.saturating_sub(2) as usize; // borders
                                                          // keep selection visible
    if app.sel < app.scroll {
        app.scroll = app.sel;
    } else if app.sel >= app.scroll + inner_h {
        app.scroll = app.sel + 1 - inner_h;
    }
    let end = (app.scroll + inner_h).min(app.rows.len());

    let inode_mode = app.metric == Metric::Inodes;
    let mut lines: Vec<Line> = Vec::new();
    for i in app.scroll..end {
        let r = &app.rows[i];
        let selected = i == app.sel;
        let indent = "  ".repeat(r.depth);
        let marker = if r.kind == 'd' {
            if r.expanded {
                "▼ "
            } else if r.has_children {
                "▶ "
            } else {
                "  "
            }
        } else {
            "  "
        };
        let ratio = if inode_mode {
            r.ratio_inodes
        } else {
            r.ratio_size
        };
        let value = if inode_mode {
            format!("{:>10}", count_human(r.inodes))
        } else {
            format!("{:>10}", human(r.size))
        };
        // SIZE channel: one calm color (the bar LENGTH conveys magnitude)
        let size_col = SIZE_COLOR;
        let name = if r.kind == 'd' {
            format!("{}/", r.name)
        } else {
            r.name.clone()
        };
        // WRITE-RATE channel: r.growth = bytes in the last hour. Separate palette
        // (blue=static → red=extreme), shown as a dot + rate, never sharing the
        // size color.
        let act = activity_color(r.growth);
        let rate = rate_str(r.growth);

        let line = Line::from(vec![
            Span::styled(
                format!("{value} "),
                Style::default().fg(size_col).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{} ", bar(ratio, 12)),
                Style::default().fg(size_col),
            ),
            // activity indicator + rate (own color channel)
            Span::styled("● ", Style::default().fg(act)),
            Span::styled(format!("{rate:<9} "), Style::default().fg(act)),
            Span::raw(indent),
            Span::styled(marker, Style::default().fg(Color::DarkGray)),
            Span::styled(
                name,
                if r.kind == 'd' {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                },
            ),
        ]);
        let mut line = line;
        if selected {
            line = line.style(Style::default().bg(Color::Rgb(38, 44, 66)));
        }
        lines.push(line);
    }
    let title = if inode_mode {
        " 🌳 Tree — bar=INODE count · ●=write rate (blue→red) "
    } else {
        " 🌳 Tree — bar=SIZE · ●=write rate (blue→red) "
    };
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(Span::styled(
                    title,
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                )),
        ),
        body,
    );

    // footer
    let mode = if inode_mode { "inodes" } else { "size" };
    let footer = Line::from(format!(
        " ↑↓ move   → / ⏎ expand   ← collapse/up   i size⇄inodes   r refresh   q quit    [{mode}] "
    ))
    .style(Style::default().fg(Color::Black).bg(Color::DarkGray));
    f.render_widget(Paragraph::new(footer), rows[4]);
}

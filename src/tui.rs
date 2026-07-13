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

/// Max children loaded per directory in the tree view. A directory with more
/// shows a visible "… more than N entries" marker instead of silently hiding.
const CHILD_LIMIT: usize = 5000;

/// What the tree graph visualizes.
#[derive(Clone, Copy, PartialEq)]
enum Metric {
    Size,
    Inodes,
}

/// Which section has keyboard focus (Tab cycles).
#[derive(Clone, Copy, PartialEq)]
enum Focus {
    Tree,
    Groups,
    Growth,
    Files,
}

#[derive(Clone, Copy, PartialEq)]
enum Screen {
    Main,
    Apps,
}

#[derive(Clone)]
struct GroupTarget {
    path: String,
}

#[derive(Clone, Copy, PartialEq)]
enum GroupView {
    Top,
    Detail(&'static str),
}

#[derive(Clone)]
struct GroupRow {
    name: &'static str,
    size: i64,
    growth: i64,
    targets: Vec<GroupTarget>,
    drill: Option<&'static str>,
}

/// One visible row in the expandable tree.
struct Row {
    dev: i64,
    inode: i64,
    name: String,
    path: String, // full, terminal-escaped path (cached so detail needs no query)
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
    top_files: Vec<(String, i64, i64, i64)>, // path, blocks, mtime, recent growth/h
    groups: Vec<GroupRow>,
    total_size: i64,
    last_scan: i64,
    window_secs: i64,
    scroll: usize,
    metric: Metric,
    root_path: String,
    db: std::path::PathBuf,
    docker_paths: Vec<String>,
    group_view: GroupView,
    fs: crate::util::FsStat,
    daemon_live: bool,
    dirty_since: Option<i64>, // Some(epoch) if the index missed events (overflow)
    paused_since: Option<i64>, // Some(epoch) if writes are paused (host pressure)
    pause_reason: String,     // why writes are paused (low disk / low memory / …)
    throttled_since: Option<i64>, // Some(epoch) if governing keeps the index stale
    // recursive write-rate per node (bytes in the last hour), summed up the tree
    growth_map: std::collections::HashMap<(i64, i64), i64>,
    growth_calc: Instant,
    items: i64,          // total indexed nodes (files + dirs)
    growth_per_day: i64, // extrapolated from the last hour of change log
    screen: Screen,      // full-screen mode vs normal TUI
    focus: Focus,        // which section the keyboard drives
    asel: usize,         // selected row in the App/OS groups panel
    gsel: usize,         // selected row in the Fastest-Growth panel
    fsel: usize,         // selected row in the Largest-Files panel
    detail: String,      // full path of the current selection (shown in footer)
    // Structure generation: bumped whenever `expanded`/`metric` change. A
    // background refresh result is only applied to the TREE if its generation
    // still matches (the user hasn't restructured since) — see the worker.
    view_gen: u64,
}

/// Inputs the background worker needs to recompute the view off the input thread.
struct Snapshot {
    expanded: std::collections::HashSet<(i64, i64)>,
    metric: Metric,
    window_secs: i64,
    gen: u64,
}

/// View data the worker produces (all owned/`Send`), applied by the UI thread.
struct RefreshResult {
    gen: u64,
    rows: Vec<Row>,
    growth_map: std::collections::HashMap<(i64, i64), i64>,
    top_growth: Vec<(String, i64)>,
    top_files: Vec<(String, i64, i64, i64)>,
    groups: Vec<GroupRow>,
    total_size: i64,
    items: i64,
    growth_per_day: i64,
    last_scan: i64,
    fs: crate::util::FsStat,
    daemon_live: bool,
    dirty_since: Option<i64>,
    paused_since: Option<i64>,
    pause_reason: String,
    throttled_since: Option<i64>,
}

/// Background refresh worker: owns its OWN read-only connection and a shadow App
/// (never rendered — just a compute context that reuses rebuild/refresh_panels),
/// so the expensive recursive growth CTEs never run on the input thread. Reopens
/// the connection if the DB is replaced under it (an atomic rescan renames a new
/// file over the old). Exits when the snapshot sender is dropped (UI quit).
fn refresh_worker(
    db: std::path::PathBuf,
    dev: i64,
    inode: i64,
    rx: crossbeam_channel::Receiver<Snapshot>,
    tx: crossbeam_channel::Sender<RefreshResult>,
) {
    let mut store = match Store::open_ro(&db) {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut open_ino = db_inode(&db);
    let mut shadow = App::new(&store, &db, dev, inode);
    while let Ok(mut snap) = rx.recv() {
        // coalesce: skip to the most recent queued request
        while let Ok(s) = rx.try_recv() {
            snap = s;
        }
        // Reopen if a rescan swapped the db inode: a connection on the old unlinked
        // inode serves stale panels with no error, so the reopen-on-error path below
        // would never trigger for a clean swap.
        let cur_ino = db_inode(&db);
        if cur_ino != 0 && cur_ino != open_ino {
            if let Ok(s) = Store::open_ro(&db) {
                store = s;
                open_ino = cur_ino;
            }
        }
        shadow.expanded = snap.expanded;
        shadow.metric = snap.metric;
        shadow.window_secs = snap.window_secs;
        shadow.growth_calc = Instant::now() - Duration::from_secs(60); // force recompute
        let ok = {
            shadow.refresh_growth_map(&store);
            shadow.rebuild(&store).is_ok() && shadow.refresh_panels(&store).is_ok()
        };
        if !ok {
            // likely a stale connection (db replaced by a rescan) — reopen once.
            if let Ok(s) = Store::open_ro(&db) {
                store = s;
                shadow.growth_calc = Instant::now() - Duration::from_secs(60);
                shadow.refresh_growth_map(&store);
                let _ = shadow.rebuild(&store);
                let _ = shadow.refresh_panels(&store);
            }
        }
        let result = RefreshResult {
            gen: snap.gen,
            rows: std::mem::take(&mut shadow.rows),
            growth_map: shadow.growth_map.clone(),
            top_growth: std::mem::take(&mut shadow.top_growth),
            top_files: std::mem::take(&mut shadow.top_files),
            groups: std::mem::take(&mut shadow.groups),
            total_size: shadow.total_size,
            items: shadow.items,
            growth_per_day: shadow.growth_per_day,
            last_scan: shadow.last_scan,
            fs: shadow.fs,
            daemon_live: shadow.daemon_live,
            throttled_since: shadow.throttled_since,
            dirty_since: shadow.dirty_since,
            paused_since: shadow.paused_since,
            pause_reason: std::mem::take(&mut shadow.pause_reason),
        };
        if tx.send(result).is_err() {
            break; // UI gone
        }
    }
}

/// WinDirStat-style live tree: folders expand inline beneath their parent (the
/// parent stays visible), indented, with a per-row heat bar (RED = hot). Opens
/// at `start` (dev,inode) — the scoped path or the index root.
pub fn run(db: &std::path::Path, start: Option<(i64, i64)>) -> Result<()> {
    // Own the store lifecycle here (don't hold a caller's connection across the
    // whole session): the setup store is used only for the first frame and dropped
    // before the event loop, which opens its OWN reopenable connection. Otherwise a
    // long-open TUI would pin the old (deleted) index inode after a rescan — the
    // very deleted-but-open leak dux is built to find.
    let (dev, inode, mut app) = {
        let store = Store::open_ro(db)?;
        let root = start.or_else(|| root_node(&store));
        let (dev, inode) = match root {
            Some(v) => v,
            None => {
                println!("empty index — run `dux scan <PATH>` first");
                return Ok(());
            }
        };
        let mut app = App::new(&store, db, dev, inode);
        // First frame = the TREE only (cheap, indexed per-dir queries). The
        // expensive growth-heat + panels are computed by the background worker and
        // applied when ready — so the TUI opens INSTANTLY even on a huge, high-churn
        // index instead of blocking ~30s on the recursive growth query.
        app.init_root(&store)?;
        app.update_detail(&store);
        (dev, inode, app)
    };

    // Spawn the background refresh worker — it recomputes the heavy view data off
    // the input thread so navigation never blocks (M4). Channels: snapshots out,
    // results in. When `snap_tx` drops (run returns), the worker exits.
    let (snap_tx, snap_rx) = crossbeam_channel::unbounded::<Snapshot>();
    let (res_tx, res_rx) = crossbeam_channel::unbounded::<RefreshResult>();
    {
        let dbp = db.to_path_buf();
        std::thread::spawn(move || refresh_worker(dbp, dev, inode, snap_rx, res_tx));
    }

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

    // Arm the restore guard BEFORE touching the terminal: if EnterAlternateScreen
    // fails after raw mode is enabled, the guard's Drop still disables raw mode
    // (disable when not raw is a harmless no-op) so the shell isn't left wedged.
    let _guard = TermGuard;
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    let mut term = Terminal::new(CrosstermBackend::new(stdout()))?;
    let res = event_loop(&mut term, &mut app, db, &snap_tx, &res_rx);
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
            "SELECT dev_id, inode FROM inodes WHERE kind='d'
             ORDER BY recursive_bytes DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()
}

impl App {
    /// Construct an App rooted at (dev,inode). Used by the UI and by the
    /// background refresh worker's shadow context (which reuses rebuild/
    /// refresh_panels to compute off the input thread — see RefreshWorker).
    fn new(store: &Store, db: &std::path::Path, dev: i64, inode: i64) -> App {
        App {
            rows: Vec::new(),
            expanded: std::collections::HashSet::new(),
            sel: 0,
            root_dev: dev,
            root_inode: inode,
            top_growth: Vec::new(),
            top_files: Vec::new(),
            groups: Vec::new(),
            total_size: 0,
            last_scan: 0,
            window_secs: 3600,
            scroll: 0,
            metric: Metric::Size,
            root_path: store.path_of(dev, inode).unwrap_or_else(|_| "/".into()),
            db: db.to_path_buf(),
            docker_paths: docker_paths(),
            group_view: GroupView::Top,
            fs: crate::util::FsStat::default(),
            daemon_live: false,
            dirty_since: None,
            paused_since: None,
            pause_reason: String::new(),
            throttled_since: None,
            growth_map: std::collections::HashMap::new(),
            growth_calc: Instant::now() - Duration::from_secs(60),
            items: 0,
            growth_per_day: 0,
            screen: Screen::Main,
            focus: Focus::Tree,
            asel: 0,
            gsel: 0,
            fsel: 0,
            detail: String::new(),
            view_gen: 0,
        }
    }

    /// Apply a worker refresh. Panels/totals/states/growth-map are always taken
    /// (they don't depend on the tree structure); the row list is taken only when
    /// the user hasn't restructured since the request (`gen` still current) — else
    /// a synchronous rebuild already produced the up-to-date rows.
    fn apply_refresh(&mut self, r: RefreshResult) {
        self.growth_map = r.growth_map;
        self.top_growth = r.top_growth;
        self.top_files = r.top_files;
        // The worker's shadow always computes TOP-level groups (Snapshot carries
        // no group_view). Only adopt them while we're actually showing the top
        // view — otherwise a refresh would clobber the drilled-in Detail groups
        // (computed synchronously on drill) back to top-level every ~4s.
        if self.group_view == GroupView::Top {
            self.groups = r.groups;
        }
        self.total_size = r.total_size;
        self.items = r.items;
        self.growth_per_day = r.growth_per_day;
        self.last_scan = r.last_scan;
        self.fs = r.fs;
        self.daemon_live = r.daemon_live;
        self.throttled_since = r.throttled_since;
        self.dirty_since = r.dirty_since;
        self.paused_since = r.paused_since;
        self.pause_reason = r.pause_reason;
        if r.gen == self.view_gen {
            let sel_id = self.rows.get(self.sel).map(|x| (x.dev, x.inode));
            self.rows = r.rows;
            if let Some(id) = sel_id {
                if let Some(pos) = self.rows.iter().position(|x| (x.dev, x.inode) == id) {
                    self.sel = pos;
                }
            }
            if self.sel >= self.rows.len() {
                self.sel = self.rows.len().saturating_sub(1);
            }
        }
        self.asel = self.asel.min(self.groups.len().saturating_sub(1));
        self.gsel = self.gsel.min(self.top_growth.len().saturating_sub(1));
        self.fsel = self.fsel.min(self.top_files.len().saturating_sub(1));
    }

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
        let cutoff = (crate::util::now_secs() - 3600) / crate::store::GROWTH_BUCKET_SECS;
        let mut map = std::collections::HashMap::new();
        // Bound the recursive ancestor walk to the HOTTEST changed inodes. On a
        // high-churn host (e.g. a busy mail store) the last hour can have hundreds
        // of thousands of changed inodes; walking every one to the root took tens
        // of seconds. The heat bar is dominated by the top contributors, so cap
        // the leaves — the long tail is visually negligible.
        if let Ok(mut stmt) = store.conn.prepare(
            "WITH RECURSIVE
               chg(dev,ino,d) AS (
                 SELECT dev_id,inode,SUM(delta) s FROM growth WHERE bucket>=?1
                 GROUP BY dev_id,inode ORDER BY s DESC LIMIT 3000
               ),
               anc(dev,ino,d,depth) AS (
                 SELECT dev,ino,d,0 FROM chg
                 UNION ALL
                 SELECT n.parent_dev,n.parent_inode,a.d,a.depth+1 FROM anc a
                 JOIN dirents n ON n.dev_id=a.dev AND n.inode=a.ino AND n.prime=1
                 WHERE NOT (n.dev_id=n.parent_dev AND n.inode=n.parent_inode) AND a.depth<4096
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
        // NOTE: the expensive recursive growth CTE (refresh_growth_map) is NOT run
        // here — it's owned by the background worker so a key-driven rebuild stays
        // cheap (just per-directory indexed queries). Rows use the cached
        // `growth_map`; the worker refreshes it.
        let sel_id = self.rows.get(self.sel).map(|r| (r.dev, r.inode));
        let mut out: Vec<Row> = Vec::new();

        // root row (name is the scan root path; totals live on the inode)
        let (size, inodes): (i64, i64) = store
            .conn
            .query_row(
                "SELECT recursive_bytes, recursive_inodes FROM inodes WHERE dev_id=?1 AND inode=?2",
                params![self.root_dev, self.root_inode],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or((0, 0));
        let name = crate::util::display_path(&self.root_path);
        let root_expanded = self.expanded.contains(&(self.root_dev, self.root_inode));
        out.push(Row {
            dev: self.root_dev,
            inode: self.root_inode,
            name: name.clone(),
            path: name,
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
            let root_path = self.root_path.clone();
            self.append_children(
                store,
                &mut out,
                self.root_dev,
                self.root_inode,
                1,
                &root_path,
            )?;
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

    fn refresh_groups(&mut self, store: &Store) {
        self.groups = compute_groups(
            store,
            &self.root_path,
            self.root_dev,
            self.root_inode,
            self.total_size,
            &self.docker_paths,
            self.group_view,
            &self.growth_map,
        );
        self.asel = self.asel.min(self.groups.len().saturating_sub(1));
    }

    /// Recursively append a directory's children (and any expanded descendants)
    /// to `out`, computing per-sibling-group ratios and growth.
    #[allow(clippy::too_many_arguments)]
    fn append_children(
        &self,
        store: &Store,
        out: &mut Vec<Row>,
        dev: i64,
        inode: i64,
        depth: usize,
        parent_path: &str, // real (unescaped) path of this parent, for joining
    ) -> Result<()> {
        let order = if self.metric == Metric::Inodes {
            "recursive_inodes"
        } else {
            "CASE WHEN kind='d' THEN recursive_bytes ELSE blocks END"
        };
        // children are dirents under this parent; metadata joins from inodes.
        // LIMIT is CHILD_LIMIT+1 so we can tell whether the list was truncated.
        let sql = format!(
            "SELECT d.dev_id, d.inode, d.name, i.kind, i.blocks, i.recursive_bytes, i.recursive_inodes
             FROM dirents d JOIN inodes i ON i.dev_id=d.dev_id AND i.inode=d.inode
             WHERE d.parent_dev=?1 AND d.parent_inode=?2
               AND NOT (d.dev_id=?1 AND d.inode=?2)
             ORDER BY {order} DESC LIMIT {}",
            CHILD_LIMIT + 1
        );
        // prepare_cached: only 2 distinct SQL strings (size vs inode order), so
        // expanding/refreshing many dirs every cycle reuses the compiled plan.
        let mut stmt = store.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![dev, inode], |r| {
            let kind: String = r.get(3)?;
            let k = kind.chars().next().unwrap_or('?');
            let size = if k == 'd' {
                r.get::<_, i64>(5)?
            } else {
                r.get::<_, i64>(4)?
            };
            let inodes = if k == 'd' { r.get::<_, i64>(6)? } else { 1 };
            let nb: Vec<u8> = r.get(2)?;
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                crate::util::display_name(&nb), // escaped, for the tree
                String::from_utf8_lossy(&nb).into_owned(), // lossy, for path joins
                k,
                size,
                inodes,
            ))
        })?;
        let mut kids: Vec<(i64, i64, String, String, char, i64, i64)> = Vec::new();
        for row in rows {
            kids.push(row?);
        }
        if kids.is_empty() {
            return Ok(());
        }
        // truncated? drop the probe row and remember to show a marker.
        let truncated = kids.len() > CHILD_LIMIT;
        if truncated {
            kids.truncate(CHILD_LIMIT);
        }
        let maxs = kids.iter().map(|k| k.5).max().unwrap_or(1).max(1);
        let maxi = kids.iter().map(|k| k.6).max().unwrap_or(1).max(1);

        for (cdev, cino, name, raw_name, kind, size, inodes) in kids {
            let is_expanded = kind == 'd' && self.expanded.contains(&(cdev, cino));
            // a directory is only collapsible/expandable if it actually has
            // descendants (recursive_inodes counts itself, so >1 means non-empty).
            let has_children = kind == 'd' && inodes > 1;
            // join this entry onto its parent's real path; escape only for display.
            let real_path = if parent_path.ends_with('/') {
                format!("{parent_path}{raw_name}")
            } else {
                format!("{parent_path}/{raw_name}")
            };
            out.push(Row {
                dev: cdev,
                inode: cino,
                name,
                path: crate::util::display_path(&real_path),
                kind,
                size,
                inodes,
                depth,
                expanded: is_expanded,
                has_children,
                ratio_size: size as f64 / maxs as f64,
                ratio_inodes: inodes as f64 / maxi as f64,
                // recursive write-rate (subtree), from the cached growth map
                growth: self.growth_map.get(&(cdev, cino)).copied().unwrap_or(0),
            });
            if is_expanded {
                self.append_children(store, out, cdev, cino, depth + 1, &real_path)?;
            }
        }
        if truncated {
            // visible marker so a >CHILD_LIMIT directory never silently hides rows
            out.push(Row {
                dev: 0,
                inode: 0,
                name: format!("… more than {CHILD_LIMIT} entries — narrow with `dux find`"),
                path: String::new(),
                kind: 'o',
                size: 0,
                inodes: 0,
                depth,
                expanded: false,
                has_children: false,
                ratio_size: 0.0,
                ratio_inodes: 0.0,
                growth: 0,
            });
        }
        Ok(())
    }

    fn toggle(&mut self, store: &Store) -> Result<()> {
        let r = match self.rows.get(self.sel) {
            Some(r) => r,
            None => return Ok(()), // defensive: never index an empty/short row list
        };
        if r.kind != 'd' {
            return Ok(());
        }
        let id = (r.dev, r.inode);
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
        self.view_gen += 1; // structure changed: stale worker rows must not apply
        self.rebuild(store)
    }

    /// Move to the parent of the selection and collapse it.
    fn ascend(&mut self, store: &Store) -> Result<()> {
        let depth = match self.rows.get(self.sel) {
            Some(r) => r.depth,
            None => return Ok(()),
        };
        if depth == 0 {
            return Ok(());
        }
        let mut i = self.sel;
        while i > 0 && self.rows[i].depth >= depth {
            i -= 1;
        }
        let id = (self.rows[i].dev, self.rows[i].inode);
        self.expanded.remove(&id);
        self.view_gen += 1;
        self.rebuild(store)?;
        self.sel = self
            .rows
            .iter()
            .position(|r| (r.dev, r.inode) == id)
            .unwrap_or(0);
        Ok(())
    }

    /// When the TUI is opened at a subtree (not the whole index root), the
    /// largest-files / fastest-growth panels must be scoped to that subtree too,
    /// or they'd show unrelated global entries. Returns the ` AND (...) IN (...)`
    /// clause + bind params, or empty strings when viewing the whole index.
    fn panel_scope(&self, store: &Store) -> (String, Vec<i64>) {
        let rdev: Option<i64> = store
            .get_meta("root_dev")
            .ok()
            .flatten()
            .and_then(|s| s.parse().ok());
        let rino: Option<i64> = store
            .get_meta("root_inode")
            .ok()
            .flatten()
            .and_then(|s| s.parse().ok());
        if rdev == Some(self.root_dev) && rino == Some(self.root_inode) {
            (String::new(), Vec::new()) // viewing the whole index — no filter
        } else {
            (
                format!(" AND (dev_id,inode) IN ({})", crate::query::SUBTREE_CTE),
                vec![self.root_dev, self.root_inode],
            )
        }
    }

    fn refresh_panels(&mut self, store: &Store) -> Result<()> {
        self.total_size = store
            .conn
            .query_row(
                "SELECT recursive_bytes FROM inodes WHERE dev_id=?1 AND inode=?2",
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
        self.daemon_live = crate::query::daemon_live(&self.db);
        self.dirty_since = crate::query::dirty_since(store);
        self.paused_since = crate::query::paused_since(store);
        self.throttled_since = crate::query::throttled_since(store);
        self.pause_reason = store
            .get_meta("pause_reason")
            .ok()
            .flatten()
            .unwrap_or_else(|| "system pressure".into());

        // status-bar aggregates
        self.items = store
            .conn
            .query_row(
                "SELECT recursive_inodes FROM inodes WHERE dev_id=?1 AND inode=?2",
                params![self.root_dev, self.root_inode],
                |r| r.get(0),
            )
            .unwrap_or(0);
        // growth/day = bytes written in the last hour, extrapolated (×24).
        // Only meaningful with a running daemon + history; 0 otherwise.
        let hour_ago = (crate::util::now_secs() - 3600) / crate::store::GROWTH_BUCKET_SECS;
        let last_hour: i64 = store
            .conn
            .query_row(
                "SELECT COALESCE(SUM(delta),0) FROM growth WHERE bucket>=?1 AND delta>0",
                params![hour_ago],
                |r| r.get(0),
            )
            .unwrap_or(0);
        self.growth_per_day = last_hour * 24;

        let cutoff =
            (crate::util::now_secs() - self.window_secs) / crate::store::GROWTH_BUCKET_SECS;
        let (scope_sql, scope_args) = self.panel_scope(store);
        let mut pr = PathResolver::new(&store.conn);
        // pull extra rows; we drop unresolved/duplicate paths then take 6
        let gsql = format!(
            "SELECT dev_id, inode, SUM(delta) d FROM growth WHERE bucket>=?1{scope_sql}
             GROUP BY dev_id, inode HAVING d>0 ORDER BY d DESC LIMIT 60"
        );
        let mut gs = store.conn.prepare(&gsql)?;
        let mut gbind: Vec<i64> = vec![cutoff];
        gbind.extend_from_slice(&scope_args);
        let g = gs.query_map(rusqlite::params_from_iter(gbind.iter()), |r| {
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
                    Some((crate::util::display_path(&p), delta))
                }
            })
            .take(6)
            .collect();

        let fsql = format!(
            "SELECT dev_id, inode, blocks, mtime FROM inodes WHERE kind!='d'{scope_sql}
             ORDER BY blocks DESC LIMIT 6"
        );
        let mut fs = store.conn.prepare(&fsql)?;
        let f = fs.query_map(rusqlite::params_from_iter(scope_args.iter()), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        self.top_files = f
            .filter_map(|x| x.ok())
            .map(|(d, i, blocks, mtime)| {
                // recent write rate for this file (leaf growth from the map)
                let growth = self.growth_map.get(&(d, i)).copied().unwrap_or(0);
                (
                    crate::util::display_path(&pr.resolve(d, i)),
                    blocks,
                    mtime,
                    growth,
                )
            })
            .collect();
        self.refresh_groups(store);
        // keep panel selections in range as panels change
        self.asel = self.asel.min(self.groups.len().saturating_sub(1));
        self.gsel = self.gsel.min(self.top_growth.len().saturating_sub(1));
        self.fsel = self.fsel.min(self.top_files.len().saturating_sub(1));
        Ok(())
    }

    /// Full path of the current selection (focused section) — shown in the footer
    /// so long/truncated names are always fully visible.
    fn update_detail(&mut self, _store: &Store) {
        self.detail = match self.focus {
            // path is cached on each Row during rebuild — no per-keypress query.
            Focus::Tree => self
                .rows
                .get(self.sel)
                .map(|r| r.path.clone())
                .unwrap_or_default(),
            Focus::Groups => self
                .groups
                .get(self.asel)
                .map(group_detail)
                .unwrap_or_default(),
            Focus::Growth => self
                .top_growth
                .get(self.gsel)
                .map(|x| x.0.clone())
                .unwrap_or_default(),
            Focus::Files => self
                .top_files
                .get(self.fsel)
                .map(|x| x.0.clone())
                .unwrap_or_default(),
        };
    }
}

struct GroupDef {
    name: &'static str,
    paths: &'static [&'static str],
}

struct AppProfile {
    id: &'static str,
    name: &'static str,
    segments: &'static [GroupDef],
}

const DOCKER_PATHS: &[&str] = &[
    "/var/lib/docker",
    "/var/lib/containerd",
    "/var/lib/kubelet",
    "/var/lib/containers",
    "/var/lib/crio",
    "/var/snap/docker/common/var-lib-docker",
];
const LOG_PATHS: &[&str] = &["/var/log", "/run/log"];
const APP_PATHS: &[&str] = &[
    "/var/lib",
    "/srv",
    "/opt",
    "/usr/local",
    "/var/www",
    "/var/lib/flatpak",
    "/var/snap",
    "/snap",
];
const CACHE_PATHS: &[&str] = &["/var/cache", "/tmp", "/var/tmp"];
const USER_PATHS: &[&str] = &["/home", "/root"];

const OS_SEGMENTS: &[GroupDef] = &[
    GroupDef {
        name: "kernel/boot",
        paths: &["/boot", "/usr/lib/modules"],
    },
    GroupDef {
        name: "libraries",
        paths: &["/usr/lib", "/usr/lib64", "/lib", "/lib64"],
    },
    GroupDef {
        name: "binaries",
        paths: &["/usr/bin", "/usr/sbin", "/bin", "/sbin"],
    },
    GroupDef {
        name: "config",
        paths: &["/etc"],
    },
    GroupDef {
        name: "package db/cache",
        paths: &["/var/lib/dpkg", "/var/lib/apt", "/var/cache/apt"],
    },
    GroupDef {
        name: "system logs",
        paths: &[
            "/var/log/journal",
            "/run/log/journal",
            "/var/log/syslog",
            "/var/log/kern.log",
        ],
    },
];

const DOCKER_SEGMENTS: &[GroupDef] = &[
    GroupDef {
        name: "images/layers",
        paths: &[
            "/var/lib/docker/overlay2",
            "/var/lib/docker/image",
            "/var/lib/containers/storage/overlay",
        ],
    },
    GroupDef {
        name: "volumes",
        paths: &[
            "/var/lib/docker/volumes",
            "/var/lib/containers/storage/volumes",
        ],
    },
    GroupDef {
        name: "container logs",
        paths: &[
            "/var/lib/docker/containers",
            "/var/log/containers",
            "/var/log/pods",
        ],
    },
    GroupDef {
        name: "build cache",
        paths: &["/var/lib/docker/buildkit", "/var/lib/docker/buildx"],
    },
    GroupDef {
        name: "containerd/kubelet",
        paths: &["/var/lib/containerd", "/var/lib/kubelet", "/var/lib/crio"],
    },
    GroupDef {
        name: "other docker",
        paths: DOCKER_PATHS,
    },
];

const NGINX_SEGMENTS: &[GroupDef] = &[
    GroupDef {
        name: "logs",
        paths: &["/var/log/nginx"],
    },
    GroupDef {
        name: "web roots",
        paths: &["/var/www", "/srv/www", "/srv/http"],
    },
    GroupDef {
        name: "config",
        paths: &["/etc/nginx"],
    },
    GroupDef {
        name: "cache",
        paths: &["/var/cache/nginx"],
    },
    GroupDef {
        name: "runtime/data",
        paths: &["/var/lib/nginx", "/run/nginx"],
    },
    GroupDef {
        name: "binary",
        paths: &["/usr/sbin/nginx"],
    },
];

const APACHE_SEGMENTS: &[GroupDef] = &[
    GroupDef {
        name: "logs",
        paths: &["/var/log/apache2", "/var/log/httpd"],
    },
    GroupDef {
        name: "web roots",
        paths: &["/var/www", "/srv/www", "/srv/http"],
    },
    GroupDef {
        name: "config",
        paths: &["/etc/apache2", "/etc/httpd"],
    },
    GroupDef {
        name: "cache",
        paths: &["/var/cache/apache2", "/var/cache/httpd"],
    },
];

const POSTGRES_SEGMENTS: &[GroupDef] = &[
    GroupDef {
        name: "data",
        paths: &["/var/lib/postgresql"],
    },
    GroupDef {
        name: "logs",
        paths: &["/var/log/postgresql"],
    },
    GroupDef {
        name: "config",
        paths: &["/etc/postgresql"],
    },
    GroupDef {
        name: "runtime",
        paths: &["/run/postgresql"],
    },
];

const MYSQL_SEGMENTS: &[GroupDef] = &[
    GroupDef {
        name: "data",
        paths: &["/var/lib/mysql"],
    },
    GroupDef {
        name: "logs",
        paths: &["/var/log/mysql", "/var/log/mariadb"],
    },
    GroupDef {
        name: "config",
        paths: &["/etc/mysql", "/etc/my.cnf", "/etc/mysql.conf.d"],
    },
    GroupDef {
        name: "runtime",
        paths: &["/run/mysqld", "/run/mariadb"],
    },
];

const REDIS_SEGMENTS: &[GroupDef] = &[
    GroupDef {
        name: "data",
        paths: &["/var/lib/redis"],
    },
    GroupDef {
        name: "logs",
        paths: &["/var/log/redis"],
    },
    GroupDef {
        name: "config",
        paths: &["/etc/redis"],
    },
    GroupDef {
        name: "runtime",
        paths: &["/run/redis"],
    },
];

const ELASTIC_SEGMENTS: &[GroupDef] = &[
    GroupDef {
        name: "data",
        paths: &["/var/lib/elasticsearch"],
    },
    GroupDef {
        name: "logs",
        paths: &["/var/log/elasticsearch"],
    },
    GroupDef {
        name: "config",
        paths: &["/etc/elasticsearch"],
    },
];

const MONGO_SEGMENTS: &[GroupDef] = &[
    GroupDef {
        name: "data",
        paths: &["/var/lib/mongodb", "/var/lib/mongo"],
    },
    GroupDef {
        name: "logs",
        paths: &["/var/log/mongodb", "/var/log/mongo"],
    },
    GroupDef {
        name: "config",
        paths: &["/etc/mongod.conf", "/etc/mongodb.conf"],
    },
];

const JOURNAL_SEGMENTS: &[GroupDef] = &[
    GroupDef {
        name: "persistent",
        paths: &["/var/log/journal"],
    },
    GroupDef {
        name: "runtime",
        paths: &["/run/log/journal"],
    },
    GroupDef {
        name: "syslog/kernel",
        paths: &["/var/log/syslog", "/var/log/kern.log", "/var/log/messages"],
    },
];

const TOP_PROFILES: &[AppProfile] = &[
    AppProfile {
        id: "os",
        name: "OS",
        segments: OS_SEGMENTS,
    },
    AppProfile {
        id: "docker",
        name: "Docker/Containers",
        segments: DOCKER_SEGMENTS,
    },
    AppProfile {
        id: "nginx",
        name: "nginx",
        segments: NGINX_SEGMENTS,
    },
    AppProfile {
        id: "apache",
        name: "apache",
        segments: APACHE_SEGMENTS,
    },
    AppProfile {
        id: "postgres",
        name: "PostgreSQL",
        segments: POSTGRES_SEGMENTS,
    },
    AppProfile {
        id: "mysql",
        name: "MySQL/MariaDB",
        segments: MYSQL_SEGMENTS,
    },
    AppProfile {
        id: "redis",
        name: "Redis",
        segments: REDIS_SEGMENTS,
    },
    AppProfile {
        id: "elastic",
        name: "Elasticsearch",
        segments: ELASTIC_SEGMENTS,
    },
    AppProfile {
        id: "mongo",
        name: "MongoDB",
        segments: MONGO_SEGMENTS,
    },
    AppProfile {
        id: "journal",
        name: "system logs",
        segments: JOURNAL_SEGMENTS,
    },
];

const FALLBACK_GROUPS: &[GroupDef] = &[
    GroupDef {
        name: "Logs",
        paths: LOG_PATHS,
    },
    GroupDef {
        name: "App Data",
        paths: APP_PATHS,
    },
    GroupDef {
        name: "Caches/Temp",
        paths: CACHE_PATHS,
    },
    GroupDef {
        name: "Users",
        paths: USER_PATHS,
    },
];

fn docker_paths() -> Vec<String> {
    let mut paths = DOCKER_PATHS
        .iter()
        .map(|p| (*p).to_string())
        .collect::<Vec<_>>();
    if let Ok(cfg) = std::fs::read_to_string("/etc/docker/daemon.json") {
        if let Some(root) = parse_docker_data_root(&cfg) {
            if !paths.iter().any(|p| p == &root) {
                paths.insert(0, root);
            }
        }
    }
    paths
}

fn parse_docker_data_root(cfg: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(cfg).ok()?;
    let root = v.get("data-root")?.as_str()?;
    root.starts_with('/').then(|| root.to_string())
}

#[derive(Clone)]
struct ResolvedTarget {
    path: String,
    cmp_path: String,
    dev: i64,
    inode: i64,
    size: i64,
    growth: i64,
}

#[allow(clippy::too_many_arguments)]
fn compute_groups(
    store: &Store,
    root_path: &str,
    root_dev: i64,
    root_inode: i64,
    total_size: i64,
    docker_paths: &[String],
    view: GroupView,
    growth_map: &std::collections::HashMap<(i64, i64), i64>,
) -> Vec<GroupRow> {
    let mut assigned: Vec<ResolvedTarget> = Vec::new();
    let mut groups = Vec::new();
    let root_cmp_path = canonical_path_string(root_path);
    let root_growth = growth_map
        .get(&(root_dev, root_inode))
        .copied()
        .unwrap_or(0);

    match view {
        GroupView::Top => {
            for profile in TOP_PROFILES {
                add_profile_group(
                    store,
                    &root_cmp_path,
                    root_dev,
                    root_inode,
                    docker_paths,
                    growth_map,
                    &mut assigned,
                    &mut groups,
                    profile,
                );
            }
            for def in FALLBACK_GROUPS {
                add_def_group(
                    store,
                    &root_cmp_path,
                    root_dev,
                    root_inode,
                    docker_paths,
                    growth_map,
                    &mut assigned,
                    &mut groups,
                    def,
                    None,
                );
            }
        }
        GroupView::Detail(id) => {
            let Some(profile) = TOP_PROFILES.iter().find(|p| p.id == id) else {
                return vec![GroupRow {
                    name: "Other",
                    size: total_size,
                    growth: root_growth,
                    targets: Vec::new(),
                    drill: None,
                }];
            };
            for def in profile.segments {
                add_def_group(
                    store,
                    &root_cmp_path,
                    root_dev,
                    root_inode,
                    docker_paths,
                    growth_map,
                    &mut assigned,
                    &mut groups,
                    def,
                    None,
                );
            }
        }
    }

    if view == GroupView::Top {
        let assigned_size: i64 = groups.iter().map(|g| g.size.max(0)).sum();
        let other_size = total_size.saturating_sub(assigned_size);
        let assigned_growth: i64 = groups.iter().map(|g| g.growth).sum();
        let other_growth = root_growth - assigned_growth;
        if other_size > 0 || other_growth != 0 || groups.is_empty() {
            groups.push(GroupRow {
                name: "Other",
                size: other_size,
                growth: other_growth,
                targets: Vec::new(),
                drill: None,
            });
        }
    }

    groups.sort_by_key(|g| std::cmp::Reverse(g.size.max(0)));
    groups.truncate(12);
    groups
}

#[allow(clippy::too_many_arguments)]
fn add_profile_group(
    store: &Store,
    root_cmp_path: &str,
    root_dev: i64,
    root_inode: i64,
    docker_paths: &[String],
    growth_map: &std::collections::HashMap<(i64, i64), i64>,
    assigned: &mut Vec<ResolvedTarget>,
    groups: &mut Vec<GroupRow>,
    profile: &AppProfile,
) {
    let mut row = GroupRow {
        name: profile.name,
        size: 0,
        growth: 0,
        targets: Vec::new(),
        drill: Some(profile.id),
    };
    for def in profile.segments {
        add_targets(
            store,
            root_cmp_path,
            root_dev,
            root_inode,
            docker_paths,
            growth_map,
            assigned,
            &mut row,
            def,
        );
    }
    if row.size > 0 || row.growth != 0 {
        groups.push(row);
    }
}

#[allow(clippy::too_many_arguments)]
fn add_def_group(
    store: &Store,
    root_cmp_path: &str,
    root_dev: i64,
    root_inode: i64,
    docker_paths: &[String],
    growth_map: &std::collections::HashMap<(i64, i64), i64>,
    assigned: &mut Vec<ResolvedTarget>,
    groups: &mut Vec<GroupRow>,
    def: &GroupDef,
    drill: Option<&'static str>,
) {
    let mut row = GroupRow {
        name: def.name,
        size: 0,
        growth: 0,
        targets: Vec::new(),
        drill,
    };
    add_targets(
        store,
        root_cmp_path,
        root_dev,
        root_inode,
        docker_paths,
        growth_map,
        assigned,
        &mut row,
        def,
    );
    if row.size > 0 || row.growth != 0 {
        groups.push(row);
    }
}

#[allow(clippy::too_many_arguments)]
fn add_targets(
    store: &Store,
    root_cmp_path: &str,
    root_dev: i64,
    root_inode: i64,
    docker_paths: &[String],
    growth_map: &std::collections::HashMap<(i64, i64), i64>,
    assigned: &mut Vec<ResolvedTarget>,
    row: &mut GroupRow,
    def: &GroupDef,
) {
    let paths = if def.name == "Docker/Containers" || def.name == "other docker" {
        docker_paths.to_vec()
    } else {
        def.paths.iter().map(|p| (*p).to_string()).collect()
    };
    for path in paths {
        let Some(mut target) = resolve_group_target(
            store,
            root_cmp_path,
            root_dev,
            root_inode,
            &path,
            growth_map,
        ) else {
            continue;
        };
        let mut covered_by_existing = false;
        for prev in assigned.iter() {
            if path_contains(&prev.cmp_path, &target.cmp_path) {
                covered_by_existing = true;
                break;
            }
            if path_contains(&target.cmp_path, &prev.cmp_path)
                && (target.dev, target.inode) != (prev.dev, prev.inode)
            {
                target.size = target.size.saturating_sub(prev.size);
                target.growth -= prev.growth;
            }
        }
        if covered_by_existing || (target.size <= 0 && target.growth == 0) {
            continue;
        }
        row.size += target.size;
        row.growth += target.growth;
        row.targets.push(GroupTarget {
            path: target.path.clone(),
        });
        assigned.push(target);
    }
}

fn resolve_group_target(
    store: &Store,
    root_cmp_path: &str,
    root_dev: i64,
    root_inode: i64,
    target_path: &str,
    growth_map: &std::collections::HashMap<(i64, i64), i64>,
) -> Option<ResolvedTarget> {
    let target_cmp_path = canonical_path_string(target_path);
    // If the TUI is already scoped inside a known category, charge the whole
    // scoped tree to that category without resolving the ancestor directory.
    if path_contains(&target_cmp_path, root_cmp_path) {
        let (size, _) = node_totals(store, root_dev, root_inode)?;
        return Some(ResolvedTarget {
            path: target_path.to_string(),
            cmp_path: target_cmp_path,
            dev: root_dev,
            inode: root_inode,
            size,
            growth: growth_map
                .get(&(root_dev, root_inode))
                .copied()
                .unwrap_or(0),
        });
    }
    if !path_contains(root_cmp_path, &target_cmp_path) {
        return None;
    }
    let m = std::fs::metadata(target_path).ok()?;
    use std::os::unix::fs::MetadataExt;
    let dev = m.dev() as i64;
    let inode = m.ino() as i64;
    let (size, _) = node_totals(store, dev, inode)?;
    Some(ResolvedTarget {
        path: target_path.to_string(),
        cmp_path: target_cmp_path,
        dev,
        inode,
        size,
        growth: growth_map.get(&(dev, inode)).copied().unwrap_or(0),
    })
}

fn canonical_path_string(path: &str) -> String {
    std::fs::canonicalize(path)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

fn node_totals(store: &Store, dev: i64, inode: i64) -> Option<(i64, i64)> {
    store
        .conn
        .query_row(
            "SELECT CASE WHEN kind='d' THEN recursive_bytes ELSE blocks END,
                    recursive_inodes
             FROM inodes WHERE dev_id=?1 AND inode=?2",
            params![dev, inode],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()
}

fn path_contains(parent: &str, child: &str) -> bool {
    if parent == "/" {
        return child.starts_with('/');
    }
    child == parent
        || child
            .strip_prefix(parent)
            .is_some_and(|s| s.starts_with('/'))
}

fn group_detail(g: &GroupRow) -> String {
    if g.targets.is_empty() {
        return g.name.to_string();
    }
    let mut paths = g
        .targets
        .iter()
        .map(|t| t.path.as_str())
        .collect::<Vec<_>>();
    paths.sort_unstable();
    format!("{}: {}", g.name, paths.join(", "))
}

/// Inode of the db file (0 if unreadable). A rescan renames a NEW inode over the
/// db, so a change here means our open connection is now on the stale old inode.
fn db_inode(db: &std::path::Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(db).map(|m| m.ino()).unwrap_or(0)
}

fn event_loop<B: Backend>(
    term: &mut Terminal<B>,
    app: &mut App,
    db: &std::path::Path,
    snap_tx: &crossbeam_channel::Sender<Snapshot>,
    res_rx: &crossbeam_channel::Receiver<RefreshResult>,
) -> Result<()> {
    // Own a REOPENABLE read connection: an in-place rescan (daemon SIGHUP or a
    // manual `dux scan`) renames a new db inode over the old one, and a connection
    // bound to the old (now-unlinked) inode keeps serving STALE data with no error.
    // We detect the inode change below and reopen, instead of freezing on the
    // pre-rescan snapshot until the user quits.
    let mut store = Store::open_ro(db)?;
    let mut open_ino = db_inode(db);
    let request = |app: &App, tx: &crossbeam_channel::Sender<Snapshot>| {
        let _ = tx.send(Snapshot {
            expanded: app.expanded.clone(),
            metric: app.metric,
            window_secs: app.window_secs,
            gen: app.view_gen,
        });
    };
    request(app, snap_tx); // kick off the first background refresh
    let mut last_input = Instant::now() - Duration::from_secs(10);
    let mut last_request = Instant::now();
    let mut needs_draw = true;
    let mut last_clock_draw = Instant::now();
    loop {
        // Reopen if a rescan swapped the db inode, so the tree stops serving stale
        // data (the worker does the same for panels). One stat per loop is cheap.
        let cur_ino = db_inode(db);
        if cur_ino != 0 && cur_ino != open_ino {
            if let Ok(s) = Store::open_ro(db) {
                store = s;
                open_ino = cur_ino;
                app.view_gen += 1;
                let _ = app.rebuild(&store);
                app.update_detail(&store);
                request(app, snap_tx);
                needs_draw = true;
            }
        }
        // Apply any background refresh results (panels/states always; tree rows
        // only if the structure hasn't changed since the request). Never blocks.
        let mut applied = false;
        while let Ok(r) = res_rx.try_recv() {
            app.apply_refresh(r);
            applied = true;
        }
        if applied {
            app.update_detail(&store);
            needs_draw = true;
        }

        // Redraw only when something changed, plus a low-rate clock tick for
        // "ago"/ETA labels. The old fixed 120ms repaint loop burned CPU in idle
        // TUIs on large terminals even when no input or refresh result arrived.
        if needs_draw || last_clock_draw.elapsed() >= Duration::from_secs(1) {
            term.draw(|f| draw(f, app))?;
            needs_draw = false;
            last_clock_draw = Instant::now();
        }

        // Block up to 120ms for input. If keys arrive, drain the WHOLE burst and
        // redraw once — navigation never waits on a background refresh.
        if event::poll(Duration::from_millis(120))? {
            loop {
                if let Event::Key(k) = event::read()? {
                    if k.kind == KeyEventKind::Press {
                        if k.code == KeyCode::Char('r') {
                            // Manual refresh: rebuild the tree from the DB (cheap,
                            // no fs I/O) and ask the background WORKER to recompute
                            // panels off this thread. Refreshing panels inline here
                            // would block the UI on fs::metadata() of a hung mount
                            // (autofs/NFS) — even 'q' would stop responding.
                            app.rebuild(&store).ok();
                            request(app, snap_tx);
                            last_request = Instant::now();
                        } else if handle_key(app, &store, k.code)? {
                            return Ok(()); // quit
                        }
                    }
                }
                if !event::poll(Duration::from_millis(0))? {
                    break; // burst drained
                }
            }
            last_input = Instant::now();
            needs_draw = true;
            continue; // redraw immediately
        }

        // Idle: ask the worker to recompute live data periodically. The recompute
        // happens off this thread, so the next keypress is never delayed by it.
        // ~4s cadence (not 1.2s): on a high-churn index the recompute is not free,
        // and the worker would otherwise be busy continuously, burning a core.
        if last_input.elapsed() >= Duration::from_millis(250)
            && last_request.elapsed() >= Duration::from_secs(4)
        {
            request(app, snap_tx);
            last_request = Instant::now();
        }
    }
}

/// Handle one keypress. Returns Ok(true) to quit.
fn handle_key(app: &mut App, store: &Store, code: KeyCode) -> Result<bool> {
    // Global keys
    match code {
        KeyCode::Char('q') => return Ok(true),
        KeyCode::Char('a') => {
            app.screen = if app.screen == Screen::Apps {
                Screen::Main
            } else {
                app.focus = Focus::Groups;
                Screen::Apps
            };
            app.update_detail(store);
            return Ok(false);
        }
        KeyCode::Esc if app.screen == Screen::Apps => {
            app.screen = Screen::Main;
            app.update_detail(store);
            return Ok(false);
        }
        KeyCode::Tab => {
            if app.screen == Screen::Apps {
                return Ok(false);
            }
            app.focus = match app.focus {
                Focus::Tree => Focus::Groups,
                Focus::Groups => Focus::Growth,
                Focus::Growth => Focus::Files,
                Focus::Files => Focus::Tree,
            };
            app.update_detail(store);
            return Ok(false);
        }
        _ => {}
    }

    if app.screen == Screen::Apps {
        match code {
            KeyCode::Down | KeyCode::Char('j') if app.asel + 1 < app.groups.len() => app.asel += 1,
            KeyCode::Up | KeyCode::Char('k') => app.asel = app.asel.saturating_sub(1),
            KeyCode::Home | KeyCode::Char('g') => app.asel = 0,
            KeyCode::End | KeyCode::Char('G') => app.asel = app.groups.len().saturating_sub(1),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if let Some(id) = app.groups.get(app.asel).and_then(|g| g.drill) {
                    app.group_view = GroupView::Detail(id);
                    app.asel = 0;
                    app.refresh_groups(store);
                }
            }
            KeyCode::Left | KeyCode::Char('h') if app.group_view != GroupView::Top => {
                app.group_view = GroupView::Top;
                app.asel = 0;
                app.refresh_groups(store);
            }
            _ => {}
        }
        app.update_detail(store);
        return Ok(false);
    }

    match app.focus {
        // ---- panels: ↑↓ select; the footer shows the full path ----
        Focus::Groups | Focus::Growth | Focus::Files => {
            let (len, sel) = match app.focus {
                Focus::Groups => (app.groups.len(), &mut app.asel),
                Focus::Growth => (app.top_growth.len(), &mut app.gsel),
                Focus::Files => (app.top_files.len(), &mut app.fsel),
                Focus::Tree => unreachable!(),
            };
            match code {
                KeyCode::Down | KeyCode::Char('j') if *sel + 1 < len => *sel += 1,
                KeyCode::Up | KeyCode::Char('k') => *sel = sel.saturating_sub(1),
                KeyCode::Home | KeyCode::Char('g') => *sel = 0,
                KeyCode::End | KeyCode::Char('G') => *sel = len.saturating_sub(1),
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l')
                    if app.focus == Focus::Groups =>
                {
                    if let Some(id) = app.groups.get(app.asel).and_then(|g| g.drill) {
                        app.group_view = GroupView::Detail(id);
                        app.asel = 0;
                        app.refresh_groups(store);
                    }
                }
                KeyCode::Left | KeyCode::Esc | KeyCode::Char('h')
                    if app.focus == Focus::Groups && app.group_view != GroupView::Top =>
                {
                    app.group_view = GroupView::Top;
                    app.asel = 0;
                    app.refresh_groups(store);
                }
                _ => {}
            }
            app.update_detail(store);
        }
        // ---- tree ----
        Focus::Tree => {
            match code {
                KeyCode::Down | KeyCode::Char('j') if app.sel + 1 < app.rows.len() => app.sel += 1,
                KeyCode::Up | KeyCode::Char('k') => app.sel = app.sel.saturating_sub(1),
                KeyCode::Enter | KeyCode::Char(' ') => app.toggle(store)?,
                KeyCode::Right | KeyCode::Char('l') => {
                    if let Some(r) = app.rows.get(app.sel) {
                        if r.kind == 'd' && !r.expanded {
                            app.expanded.insert((r.dev, r.inode));
                            app.view_gen += 1;
                            app.rebuild(store)?;
                        }
                        if app.sel + 1 < app.rows.len() {
                            app.sel += 1;
                        }
                    }
                }
                KeyCode::Left | KeyCode::Char('h') => match app.rows.get(app.sel) {
                    Some(r) if r.expanded => {
                        let id = (r.dev, r.inode);
                        app.expanded.remove(&id);
                        app.view_gen += 1;
                        app.rebuild(store)?;
                    }
                    Some(_) => app.ascend(store)?,
                    None => {}
                },
                // 'r' (manual refresh) is handled in the event loop, which owns
                // the worker channel, so the panel recompute stays off this thread.
                KeyCode::Char('i') => {
                    app.metric = if app.metric == Metric::Size {
                        Metric::Inodes
                    } else {
                        Metric::Size
                    };
                    app.view_gen += 1;
                    app.rebuild(store)?;
                }
                KeyCode::Home | KeyCode::Char('g') => app.sel = 0,
                KeyCode::End | KeyCode::Char('G') => app.sel = app.rows.len().saturating_sub(1),
                _ => {}
            }
            app.update_detail(store);
        }
    }
    Ok(false)
}

/// Single UI accent for titles/borders, so the chrome is consistent.
const ACCENT: Color = Color::Cyan;

/// One calm blue for all sizes (the bar's LENGTH conveys magnitude).
const SIZE_COLOR: Color = Color::Rgb(120, 170, 215);
/// Neutral grey for write-rate text (direction shown by ▲/▼, not by color).
const RATE_COLOR: Color = Color::Gray;
/// The ONLY alert color in the UI — used solely for a critically-full disk.
const CRIT_COLOR: Color = Color::Rgb(220, 70, 70);

/// Border style: bright accent for the focused section, dim otherwise.
fn focus_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(ACCENT)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// Human-readable write rate from bytes-in-the-last-hour.
fn rate_str(per_h: i64) -> String {
    if per_h == 0 {
        return "stable".to_string();
    }
    let arrow = if per_h > 0 { '▲' } else { '▼' };
    let a = per_h.saturating_abs(); // saturating: plain abs() panics on i64::MIN
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

/// Pad OR CLIP `s` to exactly `w` terminal COLUMNS. Measured by display width
/// (UnicodeWidthStr), not char count — a CJK/emoji char is 2 columns, so counting
/// chars would let wide filenames push the indent + path right and scatter the
/// tree (exactly what fixed columns must prevent).
fn fixw(s: &str, w: usize, right: bool) -> String {
    use unicode_width::UnicodeWidthChar;
    let width = display_width(s);
    if width >= w {
        // clip by accumulating display columns (keep wide chars whole)
        let mut acc = 0;
        let mut out = String::new();
        for c in s.chars() {
            let cw = c.width().unwrap_or(0);
            if acc + cw > w {
                break;
            }
            acc += cw;
            out.push(c);
        }
        // pad if we stopped just short of w because the next char was wide
        if acc < w {
            out.push_str(&" ".repeat(w - acc));
        }
        return out;
    }
    let pad = " ".repeat(w - width);
    if right {
        format!("{pad}{s}")
    } else {
        format!("{s}{pad}")
    }
}

/// Display width of a string in terminal columns.
fn display_width(s: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    UnicodeWidthStr::width(s)
}

fn short(p: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if display_width(p) <= max {
        return p.to_string();
    }
    // keep the last (max-1) columns, prefixed with an ellipsis
    let budget = max.saturating_sub(1);
    let mut acc = 0;
    let mut tail: Vec<char> = Vec::new();
    for c in p.chars().rev() {
        let cw = c.width().unwrap_or(0);
        if acc + cw > budget {
            break;
        }
        acc += cw;
        tail.push(c);
    }
    tail.reverse();
    let tail: String = tail.into_iter().collect();
    format!("…{tail}")
}

fn group_view_total(app: &App) -> i64 {
    if app.group_view == GroupView::Top {
        app.total_size
    } else {
        app.groups.iter().map(|g| g.size.max(0)).sum()
    }
}

fn draw_apps(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(8),
            Constraint::Length(5),
            Constraint::Length(1),
        ])
        .split(area);

    let title = match app.group_view {
        GroupView::Top => "Apps/OS Distribution",
        GroupView::Detail(id) => TOP_PROFILES
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.name)
            .unwrap_or("Details"),
    };
    let header = vec![
        Line::from(vec![
            Span::styled(
                " dux ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(title, Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(
                format!("   {}", app.root_path),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(Span::styled(
            "Size       Share  Distribution                         Growth       Name",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    f.render_widget(Paragraph::new(header), rows[0]);

    let total = group_view_total(app).max(1);
    let max_group = app
        .groups
        .iter()
        .map(|g| g.size.max(0))
        .max()
        .unwrap_or(1)
        .max(1);
    let body_h = rows[1].height.saturating_sub(2) as usize;
    let start = app.asel.saturating_sub(body_h.saturating_sub(1));
    let end = (start + body_h).min(app.groups.len());
    let mut lines = Vec::new();
    for (idx, g) in app.groups.iter().enumerate().take(end).skip(start) {
        let share = g.size.max(0) as f64 / total as f64 * 100.0;
        let ratio = g.size.max(0) as f64 / max_group as f64;
        let mut line = Line::from(vec![
            Span::styled(
                format!("{} ", fixw(&human(g.size.max(0)), 10, true)),
                Style::default().fg(SIZE_COLOR).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{:>5.1}% ", share),
                Style::default().fg(Color::Gray),
            ),
            Span::styled(
                format!("{} ", bar(ratio, 36)),
                Style::default().fg(SIZE_COLOR),
            ),
            Span::styled(
                format!("{} ", fixw(&rate_str(g.growth), 12, false)),
                Style::default().fg(RATE_COLOR),
            ),
            Span::styled(
                if g.drill.is_some() {
                    format!("{} >", g.name)
                } else {
                    g.name.to_string()
                },
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]);
        if idx == app.asel {
            line = line.style(Style::default().bg(Color::Rgb(38, 44, 66)));
        }
        lines.push(line);
    }
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(focus_style(true))
                .title(Span::styled(
                    " Distribution ",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                )),
        ),
        rows[1],
    );

    let detail = app
        .groups
        .get(app.asel)
        .map(group_detail)
        .unwrap_or_default();
    let selected = app
        .groups
        .get(app.asel)
        .map(|g| {
            format!(
                "{}  {}  {}",
                g.name,
                human(g.size.max(0)),
                rate_str(g.growth)
            )
        })
        .unwrap_or_default();
    let details = vec![
        Line::from(Span::styled(
            selected,
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::raw(short(
            &detail,
            rows[2].width.saturating_sub(4) as usize,
        ))),
    ];
    f.render_widget(
        Paragraph::new(details).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(" Paths "),
        ),
        rows[2],
    );

    let legend = if app.group_view == GroupView::Top {
        " a close · ↑↓ move · Enter/→ drill down · q quit"
    } else {
        " a close · ←/Esc back · ↑↓ move · q quit"
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            legend,
            Style::default().fg(Color::DarkGray),
        ))),
        rows[3],
    );
}

fn draw(f: &mut Frame, app: &mut App) {
    if app.screen == Screen::Apps {
        draw_apps(f, app);
        return;
    }
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
        // Freshness. IMPORTANT: when the daemon is live the index is maintained
        // in realtime, so the time since the last FULL scan is NOT staleness —
        // showing "index 2m old" there wrongly nudges users to rescan. Only a
        // snapshot (daemon off) actually ages; a dirty index is the real warning.
        if let Some(since) = app.dirty_since {
            Span::styled(
                format!("   ⚠ DIRTY {} — rescan recommended", ago(since)),
                Style::default().fg(CRIT_COLOR).add_modifier(Modifier::BOLD),
            )
        } else if let Some(since) = app.paused_since {
            // transient: guardian paused writes under host pressure, nothing lost
            Span::styled(
                format!("   ⏸ writes paused {} ({})", ago(since), app.pause_reason),
                Style::default().fg(RATE_COLOR).add_modifier(Modifier::BOLD),
            )
        } else if let Some(since) = app.throttled_since {
            // live but intentionally behind: the CPU/IO governor is protecting the
            // host under heavy fs activity, so the numbers are ~this stale.
            Span::styled(
                format!(
                    "   ◐ throttled {} stale — capping CPU/IO to protect the host; catches up when load eases",
                    ago(since)
                ),
                Style::default().fg(RATE_COLOR).add_modifier(Modifier::BOLD),
            )
        } else if app.daemon_live {
            Span::styled(
                "   ● live — maintained in realtime",
                Style::default().fg(Color::DarkGray),
            )
        } else {
            Span::styled(
                format!(
                    "   ○ snapshot · {} old (daemon off; growth/ETA need it)",
                    ago(app.last_scan)
                ),
                Style::default().fg(Color::DarkGray),
            )
        },
    ]);
    f.render_widget(Paragraph::new(header), rows[0]);

    // ---- status bar: disk gauge + used/free + growth/day + ETA + items + inodes ----
    let fs = &app.fs;
    let pct = fs.use_pct();
    let bar_w = 18usize;
    let filled = ((pct / 100.0) * bar_w as f64).round() as usize;
    // calm blue normally; the ONE red alert only when the disk is critically full
    let full_color = if pct >= 95.0 { CRIT_COLOR } else { SIZE_COLOR };
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
        Span::styled(growth_str, Style::default().fg(RATE_COLOR)),
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
        .constraints([
            Constraint::Percentage(34),
            Constraint::Percentage(33),
            Constraint::Percentage(33),
        ])
        .split(rows[3]);
    let max_group = app
        .groups
        .iter()
        .map(|g| g.size.max(0))
        .max()
        .unwrap_or(1)
        .max(1);
    let group_total = group_view_total(app);
    let group_title = match app.group_view {
        GroupView::Top => " Apps/OS Heatmap ",
        GroupView::Detail(id) => TOP_PROFILES
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.name)
            .unwrap_or("Details"),
    };
    // The panel shows only `gvis` inner rows but compute_groups can return up to
    // 12 groups. Scroll the window so the selected row is always visible instead
    // of moving an invisible cursor off the bottom edge.
    let gvis = (top[0].height as usize).saturating_sub(2).max(1);
    let gstart = if app.asel >= gvis {
        app.asel + 1 - gvis
    } else {
        0
    };
    let group_items: Vec<Line> = app
        .groups
        .iter()
        .enumerate()
        .skip(gstart)
        .take(gvis)
        .map(|(idx, g)| {
            let ratio = g.size.max(0) as f64 / max_group as f64;
            let pct = if group_total > 0 {
                g.size.max(0) as f64 / group_total as f64 * 100.0
            } else {
                0.0
            };
            let mut line = Line::from(vec![
                Span::styled(
                    format!(" {} ", fixw(&human(g.size.max(0)), 8, true)),
                    Style::default().fg(SIZE_COLOR).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{:>4.0}% ", pct),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{} ", bar(ratio, 8)),
                    Style::default().fg(SIZE_COLOR),
                ),
                Span::styled(
                    format!("{} ", fixw(&rate_str(g.growth), 10, false)),
                    Style::default().fg(RATE_COLOR),
                ),
                Span::raw(short(g.name, 18)),
            ]);
            if app.focus == Focus::Groups && idx == app.asel {
                line = line.style(Style::default().bg(Color::Rgb(38, 44, 66)));
            }
            line
        })
        .collect();
    f.render_widget(
        Paragraph::new(group_items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(focus_style(app.focus == Focus::Groups))
                .title(Span::styled(
                    format!(" {group_title} "),
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                )),
        ),
        top[0],
    );
    let growth_items: Vec<Line> = if app.top_growth.is_empty() {
        vec![Line::from(Span::styled(
            "  (no growth yet — run the daemon)",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        app.top_growth
            .iter()
            .enumerate()
            .map(|(idx, (p, d))| {
                let mut line = Line::from(vec![
                    Span::styled(
                        format!(" {:<11}", rate_str(*d)),
                        Style::default().fg(RATE_COLOR).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(short(p, 40)),
                ]);
                if app.focus == Focus::Growth && idx == app.gsel {
                    line = line.style(Style::default().bg(Color::Rgb(38, 44, 66)));
                }
                line
            })
            .collect()
    };
    f.render_widget(
        Paragraph::new(growth_items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(focus_style(app.focus == Focus::Growth))
                .title(Span::styled(
                    " 🔥 Fastest Growth (1h) ",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                )),
        ),
        top[1],
    );
    let file_items: Vec<Line> = app
        .top_files
        .iter()
        .enumerate()
        .map(|(idx, (p, s, mtime, growth))| {
            // size (calm) · age · "growing" marker (neutral) · path
            let mark = if *growth != 0 { "▲ " } else { "  " };
            let mut line = Line::from(vec![
                Span::styled(
                    format!(" {} ", fixw(&human(*s), 9, true)),
                    Style::default().fg(SIZE_COLOR).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{} ", fixw(&ago(*mtime), 4, true)),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(mark, Style::default().fg(RATE_COLOR)),
                Span::raw(short(p, 34)),
            ]);
            if app.focus == Focus::Files && idx == app.fsel {
                line = line.style(Style::default().bg(Color::Rgb(38, 44, 66)));
            }
            line
        })
        .collect();
    f.render_widget(
        Paragraph::new(file_items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(focus_style(app.focus == Focus::Files))
                .title(Span::styled(
                    " 📦 Largest Files ",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                )),
        ),
        top[2],
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
            count_human(r.inodes)
        } else {
            human(r.size)
        };
        // SIZE channel: one calm color (the bar LENGTH conveys magnitude)
        let size_col = SIZE_COLOR;
        let name = if r.kind == 'd' {
            format!("{}/", r.name)
        } else {
            r.name.clone()
        };
        // WRITE-RATE: r.growth = bytes in the last hour, shown as a neutral
        // ▲/▼ rate (direction by arrow, not by color).
        let rate = rate_str(r.growth);

        // Every column before the path is EXACTLY fixed width (value 10 + bar 12
        // + rate 12, each followed by a space) so the indent + path always start
        // at the same column and the tree can never scatter.
        let line = Line::from(vec![
            Span::styled(
                format!("{} ", fixw(&value, 10, true)),
                Style::default().fg(size_col).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{} ", bar(ratio, 12)),
                Style::default().fg(size_col),
            ),
            Span::styled(
                format!("{} ", fixw(&rate, 12, false)),
                Style::default().fg(RATE_COLOR),
            ),
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
        if selected && app.focus == Focus::Tree {
            line = line.style(Style::default().bg(Color::Rgb(38, 44, 66)));
        }
        lines.push(line);
    }
    let title = if inode_mode {
        " 🌳 Tree — bar = inode count · ▲ = write rate "
    } else {
        " 🌳 Tree — bar = size · ▲ = write rate "
    };
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(focus_style(app.focus == Focus::Tree))
                .title(Span::styled(
                    title,
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                )),
        ),
        body,
    );

    // footer: compact key legend + the FULL path of the current selection
    // (long/truncated panel names are always fully readable here).
    let foot = rows[4];
    let legend = " a apps · Tab section · ↑↓ move · →/⏎ expand · i size⇄inodes · q quit │ ";
    // measure in terminal COLUMNS (display width), and truncate via the same
    // width-aware helper as the tree, so a CJK/emoji path can't overflow the line.
    let avail = (foot.width as usize).saturating_sub(display_width(legend) + 1);
    let shown = short(&app.detail, avail);
    let footer = Line::from(vec![
        Span::styled(legend, Style::default().fg(Color::DarkGray)),
        Span::styled(
            shown,
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
    ]);
    f.render_widget(Paragraph::new(footer), foot);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{self, ScanOptions};

    fn tmp(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("dux-tui-{tag}-{}", std::process::id()))
    }
    fn id_of(p: &std::path::Path) -> (i64, i64) {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::symlink_metadata(p).unwrap();
        (m.dev() as i64, m.ino() as i64)
    }

    // The background worker reuses a shadow App; prove that shadow path produces
    // byte-identical view data (row order + total) to a synchronous build for the
    // same expanded set — i.e. moving the work off-thread changes nothing.
    #[test]
    fn worker_compute_matches_sync() {
        let dir = tmp("match");
        let db = tmp("match-db");
        let _ = std::fs::remove_dir_all(&dir);
        for s in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{s}", db.display()));
        }
        std::fs::create_dir_all(dir.join("a/aa")).unwrap();
        std::fs::create_dir_all(dir.join("b")).unwrap();
        std::fs::write(dir.join("a/aa/f"), vec![1u8; 9000]).unwrap();
        std::fs::write(dir.join("a/g"), vec![2u8; 5000]).unwrap();
        std::fs::write(dir.join("b/h"), vec![3u8; 7000]).unwrap();
        {
            let mut s = Store::open_rw(&db).unwrap();
            scan::scan(
                &mut s,
                &dir,
                &ScanOptions {
                    progress: false,
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let store = Store::open_ro(&db).unwrap();
        let (rdev, rino) = root_node(&store).unwrap();
        let mut expanded = std::collections::HashSet::new();
        expanded.insert((rdev, rino));
        expanded.insert(id_of(&dir.join("a")));
        expanded.insert(id_of(&dir.join("a/aa")));

        let build = |exp: &std::collections::HashSet<(i64, i64)>| {
            let mut app = App::new(&store, &db, rdev, rino);
            app.expanded = exp.clone();
            app.refresh_growth_map(&store);
            app.rebuild(&store).unwrap();
            app.refresh_panels(&store).unwrap();
            (
                app.rows
                    .iter()
                    .map(|r| (r.dev, r.inode))
                    .collect::<Vec<_>>(),
                app.total_size,
                app.items,
            )
        };
        let sync = build(&expanded); // the UI's synchronous build
        let shadow = build(&expanded); // what the worker computes
        assert_eq!(
            sync, shadow,
            "worker shadow build must match the sync build"
        );
        // sanity: the deep file is visible once we expanded into it
        assert!(sync.0.contains(&id_of(&dir.join("a/aa/f"))));

        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
        for s in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{s}", db.display()));
        }
    }

    fn mkrow(dev: i64, inode: i64) -> Row {
        Row {
            dev,
            inode,
            name: String::new(),
            path: String::new(),
            kind: 'd',
            size: 0,
            inodes: 0,
            depth: 0,
            expanded: false,
            has_children: false,
            ratio_size: 0.0,
            ratio_inodes: 0.0,
            growth: 0,
        }
    }
    fn result(gen: u64, rows: Vec<Row>, total: i64) -> RefreshResult {
        RefreshResult {
            gen,
            rows,
            growth_map: std::collections::HashMap::new(),
            top_growth: Vec::new(),
            top_files: Vec::new(),
            groups: Vec::new(),
            total_size: total,
            items: 0,
            growth_per_day: 0,
            last_scan: 0,
            fs: crate::util::FsStat::default(),
            daemon_live: false,
            dirty_since: None,
            paused_since: None,
            pause_reason: String::new(),
            throttled_since: None,
        }
    }

    // A worker result must update panels/totals ALWAYS, but only replace the tree
    // rows when its generation still matches (the user hasn't restructured since).
    #[test]
    fn apply_refresh_respects_generation() {
        let db = tmp("gen-db");
        // a minimal App (no DB access needed for apply_refresh)
        let mut app = App {
            rows: vec![mkrow(1, 1), mkrow(1, 2)],
            expanded: std::collections::HashSet::new(),
            sel: 1,
            root_dev: 1,
            root_inode: 1,
            top_growth: Vec::new(),
            top_files: Vec::new(),
            groups: Vec::new(),
            total_size: 0,
            last_scan: 0,
            window_secs: 3600,
            scroll: 0,
            metric: Metric::Size,
            root_path: "/".into(),
            db,
            docker_paths: Vec::new(),
            group_view: GroupView::Top,
            fs: crate::util::FsStat::default(),
            daemon_live: false,
            dirty_since: None,
            paused_since: None,
            pause_reason: String::new(),
            throttled_since: None,
            growth_map: std::collections::HashMap::new(),
            growth_calc: Instant::now(),
            items: 0,
            growth_per_day: 0,
            screen: Screen::Main,
            focus: Focus::Tree,
            asel: 0,
            gsel: 0,
            fsel: 0,
            detail: String::new(),
            view_gen: 5,
        };
        // matching generation: rows replaced, total applied, selection kept by id
        app.apply_refresh(result(5, vec![mkrow(1, 9), mkrow(1, 2), mkrow(1, 3)], 111));
        assert_eq!(app.rows.len(), 3, "matching gen replaces rows");
        assert_eq!(
            (app.rows[app.sel].dev, app.rows[app.sel].inode),
            (1, 2),
            "selection kept by identity"
        );
        assert_eq!(app.total_size, 111);

        // stale generation: rows untouched, but panels/total still applied
        app.apply_refresh(result(4, vec![mkrow(7, 7)], 222));
        assert_eq!(app.rows.len(), 3, "stale gen must NOT replace rows");
        assert_eq!(app.total_size, 222, "panels/total apply regardless of gen");
    }

    #[test]
    fn parses_docker_data_root() {
        assert_eq!(
            parse_docker_data_root(r#"{ "debug": true, "data-root": "/mnt/docker-data" }"#)
                .as_deref(),
            Some("/mnt/docker-data")
        );
        assert_eq!(
            parse_docker_data_root("{\"data-root\":\"/mnt/docker\\u002ddata\"}").as_deref(),
            Some("/mnt/docker-data")
        );
        assert_eq!(
            parse_docker_data_root(r#"{ "data-root": "relative/path" }"#),
            None
        );
        assert_eq!(parse_docker_data_root(r#"{ "data-root": 42 }"#), None);
        assert_eq!(parse_docker_data_root(r#"{ "log-level": "warn" }"#), None);
    }

    #[test]
    fn path_contains_is_component_aware() {
        assert!(path_contains("/var/lib", "/var/lib/docker"));
        assert!(path_contains("/var/lib", "/var/lib"));
        assert!(!path_contains("/var/lib", "/var/lib2"));
        assert!(path_contains("/", "/var/lib"));
    }

    #[test]
    fn app_profiles_expose_drillable_segments() {
        let nginx = TOP_PROFILES.iter().find(|p| p.id == "nginx").unwrap();
        assert!(nginx.segments.iter().any(|s| s.name == "logs"));
        assert!(nginx.segments.iter().any(|s| s.name == "config"));
        assert!(nginx.segments.iter().any(|s| s.name == "web roots"));

        let os = TOP_PROFILES.iter().find(|p| p.id == "os").unwrap();
        assert!(os.segments.iter().any(|s| s.name == "kernel/boot"));
        assert!(os.segments.iter().any(|s| s.name == "libraries"));
        assert!(os.segments.iter().any(|s| s.name == "package db/cache"));
    }
}

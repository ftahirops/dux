use crate::store::{PathResolver, Store};
use crate::util::{ago, human};
use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    prelude::*,
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use rusqlite::{params, OptionalExtension};
use std::io::stdout;
use std::os::unix::ffi::OsStrExt;
use std::time::{Duration, Instant};

/// Max children loaded per directory in the tree view. A directory with more
/// shows a visible "… more than N entries" marker instead of silently hiding.
const CHILD_LIMIT: usize = 5000;
const LARGE_PAGE_SIZE: usize = 100;

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
    Overview,
    Explore,
    Activity,
    Reclaim,
    LargeFiles,
}

#[derive(Clone, Copy, PartialEq)]
enum LargeSort {
    Size,
    Growth,
}

#[derive(Clone)]
struct LargeFileRow {
    dev: i64,
    inode: i64,
    path: String,
    allocated: i64,
    modified: i64,
    growth: i64,
}

#[derive(Clone, Copy, PartialEq)]
enum Distribution {
    Apps,
    Owners,
    Ages,
    Types,
    SizeBands,
}

impl Distribution {
    fn label(self) -> &'static str {
        match self {
            Self::Apps => "Apps / OS / Users",
            Self::Owners => "Owners",
            Self::Ages => "Last modified",
            Self::Types => "Object types",
            Self::SizeBands => "File size bands",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Apps => Self::Owners,
            Self::Owners => Self::Ages,
            Self::Ages => Self::Types,
            Self::Types => Self::SizeBands,
            Self::SizeBands => Self::Apps,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Apps => Self::SizeBands,
            Self::Owners => Self::Apps,
            Self::Ages => Self::Owners,
            Self::Types => Self::Ages,
            Self::SizeBands => Self::Types,
        }
    }
}

#[derive(Clone)]
struct CleanupRow {
    name: String,
    size: i64,
    safety: &'static str,
    action: String,
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
    name: String,
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
    recent_changes: Vec<(String, i64)>,
    top_files: Vec<(String, i64, i64, i64)>, // path, blocks, mtime, recent growth/h
    large_files: Vec<LargeFileRow>,
    large_total_count: usize,
    large_total_bytes: i64,
    large_page_offset: usize,
    large_selection: usize,
    large_sort: LargeSort,
    groups: Vec<GroupRow>,
    total_size: i64,
    last_scan: i64,
    window_secs: i64,
    scroll: usize,
    metric: Metric,
    root_path: std::path::PathBuf,
    root_display_path: String,
    db: std::path::PathBuf,
    docker_paths: Vec<String>,
    group_view: GroupView,
    fs: crate::util::FsStat,
    daemon_live: bool,
    dirty_since: Option<i64>, // Some(epoch) if the index missed events (overflow)
    paused_since: Option<i64>, // Some(epoch) if writes are paused (host pressure)
    pause_reason: String,     // why writes are paused (low disk / low memory / …)
    throttled_since: Option<i64>, // Some(epoch) if governing keeps the index stale
    watch: Option<crate::util::WatchStatus>,
    scan: Option<crate::util::ScanProgress>,
    containers: Vec<crate::containers::ContainerRow>,
    waste: crate::containers::Waste,
    cleanup: Vec<CleanupRow>,
    storage_calc: Instant,
    // recursive write-rate per node (bytes in the last hour), summed up the tree
    growth_map: std::collections::HashMap<(i64, i64), i64>,
    growth_calc: Instant,
    items: i64,                 // total indexed nodes (files + dirs)
    growth_per_day: i64,        // extrapolated from the last hour of change log
    screen: Screen,             // Overview / Explore / Activity / Reclaim
    explore_distribution: bool, // Explore toggles between tree and grouped view
    distribution: Distribution,
    show_help: bool,
    focus: Focus, // which section the keyboard drives
    asel: usize,  // selected row in the App/OS groups panel
    gsel: usize,  // selected row in the Fastest-Growth panel
    fsel: usize,  // selected row in the Largest-Files panel
    activity_sel: usize,
    reclaim_sel: usize,
    detail: String, // full path of the current selection (shown in footer)
    // Structure generation: bumped whenever `expanded`/`metric` change. A
    // background refresh result is only applied to the TREE if its generation
    // still matches (the user hasn't restructured since) — see the worker.
    view_gen: u64,
    pending_child: Option<(i64, i64)>,
    last_refresh: Option<Instant>,
    refresh_error: Option<String>,
    health_error: Option<String>,
    last_storage: Option<Instant>,
    storage_error: Option<String>,
}

/// Inputs the background worker needs to recompute the view off the input thread.
struct Snapshot {
    expanded: std::collections::HashSet<(i64, i64)>,
    metric: Metric,
    window_secs: i64,
    distribution: Distribution,
    group_view: GroupView,
    screen: Screen,
    large_page_offset: usize,
    large_sort: LargeSort,
    gen: u64,
}

/// View data the worker produces (all owned/`Send`), applied by the UI thread.
struct RefreshResult {
    gen: u64,
    group_view: GroupView,
    distribution: Distribution,
    rows: Vec<Row>,
    growth_map: std::collections::HashMap<(i64, i64), i64>,
    top_growth: Vec<(String, i64)>,
    recent_changes: Vec<(String, i64)>,
    top_files: Vec<(String, i64, i64, i64)>,
    large_files: Vec<LargeFileRow>,
    large_total_count: usize,
    large_total_bytes: i64,
    groups: Vec<GroupRow>,
    total_size: i64,
    items: i64,
    growth_per_day: i64,
    last_scan: i64,
}

/// Background refresh worker: owns its OWN read-only connection and a shadow App
/// (never rendered — just a compute context that reuses rebuild/refresh_panels),
/// so the expensive recursive growth CTEs never run on the input thread. Reopens
/// the connection if the DB is replaced under it (an atomic rescan renames a new
/// file over the old). Exits when the snapshot sender is dropped (UI quit).
type RefreshMessage = std::result::Result<RefreshResult, String>;

fn refresh_worker(
    db: std::path::PathBuf,
    dev: i64,
    inode: i64,
    rx: crossbeam_channel::Receiver<Snapshot>,
    tx: crossbeam_channel::Sender<RefreshMessage>,
) {
    let mut context: Option<App> = None;
    while let Ok(mut snap) = rx.recv() {
        while let Ok(next) = rx.try_recv() {
            snap = next;
        }
        // A fresh connection per request cannot stay pinned to an index that was
        // atomically replaced. Open failures are reported and retried next time.
        let outcome = (|| -> Result<RefreshResult> {
            let mut store = Store::open_ro(&db)?;
            let deadline = Instant::now() + Duration::from_secs(15);
            store
                .conn
                .progress_handler(10000, Some(move || Instant::now() >= deadline))?;
            let shadow = if let Some(shadow) = context.as_mut() {
                shadow
            } else {
                context.insert(App::new(&store, &db, dev, inode)?)
            };
            shadow.expanded = snap.expanded;
            shadow.metric = snap.metric;
            shadow.window_secs = snap.window_secs;
            shadow.distribution = snap.distribution;
            shadow.group_view = snap.group_view;
            shadow.screen = snap.screen;
            shadow.large_page_offset = snap.large_page_offset;
            shadow.large_sort = snap.large_sort;
            let transaction = store.conn.unchecked_transaction()?;
            shadow.refresh_growth_map(&store)?;
            shadow.rebuild(&store)?;
            shadow.refresh_panels(&store)?;
            anyhow::ensure!(
                Instant::now() < deadline,
                "view computation exceeded its 15-second budget"
            );
            transaction.commit()?;
            Ok(RefreshResult {
                gen: snap.gen,
                group_view: snap.group_view,
                distribution: snap.distribution,
                rows: std::mem::take(&mut shadow.rows),
                growth_map: shadow.growth_map.clone(),
                top_growth: std::mem::take(&mut shadow.top_growth),
                recent_changes: std::mem::take(&mut shadow.recent_changes),
                top_files: std::mem::take(&mut shadow.top_files),
                large_files: std::mem::take(&mut shadow.large_files),
                large_total_count: shadow.large_total_count,
                large_total_bytes: shadow.large_total_bytes,
                groups: std::mem::take(&mut shadow.groups),
                total_size: shadow.total_size,
                items: shadow.items,
                growth_per_day: shadow.growth_per_day,
                last_scan: shadow.last_scan,
            })
        })()
        .map_err(|e| format!("{e:#}"));
        if tx.send(outcome).is_err() {
            break;
        }
    }
}

/// Health does not wait behind growth queries, container inspection or mounts.
struct HealthUpdate {
    error: Option<String>,
    live: bool,
    dirty: Option<i64>,
    paused: Option<i64>,
    behind: Option<i64>,
    reason: String,
    watch: Option<crate::util::WatchStatus>,
    scan: Option<crate::util::ScanProgress>,
}

fn health_worker(db: std::path::PathBuf, tx: crossbeam_channel::Sender<HealthUpdate>) {
    loop {
        let opened = Store::open_ro(&db);
        let mut errors = opened
            .as_ref()
            .err()
            .map(|e| vec![format!("{e:#}")])
            .unwrap_or_default();
        let store = opened.ok();
        if let Some(s) = &store {
            let _ = s.conn.busy_timeout(Duration::from_millis(100));
        }
        let mut read_timestamp = |read: Result<Option<i64>>| match read {
            Ok(value) => value,
            Err(error) => {
                errors.push(format!("{error:#}"));
                None
            }
        };
        let dirty = store
            .as_ref()
            .map(|s| read_timestamp(crate::query::dirty_since(s)))
            .unwrap_or(None);
        let paused = store
            .as_ref()
            .map(|s| read_timestamp(crate::query::paused_since(s)))
            .unwrap_or(None);
        let behind = store
            .as_ref()
            .map(|s| read_timestamp(crate::query::throttled_since(s)))
            .unwrap_or(None);
        let reason = match store.as_ref().map(|s| s.get_meta("pause_reason")) {
            Some(Ok(reason)) => reason.unwrap_or_default(),
            Some(Err(error)) => {
                errors.push(format!("{error:#}"));
                String::new()
            }
            None => String::new(),
        };
        let update = HealthUpdate {
            error: (!errors.is_empty()).then(|| errors.join("; ")),
            live: crate::util::daemon_live_for(&db),
            dirty,
            paused,
            behind,
            reason,
            watch: crate::util::read_watch_status(&db),
            scan: crate::util::read_scan_progress_for(&db),
        };
        drop(store);
        if tx.send(update).is_err() {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

struct StorageUpdate {
    fs: crate::util::FsStat,
    containers: Vec<crate::containers::ContainerRow>,
    waste: crate::containers::Waste,
    cleanup: Vec<CleanupRow>,
}

fn storage_worker(
    db: std::path::PathBuf,
    dev: i64,
    inode: i64,
    tx: crossbeam_channel::Sender<std::result::Result<StorageUpdate, String>>,
) {
    loop {
        let update = (|| -> Result<StorageUpdate> {
            let store = Store::open_ro(&db)?;
            let mut shadow = App::new(&store, &db, dev, inode)?;
            // No read transaction is held during these potentially slow calls.
            shadow.refresh_storage(&store)?;
            let fs = crate::util::fs_stat_checked(&shadow.root_path)?;
            Ok(StorageUpdate {
                fs,
                containers: shadow.containers,
                waste: shadow.waste,
                cleanup: shadow.cleanup,
            })
        })()
        .map_err(|e| format!("{e:#}"));
        if tx.send(update).is_err() {
            break;
        }
        std::thread::sleep(Duration::from_secs(30));
    }
}

/// A one-slot mailbox stores the newest requested view, not an unbounded queue.
fn request_refresh(
    app: &App,
    tx: &crossbeam_channel::Sender<Snapshot>,
    queued: &crossbeam_channel::Receiver<Snapshot>,
) {
    let snapshot = Snapshot {
        expanded: app.expanded.clone(),
        metric: app.metric,
        window_secs: app.window_secs,
        distribution: app.distribution,
        group_view: app.group_view,
        screen: app.screen,
        large_page_offset: app.large_page_offset,
        large_sort: app.large_sort,
        gen: app.view_gen,
    };
    if let Err(crossbeam_channel::TrySendError::Full(snapshot)) = tx.try_send(snapshot) {
        let _ = queued.try_recv();
        let _ = tx.try_send(snapshot);
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
        let root = match start {
            Some(root) => Some(root),
            None => root_node(&store)?,
        };
        let (dev, inode) = match root {
            Some(v) => v,
            None => {
                println!("empty index — run `dux scan <PATH>` first");
                return Ok(());
            }
        };
        let mut app = App::new(&store, db, dev, inode)?;
        // First frame = the TREE only (cheap, indexed per-dir queries). The
        // expensive growth-heat + panels are computed by the background worker and
        // applied when ready — so the TUI opens INSTANTLY even on a huge, high-churn
        // index instead of blocking ~30s on the recursive growth query.
        app.init_root(&store)?;
        app.update_detail();
        (dev, inode, app)
    };

    // Spawn the background refresh worker — it recomputes the heavy view data off
    // the input thread so navigation never blocks (M4). Channels: snapshots out,
    // results in. When `snap_tx` drops (run returns), the worker exits.
    let (snap_tx, snap_rx) = crossbeam_channel::bounded::<Snapshot>(1);
    let (res_tx, res_rx) = crossbeam_channel::bounded::<RefreshMessage>(1);
    {
        let dbp = db.to_path_buf();
        let worker_rx = snap_rx.clone();
        std::thread::spawn(move || refresh_worker(dbp, dev, inode, worker_rx, res_tx));
    }

    let (health_tx, health_rx) = crossbeam_channel::bounded(1);
    let health_db = db.to_path_buf();
    std::thread::spawn(move || health_worker(health_db, health_tx));

    let (storage_tx, storage_rx) = crossbeam_channel::bounded(1);
    let storage_db = db.to_path_buf();
    std::thread::spawn(move || storage_worker(storage_db, dev, inode, storage_tx));

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
    let res = event_loop(
        &mut term,
        &mut app,
        &snap_tx,
        &snap_rx,
        &res_rx,
        &health_rx,
        &storage_rx,
    );
    term.show_cursor().ok();
    res
}

/// Resolve the indexed root from stored meta, else a self-parent fallback.
fn root_node(store: &Store) -> Result<Option<(i64, i64)>> {
    if let Some(root) = crate::query::index_root(store)? {
        return Ok(Some(root));
    }
    Ok(store
        .conn
        .query_row(
            "SELECT dev_id, inode FROM inodes WHERE kind='d'
             ORDER BY recursive_bytes DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

impl App {
    /// Construct an App rooted at (dev,inode). Used by the UI and by the
    /// background refresh worker's shadow context (which reuses rebuild/
    /// refresh_panels to compute off the input thread — see RefreshWorker).
    fn new(store: &Store, db: &std::path::Path, dev: i64, inode: i64) -> Result<App> {
        let root_path = store.path_buf_of(dev, inode)?;
        let root_display_path = store.path_of_display(dev, inode)?;
        anyhow::ensure!(
            !root_path.as_os_str().as_bytes().starts_with(b"inode:"),
            "indexed TUI root ({dev},{inode}) has no visible path"
        );
        Ok(App {
            rows: Vec::new(),
            expanded: std::collections::HashSet::new(),
            sel: 0,
            root_dev: dev,
            root_inode: inode,
            top_growth: Vec::new(),
            recent_changes: Vec::new(),
            top_files: Vec::new(),
            large_files: Vec::new(),
            large_total_count: 0,
            large_total_bytes: 0,
            large_page_offset: 0,
            large_selection: 0,
            large_sort: LargeSort::Size,
            groups: Vec::new(),
            total_size: 0,
            last_scan: 0,
            window_secs: 3600,
            scroll: 0,
            metric: Metric::Size,
            root_path,
            root_display_path,
            db: db.to_path_buf(),
            docker_paths: docker_paths()?,
            group_view: GroupView::Top,
            fs: crate::util::FsStat::default(),
            daemon_live: false,
            dirty_since: None,
            paused_since: None,
            pause_reason: String::new(),
            throttled_since: None,
            watch: None,
            scan: None,
            containers: Vec::new(),
            waste: crate::containers::Waste::default(),
            cleanup: Vec::new(),
            storage_calc: Instant::now() - Duration::from_secs(60),
            growth_map: std::collections::HashMap::new(),
            growth_calc: Instant::now() - Duration::from_secs(60),
            items: 0,
            growth_per_day: 0,
            screen: Screen::Overview,
            explore_distribution: false,
            distribution: Distribution::Apps,
            show_help: false,
            focus: Focus::Tree,
            asel: 0,
            gsel: 0,
            fsel: 0,
            activity_sel: 0,
            reclaim_sel: 0,
            detail: String::new(),
            view_gen: 0,
            pending_child: None,
            last_refresh: None,
            refresh_error: None,
            health_error: None,
            last_storage: None,
            storage_error: None,
        })
    }

    /// Apply a worker refresh. Panels/totals/states/growth-map are always taken
    /// (they don't depend on the tree structure); the row list is taken only when
    /// the user hasn't restructured since the request (`gen` still current) — else
    /// a synchronous rebuild already produced the up-to-date rows.
    fn apply_refresh(&mut self, r: RefreshResult) {
        if r.gen != self.view_gen {
            return;
        }
        self.last_refresh = Some(Instant::now());
        self.refresh_error = None;
        self.growth_map = r.growth_map;
        self.top_growth = r.top_growth;
        self.recent_changes = r.recent_changes;
        self.top_files = r.top_files;
        self.large_files = r.large_files;
        self.large_total_count = r.large_total_count;
        self.large_total_bytes = r.large_total_bytes;
        self.large_selection = self
            .large_selection
            .min(self.large_files.len().saturating_sub(1));
        // Group drill state travels with the request; only the matching view
        // may replace these rows.
        if self.group_view == r.group_view
            && self.distribution == r.distribution
            && self.view_gen == r.gen
        {
            self.groups = r.groups;
        }
        self.total_size = r.total_size;
        self.items = r.items;
        self.growth_per_day = r.growth_per_day;
        self.last_scan = r.last_scan;
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
        if let Some(id) = self.pending_child.take() {
            if let Some(parent) = self.rows.iter().position(|r| (r.dev, r.inode) == id) {
                if parent + 1 < self.rows.len()
                    && self.rows[parent + 1].depth > self.rows[parent].depth
                {
                    self.sel = parent + 1;
                }
            }
        }
        self.asel = self.asel.min(self.groups.len().saturating_sub(1));
        self.gsel = self.gsel.min(self.top_growth.len().saturating_sub(1));
        self.fsel = self.fsel.min(self.top_files.len().saturating_sub(1));
        self.activity_sel = self
            .activity_sel
            .min(self.recent_changes.len().saturating_sub(1));
        self.reclaim_sel = self.reclaim_sel.min(self.cleanup.len().saturating_sub(1));
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
    fn refresh_growth_map(&mut self, store: &Store) -> Result<()> {
        if self.growth_calc.elapsed() < Duration::from_secs(3) {
            return Ok(());
        }
        let cutoff =
            crate::util::now_secs().saturating_sub(3600) / crate::store::GROWTH_BUCKET_SECS;
        let mut map = std::collections::HashMap::new();
        let mut stmt = store.conn.prepare(
            "WITH RECURSIVE
               chg(dev,ino,d) AS (
                 SELECT dev_id,inode,SUM(delta) s FROM growth_current WHERE bucket>=?1
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
        )?;
        let rows = stmt.query_map(params![cutoff], |r| {
            Ok((
                (r.get::<_, i64>(0)?, r.get::<_, i64>(1)?),
                r.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (id, delta) = row?;
            map.insert(id, delta);
        }
        self.growth_map = map;
        self.growth_calc = Instant::now();
        Ok(())
    }

    fn rebuild(&mut self, store: &Store) -> Result<()> {
        // NOTE: the expensive recursive growth CTE (refresh_growth_map) is NOT run
        // here — it's owned by the background worker so a key-driven rebuild stays
        // cheap (just per-directory indexed queries). Rows use the cached
        // `growth_map`; the worker refreshes it.
        let sel_id = self.rows.get(self.sel).map(|r| (r.dev, r.inode));
        let mut out: Vec<Row> = Vec::new();

        // root row (name is the scan root path; totals live on the inode)
        let (size, inodes): (i64, i64) = store.conn.query_row(
            "SELECT recursive_bytes, recursive_inodes FROM inodes WHERE dev_id=?1 AND inode=?2",
            params![self.root_dev, self.root_inode],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let name = self.root_display_path.clone();
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
            self.append_children(
                store,
                &mut out,
                self.root_dev,
                self.root_inode,
                1,
                &self.root_display_path,
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

    fn refresh_groups(&mut self, store: &Store) -> Result<()> {
        self.groups = if self.distribution == Distribution::Apps {
            compute_groups(
                store,
                &self.root_path.to_string_lossy(),
                self.root_dev,
                self.root_inode,
                self.total_size,
                &self.docker_paths,
                self.group_view,
                &self.growth_map,
            )?
        } else {
            aggregate_groups(store, self)?
        };
        self.asel = self.asel.min(self.groups.len().saturating_sub(1));
        Ok(())
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
        parent_display_path: &str,
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
        // truncated? drop the probe row and remember to show a marker.
        let truncated = kids.len() > CHILD_LIMIT;
        if truncated {
            kids.truncate(CHILD_LIMIT);
        }
        let maxs = kids.iter().map(|k| k.4).max().unwrap_or(1).max(1);
        let maxi = kids.iter().map(|k| k.5).max().unwrap_or(1).max(1);

        for (cdev, cino, name, kind, size, inodes) in kids {
            let is_expanded = kind == 'd' && self.expanded.contains(&(cdev, cino));
            // a directory is only collapsible/expandable if it actually has
            // descendants (recursive_inodes counts itself, so >1 means non-empty).
            let has_children = kind == 'd' && inodes > 1;
            let display_path = if parent_display_path.ends_with('/') {
                format!("{parent_display_path}{name}")
            } else {
                format!("{parent_display_path}/{name}")
            };
            out.push(Row {
                dev: cdev,
                inode: cino,
                name,
                path: display_path.clone(),
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
                self.append_children(store, out, cdev, cino, depth + 1, &display_path)?;
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

    fn toggle(&mut self) -> Result<()> {
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
        self.collapse_cached();
        Ok(())
    }

    /// Move to the parent of the selection and collapse it.
    fn ascend(&mut self) -> Result<()> {
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
        self.collapse_cached();
        self.sel = self
            .rows
            .iter()
            .position(|r| (r.dev, r.inode) == id)
            .unwrap_or(0);
        Ok(())
    }

    fn collapse_cached(&mut self) {
        let mut hidden_below = None;
        self.rows.retain_mut(|row| {
            if hidden_below.is_some_and(|depth| row.depth > depth) {
                return false;
            }
            hidden_below = None;
            row.expanded = self.expanded.contains(&(row.dev, row.inode));
            if row.kind == 'd' && !row.expanded {
                hidden_below = Some(row.depth);
            }
            true
        });
        self.sel = self.sel.min(self.rows.len().saturating_sub(1));
    }

    /// When the TUI is opened at a subtree (not the whole index root), the
    /// largest-files / fastest-growth panels must be scoped to that subtree too,
    /// or they'd show unrelated global entries. Returns the ` AND (...) IN (...)`
    /// clause + bind params, or empty strings when viewing the whole index.
    fn panel_scope(&self, store: &Store) -> Result<(String, Vec<i64>)> {
        if crate::query::index_root(store)? == Some((self.root_dev, self.root_inode)) {
            Ok((String::new(), Vec::new())) // viewing the whole index — no filter
        } else {
            Ok((
                format!(" AND (dev_id,inode) IN ({})", crate::query::SUBTREE_CTE),
                vec![self.root_dev, self.root_inode],
            ))
        }
    }

    fn refresh_panels(&mut self, store: &Store) -> Result<()> {
        self.total_size = store.conn.query_row(
            "SELECT recursive_bytes FROM inodes WHERE dev_id=?1 AND inode=?2",
            params![self.root_dev, self.root_inode],
            |r| r.get(0),
        )?;
        self.last_scan = crate::query::optional_timestamp(store, "last_scan_ts")?.unwrap_or(0);
        // status-bar aggregates
        self.items = store.conn.query_row(
            "SELECT recursive_inodes FROM inodes WHERE dev_id=?1 AND inode=?2",
            params![self.root_dev, self.root_inode],
            |r| r.get(0),
        )?;
        // growth/day = bytes written in the last hour, extrapolated (×24).
        // Only meaningful with a running daemon + history; 0 otherwise.
        let hour_ago =
            crate::util::now_secs().saturating_sub(3600) / crate::store::GROWTH_BUCKET_SECS;
        let last_hour: i64 = store.conn.query_row(
            "SELECT COALESCE(SUM(delta),0) FROM growth_current WHERE bucket>=?1 AND delta>0",
            params![hour_ago],
            |r| r.get(0),
        )?;
        self.growth_per_day = last_hour.saturating_mul(24);

        let cutoff = crate::util::now_secs().saturating_sub(self.window_secs)
            / crate::store::GROWTH_BUCKET_SECS;
        let (scope_sql, scope_args) = self.panel_scope(store)?;
        let mut pr = PathResolver::new(&store.conn);
        // pull extra rows; we drop unresolved/duplicate paths then take 6
        let gsql = format!(
            "SELECT dev_id, inode, SUM(delta) d FROM growth_current WHERE bucket>=?1{scope_sql}
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
        let mut seen_paths: std::collections::HashSet<std::path::PathBuf> =
            std::collections::HashSet::new();
        let mut top_growth = Vec::new();
        for row in g {
            let (d, i, delta) = row?;
            let path = pr.resolve_path_buf(d, i)?;
            if path.as_os_str().as_bytes().starts_with(b"inode:")
                || crate::util::is_index_artifact(&path, &self.db)
                || !seen_paths.insert(path.clone())
            {
                continue;
            }
            top_growth.push((pr.resolve_display(d, i)?, delta));
            if top_growth.len() == 6 {
                break;
            }
        }
        self.top_growth = top_growth;

        // Activity is sourced entirely from the compact, bucketed change log.
        // It never scans the filesystem and is bounded to 30 resolved paths.
        let activity_scope = if scope_sql.is_empty() {
            None
        } else {
            Some((self.root_dev, self.root_inode))
        };
        self.recent_changes = crate::query::changed(store, self.window_secs, 30, activity_scope)?
            .into_iter()
            .filter(|r| {
                !r.path.starts_with("inode:")
                    && !crate::util::is_index_artifact(std::path::Path::new(&r.path), &self.db)
            })
            .map(|r| (crate::util::display_path(&r.path), r.delta))
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
        let mut top_files = Vec::new();
        for row in f {
            let (d, i, blocks, mtime) = row?;
            let growth = self.growth_map.get(&(d, i)).copied().unwrap_or(0);
            let path = pr.resolve_path_buf(d, i)?;
            if !crate::util::is_index_artifact(&path, &self.db) {
                top_files.push((pr.resolve_display(d, i)?, blocks, mtime, growth));
            }
        }
        self.top_files = top_files;
        if self.screen == Screen::LargeFiles {
            self.refresh_large_files(store, &scope_sql, &scope_args)?;
        } else {
            self.large_files.clear();
        }
        self.refresh_groups(store)?;
        // keep panel selections in range as panels change
        self.asel = self.asel.min(self.groups.len().saturating_sub(1));
        self.gsel = self.gsel.min(self.top_growth.len().saturating_sub(1));
        self.fsel = self.fsel.min(self.top_files.len().saturating_sub(1));
        self.activity_sel = self
            .activity_sel
            .min(self.recent_changes.len().saturating_sub(1));
        self.reclaim_sel = self.reclaim_sel.min(self.cleanup.len().saturating_sub(1));
        Ok(())
    }

    fn refresh_large_files(
        &mut self,
        store: &Store,
        scope_sql: &str,
        scope_args: &[i64],
    ) -> Result<()> {
        let cutoff = (crate::util::now_secs()
            .saturating_sub(self.window_secs)
            .saturating_add(crate::store::GROWTH_BUCKET_SECS - 1))
            / crate::store::GROWTH_BUCKET_SECS;
        let filter = if scope_sql.is_empty() {
            String::new()
        } else {
            format!(" AND (i.dev_id,i.inode) IN ({})", crate::query::SUBTREE_CTE)
        };
        let (order, secondary) = match self.large_sort {
            LargeSort::Size => ("i.blocks DESC", "i.blocks DESC"),
            LargeSort::Growth => (
                "CASE WHEN COALESCE(c.delta,0)=(-9223372036854775807-1)
                 THEN 9223372036854775807 ELSE ABS(COALESCE(c.delta,0)) END DESC",
                "COALESCE(c.delta,0) DESC",
            ),
        };
        let count_args = scope_args.to_vec();
        let count_sql = format!(
            "SELECT COUNT(*),COALESCE(SUM(i.blocks),0) FROM inodes i
             WHERE i.kind!='d'{filter}"
        );
        let (count, bytes): (i64, i64) = store.conn.query_row(
            &count_sql,
            rusqlite::params_from_iter(count_args.iter()),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        self.large_total_count = count.max(0) as usize;
        self.large_total_bytes = bytes.max(0);
        let sql = format!(
            "WITH c AS (
               SELECT dev_id,inode,SUM(delta) delta FROM growth_current WHERE bucket>=?1 GROUP BY dev_id,inode
             )
             SELECT i.dev_id,i.inode,i.blocks,i.mtime,COALESCE(c.delta,0)
             FROM inodes i LEFT JOIN c ON c.dev_id=i.dev_id AND c.inode=i.inode
             WHERE i.kind!='d'{filter}
             ORDER BY {order}, {secondary}, i.dev_id, i.inode LIMIT ? OFFSET ?"
        );
        let mut args = vec![cutoff];
        args.extend_from_slice(scope_args);
        args.push(LARGE_PAGE_SIZE as i64);
        args.push(self.large_page_offset.min(i64::MAX as usize) as i64);
        let mut stmt = store.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args.iter()), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        let mut resolver = PathResolver::new(&store.conn);
        self.large_files = rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(
                |(dev, inode, allocated, modified, growth)| -> Result<LargeFileRow> {
                    Ok(LargeFileRow {
                        dev,
                        inode,
                        path: resolver.resolve_display(dev, inode)?,
                        allocated,
                        modified,
                        growth,
                    })
                },
            )
            .collect::<Result<_>>()?;
        Ok(())
    }

    fn refresh_storage(&mut self, store: &Store) -> Result<()> {
        if self.storage_calc.elapsed() < Duration::from_secs(30) {
            return Ok(());
        }
        self.storage_calc = Instant::now();
        self.containers = crate::containers::list(store)?;
        self.waste = crate::containers::waste(store, &self.containers)?;
        let mut rows = Vec::new();
        let mut add = |name: &str, size: i64, safety: &'static str, action: &str| {
            if size > 0 {
                rows.push(CleanupRow {
                    name: name.into(),
                    size,
                    safety,
                    action: action.into(),
                });
            }
        };
        add(
            "Stopped container layers + logs",
            self.waste.stopped_bytes,
            "verify",
            "docker container prune  # inspect stopped containers first",
        );
        add(
            "Unreferenced Docker volumes",
            self.waste.orphan_volume_bytes,
            "verify",
            "docker volume prune  # irreversible; verify backups/owners",
        );
        add(
            "Docker build cache",
            self.waste.build_cache_bytes,
            "safe-ish",
            "docker builder prune  # cache is reproducible but rebuilds get slower",
        );

        let package_cache: i64 = [
            "/var/cache/apt/archives",
            "/var/cache/dnf",
            "/var/cache/yum",
            "/var/cache/pacman/pkg",
        ]
        .iter()
        .map(|p| indexed_path_size(store, p))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .fold(0i64, i64::saturating_add);
        add(
            "Package download caches",
            package_cache,
            "safe-ish",
            "use your package manager's clean command (apt/dnf/yum/pacman)",
        );
        add(
            "systemd journal",
            indexed_path_size(store, "/var/log/journal")?,
            "policy",
            "journalctl --disk-usage; journalctl --vacuum-time=14d",
        );
        let temp: i64 = ["/tmp", "/var/tmp"]
            .iter()
            .map(|p| indexed_path_size(store, p))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .fold(0i64, i64::saturating_add);
        add(
            "Temporary areas (upper bound)",
            temp,
            "review",
            "review age/owners; remove only stale files, never the directories",
        );
        let trash = indexed_path_size(store, "/root/.local/share/Trash")?;
        add(
            "Root user's trash",
            trash,
            "safe-ish",
            "review /root/.local/share/Trash before emptying",
        );

        let page_size: i64 = store.conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        let free_pages: i64 = store
            .conn
            .query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
        add(
            "Unused pages inside dux index",
            page_size.saturating_mul(free_pages),
            "safe",
            "stop writer briefly, then sqlite3 <dux.db> 'VACUUM;'",
        );
        rows.sort_by_key(|r| std::cmp::Reverse(r.size));
        self.cleanup = rows;
        Ok(())
    }

    /// Full path of the current selection (focused section) — shown in the footer
    /// so long/truncated names are always fully visible.
    fn update_detail(&mut self) {
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

fn indexed_path_size(store: &Store, path: &str) -> Result<i64> {
    let Some((dev, ino)) = crate::store::indexed_path(&store.conn, std::path::Path::new(path))?
    else {
        return Ok(0);
    };
    node_totals(store, dev, ino)?
        .map(|totals| totals.0)
        .with_context(|| {
            format!(
                "indexed path {} references missing inode ({dev},{ino})",
                path
            )
        })
}

fn owner_name(uid: i64) -> String {
    std::fs::read_to_string("/etc/passwd")
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                let mut fields = line.split(':');
                let name = fields.next()?;
                let _password = fields.next()?;
                let id = fields.next()?.parse::<i64>().ok()?;
                (id == uid).then(|| format!("{name} (uid {uid})"))
            })
        })
        .unwrap_or_else(|| format!("uid {uid}"))
}

/// Alternate Explore distributions. These are aggregate SQL queries over the
/// compact index, executed by the background worker only for the selected tab.
fn aggregate_groups(store: &Store, app: &App) -> Result<Vec<GroupRow>> {
    let (scope, scope_args) = app.panel_scope(store)?;
    let now = crate::util::now_secs();
    let (sql, mut args): (String, Vec<i64>) = match app.distribution {
        Distribution::Owners => (
            format!(
                "SELECT CAST(uid AS TEXT), SUM(blocks), COUNT(*) FROM inodes WHERE 1=1{scope} GROUP BY uid ORDER BY 2 DESC LIMIT 30"
            ),
            Vec::new(),
        ),
        Distribution::Ages => (
            format!(
                "SELECT CASE WHEN mtime>=?1 THEN 'Modified today' WHEN mtime>=?2 THEN 'Modified this week' WHEN mtime>=?3 THEN 'Modified this month' WHEN mtime>=?4 THEN 'Modified this year' ELSE 'Older than one year' END band, SUM(blocks), COUNT(*) FROM inodes WHERE 1=1{scope} GROUP BY band ORDER BY 2 DESC"
            ),
            vec![now - 86_400, now - 604_800, now - 2_592_000, now - 31_536_000],
        ),
        Distribution::Types => (
            format!(
                "SELECT CASE kind WHEN 'd' THEN 'Directories' WHEN 'f' THEN 'Regular files' WHEN 'l' THEN 'Symbolic links' WHEN 'b' THEN 'Block devices' WHEN 'c' THEN 'Character devices' WHEN 'p' THEN 'Named pipes' WHEN 's' THEN 'Sockets' ELSE 'Other' END type, SUM(blocks), COUNT(*) FROM inodes WHERE 1=1{scope} GROUP BY type ORDER BY 2 DESC"
            ),
            Vec::new(),
        ),
        Distribution::SizeBands => (
            format!(
                "SELECT CASE WHEN blocks>=10737418240 THEN '10 GiB and larger' WHEN blocks>=1073741824 THEN '1–10 GiB' WHEN blocks>=104857600 THEN '100 MiB–1 GiB' WHEN blocks>=10485760 THEN '10–100 MiB' WHEN blocks>=1048576 THEN '1–10 MiB' WHEN blocks>=102400 THEN '100 KiB–1 MiB' ELSE 'Under 100 KiB' END band, SUM(blocks), COUNT(*) FROM inodes WHERE kind!='d'{scope} GROUP BY band ORDER BY 2 DESC"
            ),
            Vec::new(),
        ),
        Distribution::Apps => return Ok(Vec::new()),
    };
    args.extend(scope_args);
    let mut stmt = store.conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(args.iter()), |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    rows.map(|row| -> Result<GroupRow> {
        let (mut name, size, count) = row?;
        if app.distribution == Distribution::Owners {
            name = name.parse::<i64>().map(owner_name).unwrap_or(name);
        }
        Ok(GroupRow {
            name: format!("{name} · {} items", count_human(count)),
            size,
            growth: 0,
            targets: Vec::new(),
            drill: None,
        })
    })
    .collect()
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
const USER_SEGMENTS: &[GroupDef] = &[
    GroupDef {
        name: "home directories",
        paths: &["/home"],
    },
    GroupDef {
        name: "root user",
        paths: &["/root"],
    },
];

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
        id: "users",
        name: "Users",
        segments: USER_SEGMENTS,
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
];

fn docker_paths() -> Result<Vec<String>> {
    let mut paths = DOCKER_PATHS
        .iter()
        .map(|p| (*p).to_string())
        .collect::<Vec<_>>();
    let config_path = std::path::Path::new("/etc/docker/daemon.json");
    let cfg = match std::fs::read_to_string(config_path) {
        Ok(config) => Some(config),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", config_path.display()))
        }
    };
    if let Some(cfg) = cfg {
        if let Some(root) = parse_docker_data_root(&cfg)? {
            if !paths.iter().any(|p| p == &root) {
                paths.insert(0, root);
            }
        }
    }
    Ok(paths)
}

fn parse_docker_data_root(cfg: &str) -> Result<Option<String>> {
    let value: serde_json::Value =
        serde_json::from_str(cfg).context("parsing Docker daemon config")?;
    let object = value
        .as_object()
        .context("Docker daemon configuration must be a JSON object")?;
    for key in ["data-root", "graph"] {
        if let Some(value) = object.get(key) {
            let root = value
                .as_str()
                .with_context(|| format!("Docker {key} must be a string"))?;
            if root.is_empty() {
                continue;
            }
            anyhow::ensure!(
                std::path::Path::new(root).is_absolute(),
                "Docker {key} must be absolute"
            );
            return Ok(Some(root.to_string()));
        }
    }
    Ok(None)
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
) -> Result<Vec<GroupRow>> {
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
                )?;
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
                )?;
            }
        }
        GroupView::Detail(id) => {
            if id == "users" {
                return compute_user_groups(store, &root_cmp_path, growth_map);
            }
            let Some(profile) = TOP_PROFILES.iter().find(|p| p.id == id) else {
                return Ok(vec![GroupRow {
                    name: "Other".into(),
                    size: total_size,
                    growth: root_growth,
                    targets: Vec::new(),
                    drill: None,
                }]);
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
                )?;
            }
        }
    }

    if view == GroupView::Top {
        let assigned_size: i64 = groups
            .iter()
            .map(|g| g.size.max(0))
            .fold(0i64, i64::saturating_add);
        let other_size = total_size.saturating_sub(assigned_size);
        let assigned_growth: i64 = groups
            .iter()
            .map(|g| g.growth)
            .fold(0i64, i64::saturating_add);
        let other_growth = root_growth.saturating_sub(assigned_growth);
        if other_size > 0 || other_growth != 0 || groups.is_empty() {
            groups.push(GroupRow {
                name: "Other".into(),
                size: other_size,
                growth: other_growth,
                targets: Vec::new(),
                drill: None,
            });
        }
    }

    groups.sort_by_key(|g| std::cmp::Reverse(g.size.max(0)));
    groups.truncate(12);
    Ok(groups)
}

fn compute_user_groups(
    store: &Store,
    root_cmp_path: &str,
    growth_map: &std::collections::HashMap<(i64, i64), i64>,
) -> Result<Vec<GroupRow>> {
    let mut out = Vec::new();
    if path_contains(root_cmp_path, "/home") {
        if let Some(meta) = optional_metadata(std::path::Path::new("/home"))? {
            let (dev, inode) = crate::util::indexed_identity(&meta)?;
            let mut stmt = store.conn.prepare(
                "SELECT d.name,d.dev_id,d.inode,i.recursive_bytes
                 FROM dirents d JOIN inodes i
                   ON i.dev_id=d.dev_id AND i.inode=d.inode
                 WHERE d.parent_dev=?1 AND d.parent_inode=?2 AND i.kind='d'
                 ORDER BY i.recursive_bytes DESC LIMIT 1000",
            )?;
            let rows = stmt.query_map(params![dev, inode], |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?;
            for row in rows {
                let (name, dev, inode, size) = row?;
                let shown = crate::util::display_name(&name);
                out.push(GroupRow {
                    name: shown.clone(),
                    size,
                    growth: growth_map.get(&(dev, inode)).copied().unwrap_or(0),
                    targets: vec![GroupTarget {
                        path: format!("/home/{shown}"),
                    }],
                    drill: None,
                });
            }
        }
    }
    if path_contains(root_cmp_path, "/root") {
        if let Some(meta) = optional_metadata(std::path::Path::new("/root"))? {
            let id = crate::util::indexed_identity(&meta)?;
            if let Some((size, _)) = node_totals(store, id.0, id.1)? {
                let growth = growth_map.get(&id).copied().unwrap_or(0);
                out.push(GroupRow {
                    name: "root".into(),
                    size,
                    growth,
                    targets: vec![GroupTarget {
                        path: "/root".into(),
                    }],
                    drill: None,
                });
            }
        }
    }
    out.sort_by_key(|g| std::cmp::Reverse(g.size));
    Ok(out)
}

fn optional_metadata(path: &std::path::Path) -> Result<Option<std::fs::Metadata>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("cannot inspect {}", path.display())),
    }
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
) -> Result<()> {
    let mut row = GroupRow {
        name: profile.name.into(),
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
        )?;
    }
    if row.size > 0 || row.growth != 0 {
        groups.push(row);
    }
    Ok(())
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
) -> Result<()> {
    let mut row = GroupRow {
        name: def.name.into(),
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
    )?;
    if row.size > 0 || row.growth != 0 {
        groups.push(row);
    }
    Ok(())
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
) -> Result<()> {
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
        )?
        else {
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
                target.growth = target.growth.saturating_sub(prev.growth);
            }
        }
        if covered_by_existing || (target.size <= 0 && target.growth == 0) {
            continue;
        }
        row.size = row.size.saturating_add(target.size);
        row.growth = row.growth.saturating_add(target.growth);
        row.targets.push(GroupTarget {
            path: target.path.clone(),
        });
        assigned.push(target);
    }
    Ok(())
}

fn resolve_group_target(
    store: &Store,
    root_cmp_path: &str,
    root_dev: i64,
    root_inode: i64,
    target_path: &str,
    growth_map: &std::collections::HashMap<(i64, i64), i64>,
) -> Result<Option<ResolvedTarget>> {
    let target_cmp_path = canonical_path_string(target_path);
    // If the TUI is already scoped inside a known category, charge the whole
    // scoped tree to that category without resolving the ancestor directory.
    if path_contains(&target_cmp_path, root_cmp_path) {
        let Some((size, _)) = node_totals(store, root_dev, root_inode)? else {
            return Ok(None);
        };
        return Ok(Some(ResolvedTarget {
            path: target_path.to_string(),
            cmp_path: target_cmp_path,
            dev: root_dev,
            inode: root_inode,
            size,
            growth: growth_map
                .get(&(root_dev, root_inode))
                .copied()
                .unwrap_or(0),
        }));
    }
    if !path_contains(root_cmp_path, &target_cmp_path) {
        return Ok(None);
    }
    let Some((dev, inode)) =
        crate::store::indexed_path(&store.conn, std::path::Path::new(target_path))?
    else {
        return Ok(None);
    };
    let Some((size, _)) = node_totals(store, dev, inode)? else {
        return Ok(None);
    };
    Ok(Some(ResolvedTarget {
        path: target_path.to_string(),
        cmp_path: target_cmp_path,
        dev,
        inode,
        size,
        growth: growth_map.get(&(dev, inode)).copied().unwrap_or(0),
    }))
}

fn canonical_path_string(path: &str) -> String {
    // Lexical normalization only: grouping must never trigger mount I/O. Common
    // merged-/usr aliases are resolved by their explicit profile targets.
    let mut normalized = std::path::PathBuf::new();
    for part in std::path::Path::new(path).components() {
        match part {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            part => normalized.push(part.as_os_str()),
        }
    }
    normalized.to_string_lossy().into_owned()
}

fn node_totals(store: &Store, dev: i64, inode: i64) -> Result<Option<(i64, i64)>> {
    Ok(store
        .conn
        .query_row(
            "SELECT CASE WHEN kind='d' THEN recursive_bytes ELSE blocks END,
                    recursive_inodes
             FROM inodes WHERE dev_id=?1 AND inode=?2",
            params![dev, inode],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
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
fn event_loop<B: Backend>(
    term: &mut Terminal<B>,
    app: &mut App,
    snap_tx: &crossbeam_channel::Sender<Snapshot>,
    queued: &crossbeam_channel::Receiver<Snapshot>,
    res_rx: &crossbeam_channel::Receiver<RefreshMessage>,
    health_rx: &crossbeam_channel::Receiver<HealthUpdate>,
    storage_rx: &crossbeam_channel::Receiver<std::result::Result<StorageUpdate, String>>,
) -> Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    request_refresh(app, snap_tx, queued);
    let mut last_input = Instant::now() - Duration::from_secs(10);
    let mut last_request = Instant::now();
    let mut needs_draw = true;
    let mut last_clock_draw = Instant::now();
    loop {
        // Poll and acknowledge input before consuming refresh-worker results or
        // drawing background updates. Those operations can take noticeable time
        // on a large index, so putting them first made a queued key wait behind
        // otherwise useful but non-interactive work.
        if event::poll(Duration::from_millis(16))? {
            loop {
                if let Event::Key(k) = event::read()? {
                    if k.kind == KeyEventKind::Press {
                        if is_interrupt_key(k.code, k.modifiers) {
                            return Ok(());
                        } else if matches!(k.code, KeyCode::Char('R') | KeyCode::F(5)) {
                            app.view_gen += 1;
                            request_refresh(app, snap_tx, queued);
                            last_request = Instant::now();
                        } else {
                            let before = (
                                app.view_gen,
                                app.window_secs,
                                app.distribution,
                                app.group_view,
                                app.screen,
                                app.large_page_offset,
                                app.large_sort,
                            );
                            if handle_key(app, k.code)? {
                                return Ok(());
                            }
                            if before
                                != (
                                    app.view_gen,
                                    app.window_secs,
                                    app.distribution,
                                    app.group_view,
                                    app.screen,
                                    app.large_page_offset,
                                    app.large_sort,
                                )
                            {
                                app.view_gen += 1;
                                request_refresh(app, snap_tx, queued);
                                last_request = Instant::now();
                            }
                        }
                    }
                }
                if !event::poll(Duration::from_millis(0))? {
                    break;
                }
            }
            last_input = Instant::now();
            // Render the acknowledgement immediately. Defer worker updates to
            // the next iteration so a large result cannot delay visible input.
            term.draw(|f| draw(f, app))?;
            needs_draw = false;
            last_clock_draw = Instant::now();
            continue;
        }

        while let Ok(update) = health_rx.try_recv() {
            app.health_error = update.error;
            if app.health_error.is_some() {
                needs_draw = true;
                continue;
            }
            app.daemon_live = update.live;
            app.dirty_since = update.dirty;
            app.paused_since = update.paused;
            app.throttled_since = update.behind;
            app.pause_reason = update.reason;
            app.watch = update.watch;
            app.scan = update.scan;
            needs_draw = true;
        }
        while let Ok(result) = storage_rx.try_recv() {
            match result {
                Ok(data) => {
                    app.fs = data.fs;
                    app.containers = data.containers;
                    app.waste = data.waste;
                    app.cleanup = data.cleanup;
                    app.last_storage = Some(Instant::now());
                    app.storage_error = None;
                }
                Err(error) => app.storage_error = Some(error),
            }
            needs_draw = true;
        }
        while let Ok(result) = res_rx.try_recv() {
            match result {
                Ok(result) => {
                    app.apply_refresh(result);
                    app.update_detail();
                }
                Err(error) => {
                    app.refresh_error = Some(error);
                }
            }
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

        // Idle: ask the worker to recompute live data periodically. The recompute
        // happens off this thread, so the next keypress is never delayed by it.
        // ~4s cadence (not 1.2s): on a high-churn index the recompute is not free,
        // and the worker would otherwise be busy continuously, burning a core.
        if last_input.elapsed() >= Duration::from_millis(250)
            && last_request.elapsed() >= Duration::from_secs(4)
        {
            request_refresh(app, snap_tx, queued);
            last_request = Instant::now();
        }
    }
}

fn is_interrupt_key(code: KeyCode, modifiers: KeyModifiers) -> bool {
    code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL)
}

/// Handle one keypress. Returns Ok(true) to quit.
fn handle_key(app: &mut App, code: KeyCode) -> Result<bool> {
    // Global keys
    match code {
        KeyCode::Char('q') => return Ok(true),
        KeyCode::Char('?') => {
            app.show_help = !app.show_help;
            return Ok(false);
        }
        KeyCode::Esc if app.show_help => {
            app.show_help = false;
            return Ok(false);
        }
        KeyCode::Char('1') | KeyCode::Char('o') => app.screen = Screen::Overview,
        KeyCode::Char('2') | KeyCode::Char('e') => {
            app.screen = Screen::Explore;
            app.explore_distribution = false;
            app.update_detail();
            return Ok(false);
        }
        KeyCode::Char('3') | KeyCode::Char('a') => {
            app.screen = Screen::Activity;
            return Ok(false);
        }
        KeyCode::Char('4') | KeyCode::Char('r') | KeyCode::Char('c') => {
            app.screen = Screen::Reclaim;
            return Ok(false);
        }
        KeyCode::Char('5') | KeyCode::Char('f') => {
            app.screen = Screen::LargeFiles;
            app.large_page_offset = 0;
            app.large_selection = 0;
        }
        KeyCode::Esc if app.screen != Screen::Overview => {
            app.screen = Screen::Overview;
            app.update_detail();
            return Ok(false);
        }
        KeyCode::Tab => {
            if app.screen != Screen::Explore || app.explore_distribution {
                return Ok(false);
            }
            app.focus = match app.focus {
                Focus::Tree => Focus::Groups,
                Focus::Groups => Focus::Growth,
                Focus::Growth => Focus::Files,
                Focus::Files => Focus::Tree,
            };
            app.update_detail();
            return Ok(false);
        }
        _ => {}
    }

    if app.screen == Screen::Explore && app.explore_distribution {
        match code {
            KeyCode::Char('d') => {
                app.explore_distribution = false;
                app.focus = Focus::Tree;
            }
            KeyCode::Char('[') => {
                app.distribution = app.distribution.previous();
                app.group_view = GroupView::Top;
                app.asel = 0;
                app.groups.clear();
            }
            KeyCode::Char(']') => {
                app.distribution = app.distribution.next();
                app.group_view = GroupView::Top;
                app.asel = 0;
                app.groups.clear();
            }
            KeyCode::Down | KeyCode::Char('j') if app.asel + 1 < app.groups.len() => app.asel += 1,
            KeyCode::Up | KeyCode::Char('k') => app.asel = app.asel.saturating_sub(1),
            KeyCode::Home | KeyCode::Char('g') => app.asel = 0,
            KeyCode::End | KeyCode::Char('G') => app.asel = app.groups.len().saturating_sub(1),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if let Some(id) = app.groups.get(app.asel).and_then(|g| g.drill) {
                    app.group_view = GroupView::Detail(id);
                    app.asel = 0;
                    app.view_gen += 1;
                    app.groups.clear();
                }
            }
            KeyCode::Left | KeyCode::Char('h') if app.group_view != GroupView::Top => {
                app.group_view = GroupView::Top;
                app.asel = 0;
                app.view_gen += 1;
                app.groups.clear();
            }
            _ => {}
        }
        app.update_detail();
        return Ok(false);
    }
    if app.screen == Screen::Activity {
        match code {
            KeyCode::Down | KeyCode::Char('j')
                if app.activity_sel + 1 < app.recent_changes.len() =>
            {
                app.activity_sel += 1
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.activity_sel = app.activity_sel.saturating_sub(1)
            }
            KeyCode::Home | KeyCode::Char('g') => app.activity_sel = 0,
            KeyCode::End | KeyCode::Char('G') => {
                app.activity_sel = app.recent_changes.len().saturating_sub(1)
            }
            KeyCode::Char('[') => {
                app.window_secs = match app.window_secs {
                    0..=3600 => 3600,
                    3601..=86_400 => 3600,
                    _ => 86_400,
                };
            }
            KeyCode::Char(']') => {
                app.window_secs = match app.window_secs {
                    0..=3600 => 86_400,
                    3601..=86_400 => 604_800,
                    _ => 604_800,
                };
            }
            _ => {}
        }
        return Ok(false);
    }
    if app.screen == Screen::LargeFiles {
        match code {
            KeyCode::Down | KeyCode::Char('j')
                if app.large_selection + 1 < app.large_files.len() =>
            {
                app.large_selection += 1;
            }
            KeyCode::Up | KeyCode::Char('k') if app.large_selection > 0 => {
                app.large_selection -= 1;
            }
            KeyCode::PageDown | KeyCode::Right
                if app.large_page_offset + app.large_files.len() < app.large_total_count =>
            {
                app.large_page_offset = app.large_page_offset.saturating_add(LARGE_PAGE_SIZE);
                app.large_selection = 0;
            }
            KeyCode::PageUp | KeyCode::Left if app.large_page_offset > 0 => {
                app.large_page_offset = app.large_page_offset.saturating_sub(LARGE_PAGE_SIZE);
                app.large_selection = LARGE_PAGE_SIZE.saturating_sub(1);
            }
            KeyCode::Home | KeyCode::Char('g') => {
                app.large_page_offset = 0;
                app.large_selection = 0;
            }
            KeyCode::Char('[') => {
                app.window_secs = 3600;
            }
            KeyCode::Char(']') => {
                app.window_secs = match app.window_secs {
                    0..=3600 => 86_400,
                    3601..=86_400 => 604_800,
                    _ => 604_800,
                };
            }
            KeyCode::Char('s') => {
                app.large_sort = LargeSort::Size;
                app.large_page_offset = 0;
                app.large_selection = 0;
            }
            KeyCode::Char('G') => {
                app.large_sort = LargeSort::Growth;
                app.large_page_offset = 0;
                app.large_selection = 0;
            }
            _ => {}
        }
        app.update_detail();
        return Ok(false);
    }
    if app.screen == Screen::Reclaim {
        match code {
            KeyCode::Down | KeyCode::Char('j') if app.reclaim_sel + 1 < app.cleanup.len() => {
                app.reclaim_sel += 1
            }
            KeyCode::Up | KeyCode::Char('k') => app.reclaim_sel = app.reclaim_sel.saturating_sub(1),
            KeyCode::Home | KeyCode::Char('g') => app.reclaim_sel = 0,
            KeyCode::End | KeyCode::Char('G') => {
                app.reclaim_sel = app.cleanup.len().saturating_sub(1)
            }
            _ => {}
        }
        return Ok(false);
    }

    // Overview is intentionally non-interactive: it answers the common questions
    // at a glance. Navigation keys above move into the detailed screens.
    if app.screen == Screen::Overview {
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
                        app.view_gen += 1;
                        app.groups.clear();
                    }
                }
                KeyCode::Left | KeyCode::Esc | KeyCode::Char('h')
                    if app.focus == Focus::Groups && app.group_view != GroupView::Top =>
                {
                    app.group_view = GroupView::Top;
                    app.asel = 0;
                    app.view_gen += 1;
                    app.groups.clear();
                }
                _ => {}
            }
            app.update_detail();
        }
        // ---- tree ----
        Focus::Tree => {
            match code {
                KeyCode::Down | KeyCode::Char('j') if app.sel + 1 < app.rows.len() => app.sel += 1,
                KeyCode::Up | KeyCode::Char('k') => app.sel = app.sel.saturating_sub(1),
                KeyCode::Enter | KeyCode::Char(' ') => app.toggle()?,
                KeyCode::Right | KeyCode::Char('l') => {
                    if let Some(r) = app.rows.get(app.sel) {
                        if r.kind == 'd' && !r.expanded {
                            app.pending_child = Some((r.dev, r.inode));
                            app.expanded.insert((r.dev, r.inode));
                            app.view_gen += 1;
                            app.collapse_cached();
                        } else if app.sel + 1 < app.rows.len() {
                            app.sel += 1;
                        }
                    }
                }
                KeyCode::Left | KeyCode::Char('h') => match app.rows.get(app.sel) {
                    Some(r) if r.expanded => {
                        let id = (r.dev, r.inode);
                        app.expanded.remove(&id);
                        app.view_gen += 1;
                        app.collapse_cached();
                    }
                    Some(_) => app.ascend()?,
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
                    app.collapse_cached();
                }
                KeyCode::Char('d') => {
                    app.explore_distribution = true;
                    app.focus = Focus::Groups;
                }
                KeyCode::Home | KeyCode::Char('g') => app.sel = 0,
                KeyCode::End | KeyCode::Char('G') => app.sel = app.rows.len().saturating_sub(1),
                _ => {}
            }
            app.update_detail();
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

/// Signed allocation change for a selected history window (not a rate).
fn growth_delta(delta: i64) -> String {
    match delta.cmp(&0) {
        std::cmp::Ordering::Greater => format!("+{}", human(delta)),
        std::cmp::Ordering::Less => format!("-{}", human(delta.saturating_abs())),
        std::cmp::Ordering::Equal => "0".to_owned(),
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
        app.groups
            .iter()
            .map(|g| g.size.max(0))
            .fold(0i64, i64::saturating_add)
    }
}

fn freshness_text(app: &App) -> String {
    if let Some(error) = &app.health_error {
        return format!("index health unavailable · {}", short(error, 90));
    }
    if let Some(error) = &app.refresh_error {
        return format!("refresh failed · {} · retrying", short(error, 100));
    }
    if app.watch.as_ref().is_some_and(|watch| watch.uncertain) && app.scan.is_none() {
        return "DIRTY · incomplete index · reconciliation required".into();
    }
    if let Some(p) = &app.scan {
        format!(
            "reconciling · displayed data may be stale · {} files · {} dirs · {} · {}s",
            count_human(p.files.min(i64::MAX as u64) as i64),
            count_human(p.dirs.min(i64::MAX as u64) as i64),
            human(p.bytes),
            crate::util::now_secs().saturating_sub(p.started).max(0)
        )
    } else if app.last_refresh.is_none() {
        "loading indexed data…".into()
    } else if app
        .last_refresh
        .is_some_and(|t| t.elapsed() >= Duration::from_secs(15))
    {
        format!(
            "view refresh delayed · last success {}s ago",
            app.last_refresh
                .map(|time| time.elapsed().as_secs())
                .unwrap_or(0)
        )
    } else if let Some(since) = app.dirty_since {
        format!("DIRTY {} · reconciliation required", ago(since))
    } else if let Some(since) = app.paused_since {
        format!("writes paused {} · {}", ago(since), app.pause_reason)
    } else if let Some(since) = app.throttled_since {
        format!("backlog · {} since last caught up", ago(since))
    } else if app.daemon_live {
        app.watch
            .as_ref()
            .map(watch_status_label)
            .unwrap_or_else(|| "live · watcher status unavailable".into())
    } else {
        format!("snapshot · {} old · daemon off", ago(app.last_scan))
    }
}

fn watch_status_label(w: &crate::util::WatchStatus) -> String {
    if w.scan_first {
        return if w.uncertain {
            match w.kernel_overflow_notifications {
                Some(overflows) if overflows > 0 => format!(
                    "scan-first · stale observation · {} kernel overflows",
                    count_human(overflows as i64)
                ),
                _ => "scan-first · stale observation · reconciliation scheduled".into(),
            }
        } else {
            "scan-first · last observation verified".into()
        };
    }
    if w.uncertain {
        match w.kernel_overflow_notifications {
            Some(overflows) if overflows > 0 => format!(
                "live · {} kernel queue overflows · reconciliation needed",
                count_human(overflows as i64)
            ),
            None => {
                "live · event state uncertain · overflow count unavailable · reconciliation needed"
                    .into()
            }
            _ => "live · event state uncertain · reconciliation needed".into(),
        }
    } else if w.pending == 0 && w.kernel_empty {
        let count_note = if w.kernel_overflow_notifications.is_none() {
            " · overflow count unavailable"
        } else {
            ""
        };
        format!(
            "live · caught up · {} updates{}",
            count_human(w.updates_committed as i64),
            count_note
        )
    } else {
        let count_note = if w.kernel_overflow_notifications.is_none() {
            " · overflow count unavailable"
        } else {
            ""
        };
        format!(
            "live · queue {}/{} · {}ms lag{}",
            w.pending, w.max_pending, w.behind_ms, count_note
        )
    }
}

fn draw_nav(f: &mut Frame, area: Rect, app: &App, title: &str) {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(4)])
        .split(area);
    let show_root = area.width >= 110;
    let status_width = if show_root { 64 } else { 42 };
    let mut brand = vec![
        Span::styled(
            " dux ",
            Style::default()
                .fg(Color::Black)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {title}"),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled("     ", Style::default()),
        Span::styled(
            short(&freshness_text(app), status_width),
            Style::default().fg(if app.dirty_since.is_some() {
                CRIT_COLOR
            } else {
                Color::Gray
            }),
        ),
    ];
    if show_root {
        brand.push(Span::styled(
            "     Root  ",
            Style::default().fg(Color::DarkGray),
        ));
        brand.push(Span::styled(
            short(&app.root_display_path, 20),
            Style::default().fg(Color::Gray),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(brand)), sections[0]);

    // Keep the five choices visually grouped instead of stretching five giant
    // empty tiles across an ultrawide terminal.
    let nav_width = sections[1].width.min(144);
    let nav_area = Rect {
        x: sections[1].x + sections[1].width.saturating_sub(nav_width) / 2,
        y: sections[1].y,
        width: nav_width,
        height: sections[1].height,
    };
    let tabs = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Ratio(1, 5),
            Constraint::Ratio(1, 5),
            Constraint::Ratio(1, 5),
            Constraint::Ratio(1, 5),
            Constraint::Ratio(1, 5),
        ])
        .split(nav_area);
    let choices = [
        (Screen::Overview, "1", "OVERVIEW", "Disk health"),
        (Screen::Explore, "2", "EXPLORE", "Where space is"),
        (Screen::Activity, "3", "ACTIVITY", "What changed"),
        (Screen::Reclaim, "4", "RECLAIM", "Free safely"),
        (Screen::LargeFiles, "5", "LARGE FILES", "Size and growth"),
    ];
    for (idx, (screen, number, name, purpose)) in choices.into_iter().enumerate() {
        let selected = app.screen == screen;
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if selected {
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            })
            .title(Span::styled(
                format!(" {number} "),
                if selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(ACCENT)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                },
            ));
        let lines = vec![
            Line::from(Span::styled(
                name,
                if selected {
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(Color::Gray)
                        .add_modifier(Modifier::BOLD)
                },
            )),
            Line::from(Span::styled(purpose, Style::default().fg(Color::DarkGray))),
        ];
        let mut paragraph = Paragraph::new(lines)
            .alignment(Alignment::Center)
            .block(block);
        if selected {
            paragraph = paragraph.style(Style::default().bg(Color::Rgb(22, 30, 45)));
        }
        f.render_widget(paragraph, tabs[idx]);
    }
}

fn ranked_card(title: String, lines: Vec<Line<'static>>) -> Paragraph<'static> {
    Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(Span::styled(
                format!(" {title} "),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )),
    )
}

fn draw_overview(f: &mut Frame, app: &App) {
    // On narrow terminals stack the answer cards; give each enough height for
    // borders + value + explanation. Wide terminals keep the compact row.
    let compact = f.area().width < 90;
    let card_height = if compact { 15 } else { 9 };
    let disk_height = if compact { 3 } else { 4 };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(disk_height),
            Constraint::Length(card_height),
            if compact {
                Constraint::Length(0)
            } else {
                Constraint::Min(4)
            },
            Constraint::Length(1),
        ])
        .split(f.area());
    draw_nav(f, rows[0], app, "Overview");

    let pct = app.fs.use_pct();
    let gauge = bar((pct / 100.0).clamp(0.0, 1.0), 24);
    let color = if pct >= 95.0 { CRIT_COLOR } else { SIZE_COLOR };
    f.render_widget(
        Paragraph::new(vec![
            Line::from(vec![Span::styled(
                format!(" {:>3.0}%  {gauge}", pct),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )]),
            Line::from(vec![
                Span::styled(" Used  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    human(app.fs.used),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" / {}", human(app.fs.total)),
                    Style::default().fg(Color::Gray),
                ),
                Span::styled("     Free  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    human(app.fs.avail),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(Span::styled(
                    " Disk health ",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                )),
        ),
        rows[1],
    );

    let cards = if compact {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(7), // borders + five largest files
                Constraint::Length(5), // borders + three growth rows
                Constraint::Min(3),    // borders + one reclaim summary row
            ])
            .split(rows[2])
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
            ])
            .split(rows[2])
    };
    let reclaimable: i64 = app
        .cleanup
        .iter()
        .map(|r| r.size.max(0))
        .fold(0i64, i64::saturating_add);
    let largest_width = cards[0].width.saturating_sub(38) as usize;
    let largest_lines: Vec<Line<'static>> = app
        .top_files
        .iter()
        .take(5)
        .enumerate()
        .map(|(idx, (path, size, _, _))| {
            let relation = crate::classify::classify_path(path, 'f').belongs_to;
            Line::from(vec![
                Span::styled(
                    format!(" {} ", idx + 1),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{} ", fixw(&human(*size), 10, true)),
                    Style::default().fg(SIZE_COLOR).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{} ", fixw(relation, 20, false)),
                    Style::default().fg(Color::Gray),
                ),
                Span::raw(short(path, largest_width)),
            ])
        })
        .collect();
    f.render_widget(
        ranked_card(
            "Top 5 largest · size │ belongs to │ path".into(),
            if largest_lines.is_empty() {
                vec![Line::from(" index is warming up")]
            } else {
                largest_lines
            },
        ),
        cards[0],
    );
    let growth_width = cards[1].width.saturating_sub(38) as usize;
    let growth_lines: Vec<Line<'static>> = app
        .top_growth
        .iter()
        .take(5)
        .enumerate()
        .map(|(idx, (path, delta))| {
            let relation = crate::classify::classify_path(path, 'f').belongs_to;
            Line::from(vec![
                Span::styled(
                    format!(" {} ", idx + 1),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{} ", fixw(&rate_str(*delta), 10, false)),
                    Style::default().fg(RATE_COLOR).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{} ", fixw(relation, 20, false)),
                    Style::default().fg(Color::Gray),
                ),
                Span::raw(short(path, growth_width)),
            ])
        })
        .collect();
    f.render_widget(
        ranked_card(
            "Top 5 growth · rate │ belongs to │ path".into(),
            if growth_lines.is_empty() {
                vec![Line::from(" no recent growth")]
            } else {
                growth_lines
            },
        ),
        cards[1],
    );
    let reclaim_width = cards[2].width.saturating_sub(27) as usize;
    let reclaim_lines: Vec<Line<'static>> = app
        .cleanup
        .iter()
        .take(5)
        .enumerate()
        .map(|(idx, row)| {
            Line::from(vec![
                Span::styled(
                    format!(" {} ", idx + 1),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{} ", fixw(&human(row.size), 10, true)),
                    Style::default().fg(SIZE_COLOR).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{} ", fixw(row.safety, 10, false)),
                    Style::default().fg(Color::Gray),
                ),
                Span::raw(short(&row.name, reclaim_width)),
            ])
        })
        .collect();
    f.render_widget(
        ranked_card(
            format!(
                "Top 5 reclaim · total {} · size │ safety │ source",
                human(reclaimable)
            ),
            if reclaim_lines.is_empty() {
                vec![Line::from(" no measured candidates")]
            } else {
                reclaim_lines
            },
        ),
        cards[2],
    );

    let max = app
        .groups
        .iter()
        .map(|g| g.size.max(0))
        .max()
        .unwrap_or(1)
        .max(1);
    let inner = rows[3].height.saturating_sub(2) as usize;
    let lines: Vec<Line> = app
        .groups
        .iter()
        .take(inner)
        .map(|g| {
            Line::from(vec![
                Span::styled(
                    format!(" {} ", fixw(&human(g.size.max(0)), 9, true)),
                    Style::default().fg(SIZE_COLOR).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{} ", bar(g.size.max(0) as f64 / max as f64, 18)),
                    Style::default().fg(SIZE_COLOR),
                ),
                Span::styled(
                    format!("{} ", fixw(&rate_str(g.growth), 11, false)),
                    Style::default().fg(RATE_COLOR),
                ),
                Span::raw(&g.name),
            ])
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(Span::styled(
                    format!(" Where space is used — {} ", app.distribution.label()),
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                )),
        ),
        rows[3],
    );
    let footer = if f.area().width < 100 {
        " 2 Explore · 3 Activity · 4 Reclaim · ? Help · q Quit"
    } else {
        " 2/e explore · 3/a activity · 4/r reclaim · F5/R refresh · ? help · q/Ctrl-C quit"
    };
    f.render_widget(
        Paragraph::new(Span::styled(footer, Style::default().fg(Color::DarkGray))),
        rows[4],
    );
}

fn window_label(secs: i64) -> &'static str {
    if secs <= 3600 {
        "1 hour"
    } else if secs <= 86_400 {
        "24 hours"
    } else {
        "7 days"
    }
}

fn draw_activity(f: &mut Frame, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(2),
            Constraint::Min(8),
            Constraint::Length(4),
            Constraint::Length(1),
        ])
        .split(f.area());
    draw_nav(f, rows[0], app, "Activity");
    let grown = app
        .recent_changes
        .iter()
        .filter(|x| x.1 > 0)
        .fold(0i64, |a, x| a.saturating_add(x.1));
    let freed = app
        .recent_changes
        .iter()
        .filter(|x| x.1 < 0)
        .fold(0i64, |a, x| a.saturating_add(x.1.saturating_abs()));
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" Window {} ", window_label(app.window_secs)),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "grown {} · freed {} · net {} · {} changed paths",
                human(grown),
                human(freed),
                rate_str(grown.saturating_sub(freed)),
                app.recent_changes.len()
            )),
        ])),
        rows[1],
    );
    let panels = Layout::default()
        .direction(if f.area().width >= 95 {
            Direction::Horizontal
        } else {
            Direction::Vertical
        })
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[2]);
    let visible = panels[0].height.saturating_sub(2) as usize;
    let start = app.activity_sel.saturating_sub(visible.saturating_sub(1));
    let changes: Vec<Line> = app
        .recent_changes
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(idx, (p, d))| {
            let mut line = Line::from(vec![
                Span::styled(
                    format!(" {} ", fixw(&rate_str(*d), 12, false)),
                    Style::default().fg(RATE_COLOR).add_modifier(Modifier::BOLD),
                ),
                Span::raw(short(p, panels[0].width.saturating_sub(18) as usize)),
            ]);
            if idx == app.activity_sel {
                line = line.style(Style::default().bg(Color::Rgb(38, 44, 66)));
            }
            line
        })
        .collect();
    f.render_widget(
        Paragraph::new(if changes.is_empty() {
            vec![Line::from("  No indexed changes in this window.")]
        } else {
            changes
        })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(Span::styled(
                    " Biggest changes — fills and frees ",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                )),
        ),
        panels[0],
    );
    let hot: Vec<Line> = app
        .top_growth
        .iter()
        .take(visible)
        .map(|(p, d)| {
            Line::from(vec![
                Span::styled(
                    format!(" {} ", fixw(&rate_str(*d), 12, false)),
                    Style::default().fg(RATE_COLOR).add_modifier(Modifier::BOLD),
                ),
                Span::raw(short(p, panels[1].width.saturating_sub(18) as usize)),
            ])
        })
        .collect();
    f.render_widget(
        Paragraph::new(if hot.is_empty() {
            vec![Line::from(
                "  No growing paths yet; keep the daemon running.",
            )]
        } else {
            hot
        })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(Span::styled(
                    " Growing now ",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                )),
        ),
        panels[1],
    );
    let selected = app
        .recent_changes
        .get(app.activity_sel)
        .map(|x| format!("{} · {}", rate_str(x.1), x.0))
        .unwrap_or_else(|| "No selection".into());
    f.render_widget(
        Paragraph::new(selected).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(" Selected path "),
        ),
        rows[3],
    );
    f.render_widget(
        Paragraph::new(Span::styled(
            " [ / ] window (1h, 24h, 7d) · ↑↓ select · 1 overview · 2 explore · 4 reclaim · ? help",
            Style::default().fg(Color::DarkGray),
        )),
        rows[4],
    );
}

fn draw_apps(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(1),
            Constraint::Min(8),
            Constraint::Length(5),
            Constraint::Length(1),
        ])
        .split(area);

    let title = match app.group_view {
        GroupView::Top => app.distribution.label(),
        GroupView::Detail(id) => TOP_PROFILES
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.name)
            .unwrap_or("Details"),
    };
    draw_nav(f, rows[0], app, &format!("Explore / {title}"));
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Size       Share  Distribution                         Growth       Name",
            Style::default().fg(Color::DarkGray),
        ))),
        rows[1],
    );

    let total = group_view_total(app).max(1);
    let max_group = app
        .groups
        .iter()
        .map(|g| g.size.max(0))
        .max()
        .unwrap_or(1)
        .max(1);
    let body_h = rows[2].height.saturating_sub(2) as usize;
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
        rows[2],
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
            rows[3].width.saturating_sub(4) as usize,
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
        rows[3],
    );

    let legend = if app.group_view == GroupView::Top {
        " [/] grouping · d tree · ↑↓ move · Enter/→ drill · 1 overview · 3 activity · 4 reclaim · q quit"
    } else {
        " d tree · ← back · ↑↓ move · 1 overview · 3 activity · 4 reclaim · q quit"
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            legend,
            Style::default().fg(Color::DarkGray),
        ))),
        rows[4],
    );
}

fn draw_storage(f: &mut Frame, app: &App) {
    let area = f.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(1),
            Constraint::Percentage(45),
            Constraint::Min(6),
            Constraint::Length(4),
            Constraint::Length(1),
        ])
        .split(area);
    let storage_title = match (&app.storage_error, app.last_storage) {
        (Some(error), _) => format!("Reclaim · refresh failed: {}", short(error, 80)),
        (_, Some(time)) => format!(
            "Reclaim · storage evidence {}s old",
            time.elapsed().as_secs()
        ),
        _ => "Reclaim · loading storage evidence…".into(),
    };
    draw_nav(f, rows[0], app, &storage_title);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(
                    " Measured Docker reclaimable {} · ",
                    human(app.waste.total())
                ),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "advice only — dux never deletes anything",
                Style::default().fg(Color::DarkGray),
            ),
        ])),
        rows[1],
    );

    let cmax = app
        .containers
        .iter()
        .map(|c| c.total())
        .max()
        .unwrap_or(1)
        .max(1);
    let cheight = rows[2].height.saturating_sub(2) as usize;
    let clines: Vec<Line> = if app.containers.is_empty() {
        vec![Line::from(Span::styled(
            "  No Docker/Podman metadata found (or storage paths are not readable/indexed).",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        app.containers
            .iter()
            .take(cheight)
            .map(|c| {
                Line::from(vec![
                    Span::styled(
                        format!(" {} ", fixw(&human(c.total()), 9, true)),
                        Style::default().fg(SIZE_COLOR).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("{} ", bar(c.total() as f64 / cmax as f64, 14)),
                        Style::default().fg(SIZE_COLOR),
                    ),
                    Span::styled(
                        format!("{:<8} ", c.state()),
                        Style::default().fg(if c.running { Color::Gray } else { RATE_COLOR }),
                    ),
                    Span::raw(format!(
                        "W {}  L {}  V {}  reclaim {}  {} ({})",
                        human(c.writable_bytes),
                        human(c.log_bytes),
                        human(c.volume_bytes),
                        human(c.reclaimable()),
                        short(&c.name, 24),
                        short(&c.image, 24),
                    )),
                ])
            })
            .collect()
    };
    f.render_widget(
        Paragraph::new(clines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(Span::styled(
                    " Docker/Podman — writable · logs · volumes ",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                )),
        ),
        rows[2],
    );

    let rheight = rows[3].height.saturating_sub(2) as usize;
    let suggestions: Vec<Line> = if app.cleanup.is_empty() {
        vec![Line::from(Span::styled(
            "  No indexed cleanup candidates found.",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        app.cleanup
            .iter()
            .enumerate()
            .take(rheight)
            .map(|(idx, r)| {
                let mut line = Line::from(vec![
                    Span::styled(
                        format!(" {} ", fixw(&human(r.size), 9, true)),
                        Style::default().fg(SIZE_COLOR).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("{:<9} ", r.safety),
                        Style::default().fg(Color::Gray),
                    ),
                    Span::styled(
                        format!("{:<31} ", short(&r.name, 30)),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(short(&r.action, rows[3].width.saturating_sub(56) as usize)),
                ]);
                if idx == app.reclaim_sel {
                    line = line.style(Style::default().bg(Color::Rgb(38, 44, 66)));
                }
                line
            })
            .collect()
    };
    f.render_widget(
        Paragraph::new(suggestions).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(Span::styled(
                    " Cleanup candidates — measured size · safety · recommended action ",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                )),
        ),
        rows[3],
    );
    let evidence = app
        .cleanup
        .get(app.reclaim_sel)
        .map(|r| {
            vec![
                Line::from(vec![
                    Span::styled(
                        format!("{} · {} · ", human(r.size), r.safety),
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(&r.name),
                ]),
                Line::from(vec![
                    Span::styled("Suggested: ", Style::default().fg(Color::DarkGray)),
                    Span::raw(&r.action),
                ]),
            ]
        })
        .unwrap_or_else(|| vec![Line::from("No cleanup candidate selected.")]);
    f.render_widget(
        Paragraph::new(evidence).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(" Evidence & suggested command (not executed) "),
        ),
        rows[4],
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " ↑↓ inspect candidate · 1 overview · 2 explore · 3 activity · ? help · q/Ctrl-C quit",
            Style::default().fg(Color::DarkGray),
        ))),
        rows[5],
    );
}

fn draw_explore(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5), // prominent navigation tiles + live state
            Constraint::Length(1), // capacity gauge
            Constraint::Min(3),    // tree (primary view, on top)
            Constraint::Length(8), // panels (growth | largest), below
            Constraint::Length(1), // footer
        ])
        .split(area);

    draw_nav(f, rows[0], app, "Explore / Tree");

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
        label("Queue"),
        Span::styled(
            app.watch
                .as_ref()
                .map(|w| {
                    if w.scan_first {
                        format!("{} hints · scans", count_human(w.events_seen as i64))
                    } else {
                        format!(
                            "{} ({:.0}% resolved)",
                            count_human(w.pending as i64),
                            if w.events_seen == 0 {
                                100.0
                            } else {
                                w.events_resolved as f64 * 100.0 / w.events_seen as f64
                            }
                        )
                    }
                })
                .unwrap_or_else(|| "—".into()),
            Style::default(),
        ),
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
                Span::raw(short(&g.name, 18)),
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
    let legend =
        " d distribution · Tab section · ↑↓ move · →/⏎ expand · i size⇄inodes · 1/3/4 screens · q quit │ ";
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

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

/// A true area-proportional treemap for the current ranked page. Keeping the
/// treemap page-local makes size semantics honest and avoids loading a
/// multi-million-file index into UI memory; all files remain reachable through
/// deterministic 100-row pages.
fn split_treemap(weighted: &[(usize, i64)], area: Rect, output: &mut Vec<(usize, Rect)>) {
    if weighted.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }
    if weighted.len() == 1 {
        output.push((weighted[0].0, area));
        return;
    }
    if area.width == 1 && area.height == 1 {
        // There is one terminal cell for several sub-cell entries; keep the
        // largest ranked entry and leave the rest discoverable in the table.
        output.push((weighted[0].0, area));
        return;
    }
    let total: i128 = weighted
        .iter()
        .map(|(_, size)| i128::from((*size).max(1)))
        .sum();
    let mut accumulated = 0i128;
    let mut split = 1usize;
    for (index, (_, size)) in weighted.iter().enumerate().take(weighted.len() - 1) {
        accumulated += i128::from((*size).max(1));
        split = index + 1;
        if accumulated * 2 >= total {
            break;
        }
    }
    let first_total: i128 = weighted[..split]
        .iter()
        .map(|(_, size)| i128::from((*size).max(1)))
        .sum();
    let vertical = area.width >= area.height;
    let extent = if vertical { area.width } else { area.height };
    if extent <= 1 {
        output.push((weighted[0].0, area));
        return;
    }
    let first_extent = ((i128::from(extent) * first_total + total / 2) / total)
        .clamp(1, i128::from(extent - 1)) as u16;
    let (first, second) = if vertical {
        (
            Rect {
                width: first_extent,
                ..area
            },
            Rect {
                x: area.x + first_extent,
                width: area.width - first_extent,
                ..area
            },
        )
    } else {
        (
            Rect {
                height: first_extent,
                ..area
            },
            Rect {
                y: area.y + first_extent,
                height: area.height - first_extent,
                ..area
            },
        )
    };
    split_treemap(&weighted[..split], first, output);
    split_treemap(&weighted[split..], second, output);
}

fn file_tile_color(row: &LargeFileRow, selected: bool) -> Color {
    if selected {
        return Color::Rgb(28, 105, 142);
    }
    let extension = std::path::Path::new(&row.path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let palette = [
        Color::Rgb(49, 89, 134),
        Color::Rgb(47, 112, 104),
        Color::Rgb(117, 83, 135),
        Color::Rgb(147, 90, 51),
        Color::Rgb(105, 112, 54),
        Color::Rgb(58, 105, 138),
        Color::Rgb(126, 72, 82),
        Color::Rgb(72, 103, 119),
    ];
    let slot = extension.bytes().fold(0usize, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte as usize)
    });
    palette[slot % palette.len()]
}

fn draw_large_files(f: &mut Frame, app: &App) {
    let compact = f.area().height < 29;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(2),
            Constraint::Min(if compact { 6 } else { 8 }),
            Constraint::Length(if compact { 8 } else { 11 }),
            Constraint::Length(1),
        ])
        .split(f.area());
    draw_nav(f, rows[0], app, "Large Files / Treemap");

    let page = app.large_page_offset / LARGE_PAGE_SIZE + 1;
    let pages = app.large_total_count.div_ceil(LARGE_PAGE_SIZE).max(1);
    let ranking = match app.large_sort {
        LargeSort::Size => "largest allocated size",
        LargeSort::Growth => "largest absolute change",
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {} files ", app.large_total_count),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "· {} allocated · page {page}/{pages} · {ranking} · {} window · ",
                human(app.large_total_bytes),
                window_label(app.window_secs)
            )),
            Span::styled(
                if app.daemon_live {
                    "live history"
                } else {
                    "history requires daemon"
                },
                Style::default().fg(if app.daemon_live {
                    RATE_COLOR
                } else {
                    Color::Yellow
                }),
            ),
        ])),
        rows[1],
    );

    let map_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(format!(
            " Disk map · area proportional within page {page} · ↑↓ select "
        ));
    let map_area = map_block.inner(rows[2]);
    f.render_widget(map_block, rows[2]);
    if app.large_files.is_empty() {
        f.render_widget(
            Paragraph::new("Index warming up, or no files in this scope."),
            map_area,
        );
    } else {
        let weighted: Vec<(usize, i64)> = app
            .large_files
            .iter()
            .enumerate()
            .map(|(index, row)| (index, row.allocated.max(1)))
            .collect();
        let mut tiles = Vec::with_capacity(weighted.len());
        split_treemap(&weighted, map_area, &mut tiles);
        for (index, area) in tiles {
            let row = &app.large_files[index];
            let style = Style::default()
                .fg(Color::White)
                .bg(file_tile_color(row, index == app.large_selection));
            let name = std::path::Path::new(&row.path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&row.path);
            let label = if area.width >= 18 {
                format!(
                    "{}\n{}",
                    short(name, area.width as usize - 1),
                    human(row.allocated)
                )
            } else if area.width >= 8 {
                short(name, area.width as usize)
            } else {
                String::new()
            };
            f.render_widget(Paragraph::new(label).style(style), area);
        }
    }

    let table_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(" Ranked files · allocated · growth in selected window · modified ");
    let table_area = table_block.inner(rows[3]);
    f.render_widget(table_block, rows[3]);
    let visible = table_area.height as usize;
    let body_rows = visible.saturating_sub(1);
    let start = app
        .large_selection
        .saturating_sub(body_rows.saturating_sub(1));
    let mut lines = vec![Line::from(vec![
        Span::styled("ALLOCATED ", Style::default().fg(SIZE_COLOR)),
        Span::styled("GROWTH ", Style::default().fg(RATE_COLOR)),
        Span::raw("AGE  PATH"),
    ])];
    for (index, row) in app
        .large_files
        .iter()
        .enumerate()
        .skip(start)
        .take(body_rows)
    {
        let growth = if app.daemon_live {
            growth_delta(row.growth)
        } else {
            "—".to_owned()
        };
        let line = Line::from(vec![
            Span::styled(
                format!("{} ", fixw(&human(row.allocated), 10, true)),
                Style::default().fg(SIZE_COLOR).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{} ", fixw(&growth, 11, false)),
                Style::default().fg(if row.growth < 0 {
                    Color::Green
                } else {
                    RATE_COLOR
                }),
            ),
            Span::styled(
                format!("{} ", fixw(&ago(row.modified), 5, true)),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(short(
                &row.path,
                table_area.width.saturating_sub(32) as usize,
            )),
        ]);
        lines.push(if index == app.large_selection {
            line.style(Style::default().bg(Color::Rgb(38, 44, 66)))
        } else {
            line
        });
    }
    if app.large_files.is_empty() {
        lines.push(Line::from("No matching indexed files."));
    }
    f.render_widget(Paragraph::new(lines), table_area);

    let selected = app.large_files.get(app.large_selection);
    let footer = selected.map_or_else(
        || "PgUp/PgDn page · [/] time window · s size rank · G growth rank".to_owned(),
        |row| {
            format!(
                "{} · {} allocated · {} growth / {} · inode {}:{} · PgUp/PgDn page",
                row.path,
                human(row.allocated),
                if app.daemon_live {
                    growth_delta(row.growth)
                } else {
                    "—".into()
                },
                window_label(app.window_secs),
                row.dev,
                row.inode
            )
        },
    );
    f.render_widget(
        Paragraph::new(Span::styled(
            short(&footer, f.area().width as usize),
            Style::default().fg(Color::DarkGray),
        )),
        rows[4],
    );
}

fn draw_help(f: &mut Frame) {
    let area = centered_rect(72, 62, f.area());
    f.render_widget(Clear, area);
    let help = vec![
        Line::from(Span::styled(
            "Four questions, four screens",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("1 / o  Overview     Is the disk healthy? What matters now?"),
        Line::from("2 / e  Explore      Where is space used? Expand the indexed tree."),
        Line::from("       d             Toggle Tree ↔ Apps/OS/Users distribution."),
        Line::from("3 / a  Activity     What changed? [ and ] select 1h / 24h / 7d."),
        Line::from("4 / r  Reclaim      What may be freed? Inspect evidence and safety."),
        Line::from("5 / f  Large files  Treemap + ranked files, size and growth window."),
        Line::from(""),
        Line::from("↑↓ / j k move · Enter/→ expand or drill · PgUp/PgDn page files"),
        Line::from("Large Files: [/] time · s size rank · G growth rank."),
        Line::from("Tab changes Explore section · i toggles size/inode heat"),
        Line::from("F5 or R refreshes from the index · q or Ctrl-C exits"),
        Line::from(""),
        Line::from(Span::styled(
            "dux only advises: it never runs cleanup commands.",
            Style::default().fg(Color::Gray),
        )),
    ];
    f.render_widget(
        Paragraph::new(help).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Double)
                .border_style(Style::default().fg(ACCENT))
                .title(" Help — press ? or Esc to close "),
        ),
        area,
    );
}

fn draw(f: &mut Frame, app: &mut App) {
    // Help is a modal screen. Avoid rebuilding the underlying dashboard on
    // every keypress while the modal is open; large indexes can otherwise
    // make the help acknowledgement miss the interactive latency budget.
    // Ratatui's diff renderer will repaint the dashboard when help closes.
    if app.show_help {
        draw_help(f);
        return;
    }

    match app.screen {
        Screen::Overview => draw_overview(f, app),
        Screen::Explore if app.explore_distribution => draw_apps(f, app),
        Screen::Explore => draw_explore(f, app),
        Screen::Activity => draw_activity(f, app),
        Screen::Reclaim => draw_storage(f, app),
        Screen::LargeFiles => draw_large_files(f, app),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{self, ScanOptions};

    #[test]
    fn indexed_path_size_distinguishes_absent_path_from_broken_inode_reference() {
        let store = Store {
            conn: rusqlite::Connection::open_in_memory().unwrap(),
            schema_version: crate::store::SCHEMA_VERSION,
        };
        store.conn.execute_batch(crate::store::SCHEMA_SQL).unwrap();
        store.set_meta("root_dev", "1").unwrap();
        store.set_meta("root_inode", "1").unwrap();
        store.set_meta("last_scan_root", "/").unwrap();
        store
            .conn
            .execute(
                "INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
                 VALUES(1,1,?1,1,99,1)",
                [b"broken".as_slice()],
            )
            .unwrap();

        assert_eq!(indexed_path_size(&store, "/not-indexed").unwrap(), 0);
        let error = indexed_path_size(&store, "/broken")
            .unwrap_err()
            .to_string();
        assert!(error.contains("references missing inode (1,99)"), "{error}");
    }

    #[test]
    fn invalid_root_metadata_does_not_fall_back_to_another_directory() {
        let store = Store {
            conn: rusqlite::Connection::open_in_memory().unwrap(),
            schema_version: crate::store::SCHEMA_VERSION,
        };
        store.conn.execute_batch(crate::store::SCHEMA_SQL).unwrap();
        store.set_meta("root_dev", "1").unwrap();
        store.set_meta("root_inode", "not-an-inode").unwrap();
        assert!(root_node(&store)
            .unwrap_err()
            .to_string()
            .contains("invalid root_inode metadata"));

        store.set_meta("last_scan_root", "/").unwrap();
        store
            .conn
            .execute(
                "INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,uid,mtime)
             VALUES(1,1,'d',4096,4096,0,0)",
                [],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
             VALUES(1,1,X'2f',1,1,1)",
                [],
            )
            .unwrap();
        let app = App::new(&store, std::path::Path::new("unused-index"), 1, 1).unwrap();
        assert!(app
            .panel_scope(&store)
            .unwrap_err()
            .to_string()
            .contains("invalid root_inode metadata"));
    }

    #[test]
    fn app_creation_propagates_unresolvable_indexed_root() {
        let store = Store {
            conn: rusqlite::Connection::open_in_memory().unwrap(),
            schema_version: crate::store::SCHEMA_VERSION,
        };
        store.conn.execute_batch(crate::store::SCHEMA_SQL).unwrap();
        store
            .conn
            .execute(
                "INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,uid,mtime)
             VALUES(1,7,'d',4096,4096,0,0)",
                [],
            )
            .unwrap();
        let error = App::new(&store, std::path::Path::new("unused-index"), 1, 7)
            .err()
            .expect("unresolvable root must fail App creation")
            .to_string();
        assert!(error.contains("has no visible path"), "{error}");
    }

    #[test]
    fn optional_metadata_suppresses_only_absence() {
        let path = tmp("metadata-not-directory");
        std::fs::write(&path, b"file").unwrap();
        assert!(optional_metadata(&path.join("child")).is_err());
        assert!(optional_metadata(&tmp("metadata-missing"))
            .unwrap()
            .is_none());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn long_running_scan_status_explains_indexed_view_may_be_stale() {
        let db = std::env::temp_dir().join(format!("dux-tui-freshness-{}", std::process::id()));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
        let store = Store::create_fresh(&db).unwrap();
        install_test_root(&store, "/");
        let mut app = App::new(&store, &db, 1, 1).unwrap();
        app.last_refresh = Some(Instant::now() - Duration::from_secs(60));
        app.scan = Some(crate::util::ScanProgress {
            started: crate::util::now_secs() - 30,
            files: 1234,
            dirs: 56,
            bytes: 4096,
            indexing: false,
            initial_baseline: false,
        });

        let status = freshness_text(&app);
        assert!(status.contains("reconciling"), "{status}");
        assert!(status.contains("displayed data may be stale"), "{status}");
        assert!(
            status.contains(&format!("{} files", count_human(1234))),
            "{status}"
        );
        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
    }

    #[test]
    fn uncertain_watch_status_never_looks_caught_up() {
        let uncertain = crate::util::WatchStatus {
            pending: 0,
            kernel_empty: true,
            uncertain: true,
            updates_committed: 5,
            ..Default::default()
        };
        assert_eq!(
            watch_status_label(&uncertain),
            "live · event state uncertain · overflow count unavailable · reconciliation needed"
        );

        let caught_up = crate::util::WatchStatus {
            pending: 0,
            kernel_empty: true,
            updates_committed: 5,
            kernel_overflow_notifications: Some(0),
            ..Default::default()
        };
        assert_eq!(
            watch_status_label(&caught_up),
            "live · caught up · 5 updates"
        );

        let scan_first = crate::util::WatchStatus {
            scan_first: true,
            uncertain: true,
            kernel_overflow_notifications: Some(2),
            ..Default::default()
        };
        assert_eq!(
            watch_status_label(&scan_first),
            "scan-first · stale observation · 2 kernel overflows"
        );
        let verified_scan_first = crate::util::WatchStatus {
            scan_first: true,
            ..Default::default()
        };
        assert_eq!(
            watch_status_label(&verified_scan_first),
            "scan-first · last observation verified"
        );
    }

    #[test]
    fn legacy_watch_status_labels_overflow_count_as_unavailable() {
        let legacy = crate::util::WatchStatus {
            pending: 0,
            kernel_empty: true,
            updates_committed: 5,
            kernel_overflow_notifications: None,
            ..Default::default()
        };
        assert_eq!(
            watch_status_label(&legacy),
            "live · caught up · 5 updates · overflow count unavailable"
        );
    }

    #[test]
    fn ctrl_c_quits_but_plain_c_remains_storage_shortcut() {
        assert!(is_interrupt_key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(!is_interrupt_key(KeyCode::Char('c'), KeyModifiers::NONE));
    }

    #[test]
    fn treemap_tiles_are_non_overlapping_and_allocate_area_by_size() {
        let mut tiles = Vec::new();
        split_treemap(
            &[(0, 90), (1, 10)],
            Rect {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            },
            &mut tiles,
        );
        assert_eq!(tiles.len(), 2);
        let areas: std::collections::HashMap<_, _> = tiles
            .iter()
            .map(|(index, rect)| (*index, usize::from(rect.width) * usize::from(rect.height)))
            .collect();
        assert_eq!(areas.get(&0), Some(&90));
        assert_eq!(areas.get(&1), Some(&10));
        for (i, (_, left)) in tiles.iter().enumerate() {
            for (_, right) in tiles.iter().skip(i + 1) {
                let overlap = left.x < right.x + right.width
                    && right.x < left.x + left.width
                    && left.y < right.y + right.height
                    && right.y < left.y + left.height;
                assert!(!overlap, "treemap rectangles overlap: {left:?} {right:?}");
            }
        }
    }

    #[test]
    fn growth_delta_is_signed_window_total_not_a_rate() {
        assert_eq!(growth_delta(0), "0");
        assert_eq!(growth_delta(1024), "+1 KiB");
        assert_eq!(growth_delta(-2048), "-2 KiB");
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("dux-tui-{tag}-{}", std::process::id()))
    }

    fn install_test_root(store: &Store, path: &str) {
        store.set_meta("last_scan_root", path).unwrap();
        store.set_meta("root_dev", "1").unwrap();
        store.set_meta("root_inode", "1").unwrap();
        store
            .conn
            .execute(
                "INSERT OR IGNORE INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,uid,mtime)
             VALUES(1,1,'d',4096,4096,0,0)",
                [],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT OR IGNORE INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
             VALUES(1,1,?1,1,1,1)",
                [path.as_bytes()],
            )
            .unwrap();
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
                    // The test fixture lives under tmpfs-backed TMPDIR.
                    include_pseudo: true,
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let store = Store::open_ro(&db).unwrap();
        let (rdev, rino) = root_node(&store).unwrap().unwrap();
        let mut expanded = std::collections::HashSet::new();
        expanded.insert((rdev, rino));
        expanded.insert(id_of(&dir.join("a")));
        expanded.insert(id_of(&dir.join("a/aa")));

        let build = |exp: &std::collections::HashSet<(i64, i64)>| {
            let mut app = App::new(&store, &db, rdev, rino).unwrap();
            app.expanded = exp.clone();
            app.refresh_growth_map(&store).unwrap();
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

    #[test]
    fn large_files_are_ranked_paged_and_join_the_selected_growth_window() {
        let dir = tmp("large-files");
        let db = tmp("large-files-db");
        let _ = std::fs::remove_dir_all(&dir);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
        std::fs::create_dir_all(&dir).unwrap();
        for index in 0..205 {
            std::fs::write(
                dir.join(format!("file-{index:03}.dat")),
                vec![index as u8; 2048 + index],
            )
            .unwrap();
        }
        let mut store = Store::open_rw(&db).unwrap();
        scan::scan(
            &mut store,
            &dir,
            &ScanOptions {
                progress: false,
                ..Default::default()
            },
        )
        .unwrap();
        let fast = id_of(&dir.join("file-201.dat"));
        let slow = id_of(&dir.join("file-202.dat"));
        let boundary = id_of(&dir.join("file-203.dat"));
        store
            .conn
            .execute(
                "UPDATE inodes SET birth_ns=100
             WHERE (dev_id=?1 AND inode=?2) OR (dev_id=?3 AND inode=?4)
                OR (dev_id=?5 AND inode=?6)",
                params![fast.0, fast.1, slow.0, slow.1, boundary.0, boundary.1],
            )
            .unwrap();
        let bucket = crate::util::now_secs() / crate::store::GROWTH_BUCKET_SECS;
        store
            .conn
            .execute(
                "INSERT INTO growth(bucket,dev_id,inode,birth_ns,delta)
                 SELECT ?1,?2,?3,birth_ns,?4 FROM inodes WHERE dev_id=?2 AND inode=?3 AND birth_ns!=0",
                params![bucket, slow.0, slow.1, 2 * 1024 * 1024],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO growth(bucket,dev_id,inode,birth_ns,delta)
                 SELECT ?1,?2,?3,birth_ns,?4 FROM inodes WHERE dev_id=?2 AND inode=?3 AND birth_ns!=0",
                params![bucket, fast.0, fast.1, 3 * 1024 * 1024],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO growth(bucket,dev_id,inode,birth_ns,delta)
                 VALUES(?1,?2,?3,100,(-9223372036854775807-1))",
                params![bucket, boundary.0, boundary.1],
            )
            .unwrap();
        let (root_dev, root_inode) = root_node(&store).unwrap().unwrap();
        let mut app = App::new(&store, &db, root_dev, root_inode).unwrap();
        app.screen = Screen::LargeFiles;
        app.refresh_panels(&store).unwrap();
        assert_eq!(app.large_total_count, 205);
        assert_eq!(app.large_files.len(), LARGE_PAGE_SIZE);
        assert!(app
            .large_files
            .windows(2)
            .all(|rows| rows[0].allocated >= rows[1].allocated));
        let expected_total: i64 = store
            .conn
            .query_row("SELECT SUM(blocks) FROM inodes WHERE kind!='d'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(app.large_total_bytes, expected_total);
        let first_page: std::collections::HashSet<_> = app
            .large_files
            .iter()
            .map(|row| (row.dev, row.inode))
            .collect();
        app.large_page_offset = LARGE_PAGE_SIZE;
        app.refresh_panels(&store).unwrap();
        assert_eq!(app.large_files.len(), 100);
        assert!(app
            .large_files
            .iter()
            .all(|row| !first_page.contains(&(row.dev, row.inode))));
        app.large_sort = LargeSort::Growth;
        app.large_page_offset = 0;
        app.refresh_panels(&store).unwrap();
        assert!(app.large_files[0].path.ends_with("file-203.dat"));
        assert_eq!(app.large_files[0].growth, i64::MIN);
        assert!(app
            .recent_changes
            .iter()
            .any(|(path, delta)| path.ends_with("file-203.dat") && *delta == i64::MIN));
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
    }

    #[test]
    fn large_file_refresh_reports_row_conversion_failure() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::store::SCHEMA_SQL).unwrap();
        // SQLite's non-STRICT schema permits a malformed value. A failed
        // conversion must fail the refresh, rather than hide an indexed file.
        conn.execute_batch(
            "INSERT INTO inodes(dev_id,inode,kind,blocks,uid,mtime)
             VALUES(1,2,'f',4096,0,'malformed')",
        )
        .unwrap();
        let store = Store {
            conn,
            schema_version: crate::store::SCHEMA_VERSION,
        };
        install_test_root(&store, "/");
        let mut app = App::new(&store, std::path::Path::new("unused-memory-index"), 1, 1).unwrap();
        let error = app.refresh_large_files(&store, "", &[]).unwrap_err();
        assert!(
            error.to_string().contains("Invalid column type"),
            "unexpected failure: {error:#}"
        );
    }

    #[test]
    fn distribution_refresh_propagates_sql_failures() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::store::SCHEMA_SQL).unwrap();
        let store = Store {
            conn,
            schema_version: crate::store::SCHEMA_VERSION,
        };
        install_test_root(&store, "/");
        let mut app = App::new(&store, std::path::Path::new("unused-memory-index"), 1, 1).unwrap();
        app.distribution = Distribution::Owners;
        assert_eq!(aggregate_groups(&store, &app).unwrap().len(), 1);
        store.conn.execute_batch("DROP TABLE inodes").unwrap();
        assert!(app.refresh_groups(&store).is_err());
    }

    #[test]
    fn application_group_refresh_propagates_index_lookup_failures() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::store::SCHEMA_SQL).unwrap();
        conn.execute_batch(
            "INSERT INTO meta(key,value) VALUES
             ('last_scan_root','/'),('root_dev','1'),('root_inode','1');
             DROP TABLE dirents;",
        )
        .unwrap();
        let store = Store {
            conn,
            schema_version: crate::store::SCHEMA_VERSION,
        };
        let growth = std::collections::HashMap::new();
        let error = compute_groups(&store, "/", 1, 1, 0, &[], GroupView::Top, &growth)
            .err()
            .expect("broken index lookups must fail the group refresh");
        assert!(error.to_string().contains("no such table: dirents"));
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
            group_view: GroupView::Top,
            distribution: Distribution::Apps,
            rows,
            growth_map: std::collections::HashMap::new(),
            top_growth: Vec::new(),
            recent_changes: Vec::new(),
            top_files: Vec::new(),
            large_files: Vec::new(),
            large_total_count: 0,
            large_total_bytes: 0,
            groups: Vec::new(),
            total_size: total,
            items: 0,
            growth_per_day: 0,
            last_scan: 0,
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
            recent_changes: Vec::new(),
            top_files: Vec::new(),
            large_files: Vec::new(),
            large_total_count: 0,
            large_total_bytes: 0,
            large_page_offset: 0,
            large_selection: 0,
            large_sort: LargeSort::Size,
            groups: Vec::new(),
            total_size: 0,
            last_scan: 0,
            window_secs: 3600,
            scroll: 0,
            metric: Metric::Size,
            root_path: "/".into(),
            root_display_path: "/".into(),
            db,
            docker_paths: Vec::new(),
            group_view: GroupView::Top,
            fs: crate::util::FsStat::default(),
            daemon_live: false,
            dirty_since: None,
            paused_since: None,
            pause_reason: String::new(),
            throttled_since: None,
            watch: None,
            scan: None,
            containers: Vec::new(),
            waste: crate::containers::Waste::default(),
            cleanup: Vec::new(),
            storage_calc: Instant::now(),
            growth_map: std::collections::HashMap::new(),
            growth_calc: Instant::now(),
            items: 0,
            growth_per_day: 0,
            screen: Screen::Overview,
            explore_distribution: false,
            distribution: Distribution::Apps,
            show_help: false,
            focus: Focus::Tree,
            asel: 0,
            gsel: 0,
            fsel: 0,
            activity_sel: 0,
            reclaim_sel: 0,
            detail: String::new(),
            view_gen: 5,
            pending_child: None,
            last_refresh: None,
            refresh_error: None,
            health_error: None,
            last_storage: None,
            storage_error: None,
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
        assert_eq!(
            app.total_size, 111,
            "an obsolete view must not overwrite current panels"
        );
    }

    #[test]
    fn parses_docker_data_root() {
        assert_eq!(
            parse_docker_data_root(r#"{ "debug": true, "data-root": "/mnt/docker-data" }"#)
                .unwrap()
                .as_deref(),
            Some("/mnt/docker-data")
        );
        assert_eq!(
            parse_docker_data_root("{\"data-root\":\"/mnt/docker\\u002ddata\"}")
                .unwrap()
                .as_deref(),
            Some("/mnt/docker-data")
        );
        assert!(parse_docker_data_root(r#"{ "data-root": "relative/path" }"#).is_err());
        assert!(parse_docker_data_root(r#"{ "data-root": 42 }"#).is_err());
        assert_eq!(
            parse_docker_data_root(r#"{ "log-level": "warn" }"#).unwrap(),
            None
        );
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

    #[test]
    fn worker_reports_open_failures_and_recovers() {
        let dir = tmp("worker-recovery");
        let db = tmp("worker-recovery-db");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db);
        std::fs::create_dir_all(&dir).unwrap();
        let id = id_of(&dir);
        let (requests, rx) = crossbeam_channel::bounded(1);
        let (results, output) = crossbeam_channel::bounded(1);
        let worker_db = db.clone();
        let worker = std::thread::spawn(move || refresh_worker(worker_db, id.0, id.1, rx, results));
        let snapshot = || Snapshot {
            expanded: std::collections::HashSet::from([id]),
            metric: Metric::Size,
            window_secs: 3600,
            distribution: Distribution::Apps,
            group_view: GroupView::Top,
            screen: Screen::Explore,
            large_page_offset: 0,
            large_sort: LargeSort::Size,
            gen: 0,
        };
        requests.send(snapshot()).unwrap();
        assert!(output
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .is_err());
        scan::rebuild_atomic(&db, &dir, &ScanOptions::default()).unwrap();
        requests.send(snapshot()).unwrap();
        let recovered = output.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(recovered.is_ok());
        drop(requests);
        worker.join().unwrap();
        drop(output);
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_file(db);
    }
}

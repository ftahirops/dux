use crate::store::{PathResolver, Store};
use crate::util::{human, now_secs};
use anyhow::{Context, Result};
use rusqlite::params;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// Resolve a filesystem path to its (dev, inode) and confirm it is in the index.
/// Returns None when the path is the index root (so callers query globally).
pub fn resolve_scope(store: &Store, path: &Path) -> Result<Option<(i64, i64)>> {
    let m = std::fs::symlink_metadata(path)
        .with_context(|| format!("cannot stat {}", path.display()))?;
    let dev = m.dev() as i64;
    let ino = m.ino() as i64;
    let in_index = store
        .conn
        .query_row(
            "SELECT 1 FROM inodes WHERE dev_id=?1 AND inode=?2",
            params![dev, ino],
            |_| Ok(()),
        )
        .is_ok();
    if !in_index {
        anyhow::bail!(
            "{} is not in the index — run `dux scan` over a parent path first",
            path.display()
        );
    }
    let root_dev: Option<i64> = store.get_meta("root_dev")?.and_then(|s| s.parse().ok());
    let root_ino: Option<i64> = store.get_meta("root_inode")?.and_then(|s| s.parse().ok());
    if Some(dev) == root_dev && Some(ino) == root_ino {
        Ok(None) // whole index — no subtree filter needed
    } else {
        Ok(Some((dev, ino)))
    }
}

/// Recursive CTE collecting every inode in the subtree rooted at (dev,inode),
/// walking the `dirents` parent→child edges. UNION (not UNION ALL) dedups so a
/// hardlink or a corrupt cycle terminates; the depth bound mirrors the 4096
/// cycle-guard used by the Rust tree walkers. The two bind params are dev, inode.
pub(crate) const SUBTREE_CTE: &str = "WITH RECURSIVE sub(d,i,depth) AS (
        SELECT ?,?,0
        UNION
        SELECT de.dev_id, de.inode, sub.depth+1 FROM dirents de
        JOIN sub ON de.parent_inode=sub.i AND de.parent_dev=sub.d
        WHERE NOT (de.dev_id=de.parent_dev AND de.inode=de.parent_inode) AND sub.depth<4096
    ) SELECT d,i FROM sub";

/// The subtree CTE stopped at `max_depth` levels below the root (root = depth 0).
///
/// This PRUNES the recursion rather than filtering after the fact: `du --depth 1`
/// on a huge tree reads the root's children instead of walking every descendant
/// and discarding it in Rust. Pruning is sound because a directory's
/// `recursive_bytes` is materialised at scan time, so a parent's total is already
/// correct without visiting the descendants we skip.
///
/// `max_depth` is clamped to the same 4096 cycle-guard as [`SUBTREE_CTE`], so a
/// corrupt cycle still terminates. It is a `usize` formatted into the SQL (never
/// user text), so there is no injection surface.
pub(crate) fn subtree_cte_depth(max_depth: Option<usize>) -> String {
    match max_depth {
        None => SUBTREE_CTE.to_string(),
        Some(md) => format!(
            "WITH RECURSIVE sub(d,i,depth) AS (
        SELECT ?,?,0
        UNION
        SELECT de.dev_id, de.inode, sub.depth+1 FROM dirents de
        JOIN sub ON de.parent_inode=sub.i AND de.parent_dev=sub.d
        WHERE NOT (de.dev_id=de.parent_dev AND de.inode=de.parent_inode) AND sub.depth<{}
    ) SELECT d,i FROM sub",
            md.min(4096)
        ),
    }
}

/// Subtree filter for queries over the `inodes`/`growth` tables (their own
/// columns are `dev_id`,`inode`).
fn scope_predicate() -> String {
    format!(" AND (dev_id,inode) IN ({SUBTREE_CTE})")
}

/// Clamp a usize LIMIT to a positive i64: a value past i64::MAX casts negative,
/// and SQLite treats a negative LIMIT as "unlimited" (returns everything).
fn lim(n: usize) -> i64 {
    n.min(i64::MAX as usize) as i64
}

/// Escape SQLite GLOB metacharacters (`*`, `?`, `[`) by wrapping each in a
/// one-char class (`[*]` etc.), so a value meant to be literal isn't a pattern.
fn glob_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '*' | '?' | '[' => {
                out.push('[');
                out.push(c);
                out.push(']');
            }
            _ => out.push(c),
        }
    }
    out
}

pub struct Row {
    pub path: String,
    pub size: i64,
    pub inodes: i64,
    pub mtime: i64,
    pub kind: char,
    pub uid: i64,
}

/// Inspect one exact path entirely from the index. Lookup is narrowed by the
/// basename index, then candidate parent paths are resolved and compared. This
/// works even when the caller cannot traverse the real directory (trusted `dux`
/// group members can query the protected index without a failing filesystem stat).
pub fn inspect(store: &Store, path: &Path) -> Result<Row> {
    let requested = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };

    // The root uses a self-parent dirent whose name is the full root path, so it
    // needs a direct identity check before ordinary basename lookup.
    if let Some((dev, inode)) = index_root(store) {
        let root_path = store.path_of(dev, inode).unwrap_or_default();
        if Path::new(&root_path) == requested {
            return inode_row(store, dev, inode, root_path);
        }
    }

    let basename = requested
        .file_name()
        .context("path has no filename")?
        .as_bytes();
    let mut stmt = store.conn.prepare(
        "SELECT d.dev_id,d.inode,d.parent_dev,d.parent_inode,d.name
         FROM dirents d WHERE d.name=?1",
    )?;
    let candidates = stmt.query_map(params![basename], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, Vec<u8>>(4)?,
        ))
    })?;
    let mut resolver = PathResolver::new(&store.conn);
    for candidate in candidates {
        let (dev, inode, pdev, pino, raw_name) = candidate?;
        let parent = resolver.resolve(pdev, pino);
        let name = String::from_utf8_lossy(&raw_name);
        let full = if parent.ends_with('/') {
            format!("{parent}{name}")
        } else {
            format!("{parent}/{name}")
        };
        if Path::new(&full) == requested {
            return inode_row(store, dev, inode, full);
        }
    }
    anyhow::bail!(
        "{} is not in the index — wait for the daemon or run `dux scan`",
        path.display()
    )
}

fn inode_row(store: &Store, dev: i64, inode: i64, path: String) -> Result<Row> {
    Ok(store.conn.query_row(
        "SELECT blocks,recursive_bytes,recursive_inodes,mtime,kind,uid
         FROM inodes WHERE dev_id=?1 AND inode=?2",
        params![dev, inode],
        |r| {
            let kind: String = r.get(4)?;
            let kind = kind.chars().next().unwrap_or('?');
            Ok(Row {
                path,
                size: if kind == 'd' { r.get(1)? } else { r.get(0)? },
                inodes: if kind == 'd' { r.get(2)? } else { 1 },
                mtime: r.get(3)?,
                kind,
                uid: r.get(5)?,
            })
        },
    )?)
}

/// Largest nodes under the index. `dirs`: directories ranked by recursive bytes
/// (or recursive inode count when `by_inodes`); otherwise files by own size.
pub fn top(
    store: &Store,
    dirs: bool,
    limit: usize,
    scope: Option<(i64, i64)>,
    by_inodes: bool,
) -> Result<Vec<Row>> {
    // disk usage is measured in allocated blocks (matches du / dir totals);
    // sparse files therefore report their real on-disk footprint.
    let col = if by_inodes {
        "recursive_inodes"
    } else if dirs {
        "recursive_bytes"
    } else {
        "blocks"
    };
    let kind_cmp = if dirs { "=" } else { "!=" };
    let mut sql = format!(
        "SELECT dev_id, inode, blocks, recursive_bytes, recursive_inodes, mtime, kind, uid, {col} AS s
         FROM inodes WHERE kind{kind_cmp}'d'"
    );
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some((d, i)) = scope {
        sql.push_str(&scope_predicate());
        args.push(Box::new(d));
        args.push(Box::new(i));
    }
    sql.push_str(" ORDER BY s DESC LIMIT ?");
    args.push(Box::new(lim(limit)));

    let mut stmt = store.conn.prepare(&sql)?;
    let pref: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(pref.as_slice(), |r| {
        let kind: String = r.get(6)?;
        let k = kind.chars().next().unwrap_or('?');
        let size = if k == 'd' {
            r.get::<_, i64>(3)?
        } else {
            r.get::<_, i64>(2)?
        };
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            size,
            r.get::<_, i64>(4)?,
            r.get::<_, i64>(5)?,
            k,
            r.get::<_, i64>(7)?,
        ))
    })?;
    let mut out = Vec::new();
    let mut pr = PathResolver::new(&store.conn);
    for row in rows {
        let (dev, inode, size, inodes, mtime, kind, uid) = row?;
        out.push(Row {
            path: pr.resolve(dev, inode),
            size,
            inodes: if kind == 'd' { inodes } else { 1 },
            mtime,
            kind,
            uid,
        });
    }
    Ok(out)
}

pub struct FindOpts {
    pub name: Option<String>,
    pub ext: Option<String>,
    pub newer_than: Option<i64>, // secs
    pub larger_than: Option<i64>,
    pub owner_uid: Option<i64>,
    pub limit: usize,
    pub scope: Option<(i64, i64)>,
}

/// Ultra-fast search over the live index — the locate/find replacement. Returns
/// one row per matching directory-entry, so EVERY hardlink path is findable (a
/// search for any valid name resolves to that exact path).
pub fn find(store: &Store, o: &FindOpts) -> Result<Vec<Row>> {
    // dirents carry the path; inodes carry blocks/mtime/uid (allocated blocks =
    // disk usage, consistent with `top`).
    let mut sql = String::from(
        "SELECT d.dev_id, d.inode, i.blocks, i.mtime, i.kind, d.parent_dev, d.parent_inode, d.name, i.uid
         FROM dirents d JOIN inodes i ON i.dev_id=d.dev_id AND i.inode=d.inode WHERE 1=1",
    );
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(n) = &o.name {
        // GLOB natively supports * and ?; the trigram FTS accelerates it. The
        // external-content FTS shares dirents.rowid, so we match on rowid.
        let pat = if n.contains('*') || n.contains('?') {
            n.clone() // user typed a glob — honor their * and ? (and [ ] classes)
        } else {
            // bare term => substring match. Escape GLOB metacharacters (`*`, `?`,
            // `[`) so a literal name like `report[final]` matches the real file
            // instead of being read as an unterminated character class — which
            // GLOB silently matches against nothing. Mirrors the `--ext` path.
            format!("*{}*", glob_escape(n))
        };
        sql.push_str(" AND d.rowid IN (SELECT rowid FROM names_fts WHERE name GLOB ?)");
        args.push(Box::new(pat));
    }
    if let Some(e) = &o.ext {
        // an extension is LITERAL — escape GLOB metacharacters so `--ext c++` or
        // `--ext d]` match the real extension instead of being treated as a glob.
        let e = glob_escape(e.trim_start_matches('.'));
        sql.push_str(" AND d.rowid IN (SELECT rowid FROM names_fts WHERE name GLOB ?)");
        args.push(Box::new(format!("*.{e}")));
    }
    if let Some(t) = o.newer_than {
        let cutoff = now_secs() - t;
        sql.push_str(" AND i.mtime >= ?");
        args.push(Box::new(cutoff));
    }
    if let Some(s) = o.larger_than {
        sql.push_str(" AND i.blocks >= ?");
        args.push(Box::new(s));
    }
    if let Some(u) = o.owner_uid {
        sql.push_str(" AND i.uid = ?");
        args.push(Box::new(u));
    }
    if let Some((d, i)) = o.scope {
        sql.push_str(&format!(" AND (d.dev_id,d.inode) IN ({SUBTREE_CTE})"));
        args.push(Box::new(d));
        args.push(Box::new(i));
    }
    // newest first when searching by recency, else biggest first
    if o.newer_than.is_some() {
        sql.push_str(" ORDER BY i.mtime DESC");
    } else {
        sql.push_str(" ORDER BY i.blocks DESC");
    }
    sql.push_str(" LIMIT ?");
    args.push(Box::new(lim(o.limit)));

    let mut stmt = store.conn.prepare(&sql)?;
    let params_ref: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(params_ref.as_slice(), |r| {
        let name: Vec<u8> = r.get(7)?;
        Ok((
            r.get::<_, i64>(0)?,    // dev
            r.get::<_, i64>(1)?,    // inode
            r.get::<_, i64>(2)?,    // blocks
            r.get::<_, i64>(3)?,    // mtime
            r.get::<_, String>(4)?, // kind
            r.get::<_, i64>(5)?,    // parent_dev
            r.get::<_, i64>(6)?,    // parent_inode
            String::from_utf8_lossy(&name).into_owned(),
            r.get::<_, i64>(8)?,
        ))
    })?;
    let mut out = Vec::new();
    let mut pr = PathResolver::new(&store.conn);
    for row in rows {
        let (dev, inode, size, mtime, kind, pdev, pino, name, uid) = row?;
        // exact matched path = this entry's parent path + its own name (so the
        // path shown is the link that matched, not an arbitrary other hardlink).
        let path = if (pdev == dev && pino == inode) || pino == 0 {
            name
        } else {
            let prefix = pr.resolve(pdev, pino);
            if prefix.ends_with('/') {
                format!("{prefix}{name}")
            } else {
                format!("{prefix}/{name}")
            }
        };
        out.push(Row {
            path,
            size,
            inodes: 1,
            mtime,
            kind: kind.chars().next().unwrap_or('?'),
            uid,
        });
    }
    Ok(out)
}

pub struct GrowthRow {
    pub path: String,
    pub delta: i64,
}

/// Fastest-growing inodes within the window, from the rolling `changes` log.
pub fn growth(
    store: &Store,
    since_secs: i64,
    limit: usize,
    scope: Option<(i64, i64)>,
) -> Result<Vec<GrowthRow>> {
    // growth history is bucketed (5-min); the window is rounded to the bucket
    // Round the bucket cutoff UP so the window never exceeds what was asked: a
    // floor here would include the whole bucket containing (now-window), handing
    // back up to ~5 min more than requested (e.g. `--since 10m` → ~15m).
    let cutoff = (now_secs() - since_secs + crate::store::GROWTH_BUCKET_SECS - 1)
        / crate::store::GROWTH_BUCKET_SECS;
    let mut sql =
        String::from("SELECT dev_id, inode, SUM(delta) AS d FROM growth WHERE bucket >= ?");
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(cutoff)];
    if let Some((d, i)) = scope {
        // reuse the scope predicate against the growth rows
        sql.push_str(&scope_predicate());
        args.push(Box::new(d));
        args.push(Box::new(i));
    }
    sql.push_str(" GROUP BY dev_id, inode HAVING d != 0 ORDER BY d DESC LIMIT ?");
    args.push(Box::new(lim(limit)));
    let mut stmt = store.conn.prepare(&sql)?;
    let pref: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(pref.as_slice(), |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    let mut pr = PathResolver::new(&store.conn);
    for row in rows {
        let (dev, inode, delta) = row?;
        out.push(GrowthRow {
            path: pr.resolve(dev, inode),
            delta,
        });
    }
    Ok(out)
}

pub struct OwnerRow {
    pub uid: i64,
    pub bytes: i64,
    pub files: i64,
}

pub fn by_owner(store: &Store, limit: usize) -> Result<Vec<OwnerRow>> {
    let mut stmt = store.conn.prepare(
        "SELECT uid, SUM(blocks) AS s, COUNT(*) FROM inodes
         WHERE kind!='d'
         GROUP BY uid ORDER BY s DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![lim(limit)], |r| {
        Ok(OwnerRow {
            uid: r.get(0)?,
            bytes: r.get(1)?,
            files: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub struct ExtRow {
    pub ext: String,
    pub bytes: i64,
    pub files: i64,
}

pub fn by_ext(store: &Store, limit: usize) -> Result<Vec<ExtRow>> {
    // Extract extension in Rust to avoid brittle SQL string ops. Count each file
    // inode once via its prime dirent (a hardlink's extra names don't re-count).
    let mut stmt = store.conn.prepare(
        "SELECT d.name, i.blocks FROM dirents d
         JOIN inodes i ON i.dev_id=d.dev_id AND i.inode=d.inode
         WHERE i.kind='f' AND d.prime=1",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?)))?;
    use std::collections::HashMap;
    let mut map: HashMap<String, (i64, i64)> = HashMap::new();
    for row in rows {
        let (name_bytes, size) = row?;
        let name = String::from_utf8_lossy(&name_bytes).into_owned();
        let ext = match name.rsplit_once('.') {
            Some((_, e)) if !e.is_empty() && e.len() <= 16 => e.to_lowercase(),
            _ => "(none)".to_string(),
        };
        let e = map.entry(ext).or_insert((0, 0));
        e.0 += size;
        e.1 += 1;
    }
    let mut v: Vec<ExtRow> = map
        .into_iter()
        .map(|(ext, (bytes, files))| ExtRow { ext, bytes, files })
        .collect();
    v.sort_by_key(|r| std::cmp::Reverse(r.bytes));
    v.truncate(limit);
    Ok(v)
}

/// Net change per path within a window (`dux diff`), ranked by MAGNITUDE so the
/// biggest fills AND the biggest frees both surface — the "what changed the disk"
/// question. Same bucketed-growth source as `growth`, but signed and abs-ordered.
pub fn changed(
    store: &Store,
    since_secs: i64,
    limit: usize,
    scope: Option<(i64, i64)>,
) -> Result<Vec<GrowthRow>> {
    let cutoff = (now_secs() - since_secs + crate::store::GROWTH_BUCKET_SECS - 1)
        / crate::store::GROWTH_BUCKET_SECS;
    let mut sql =
        String::from("SELECT dev_id, inode, SUM(delta) AS d FROM growth WHERE bucket >= ?");
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(cutoff)];
    if let Some((d, i)) = scope {
        sql.push_str(&scope_predicate());
        args.push(Box::new(d));
        args.push(Box::new(i));
    }
    // abs(d) DESC: rank by magnitude of change, sign preserved in the value.
    sql.push_str(" GROUP BY dev_id, inode HAVING d != 0 ORDER BY abs(d) DESC LIMIT ?");
    args.push(Box::new(lim(limit)));
    let mut stmt = store.conn.prepare(&sql)?;
    let pref: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(pref.as_slice(), |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    let mut pr = PathResolver::new(&store.conn);
    for row in rows {
        let (dev, inode, delta) = row?;
        out.push(GrowthRow {
            path: pr.resolve(dev, inode),
            delta,
        });
    }
    Ok(out)
}

/// (indexed node count, indexed bytes) — the root's recursive totals, or a
/// whole-table fallback when the root marker is missing. Shared by status/metrics.
pub fn index_totals(store: &Store) -> (i64, i64) {
    let rd: Option<i64> = store
        .get_meta("root_dev")
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok());
    let ri: Option<i64> = store
        .get_meta("root_inode")
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok());
    if let (Some(d), Some(i)) = (rd, ri) {
        if let Ok(t) = store.conn.query_row(
            "SELECT recursive_inodes, recursive_bytes FROM inodes WHERE dev_id=?1 AND inode=?2",
            params![d, i],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ) {
            return t;
        }
    }
    let count = store
        .conn
        .query_row("SELECT COUNT(*) FROM inodes", [], |r| r.get(0))
        .unwrap_or(0);
    let bytes = store
        .conn
        .query_row(
            "SELECT recursive_bytes FROM inodes WHERE kind='d' ORDER BY recursive_bytes DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    (count, bytes)
}

#[derive(Debug)]
pub struct DuRow {
    pub path: String,
    pub bytes: i64, // allocated (blocks*512) — matches `du` default, not apparent
    pub is_dir: bool,
    pub mtime: i64,
}

/// `du`-equivalent listing from the index: every directory in the subtree (and
/// files too when `all`) with its allocated size. Directories carry their
/// recursive total; files their own blocks — exactly what `du` reports.
///
/// `max_depth` (root = 0) is pushed down into the subtree walk, so a shallow
/// `du --depth 1` over a huge tree never materialises the deep rows it would only
/// discard. Callers get rows already depth-filtered — do not re-filter.
pub fn du(
    store: &Store,
    scope: Option<(i64, i64)>,
    all: bool,
    max_depth: Option<usize>,
) -> Result<Vec<DuRow>> {
    // Resolve the subtree root: explicit scope, else the index root marker.
    let root = match scope {
        Some(x) => Some(x),
        None => {
            let rd: Option<i64> = store.get_meta("root_dev")?.and_then(|s| s.parse().ok());
            let ri: Option<i64> = store.get_meta("root_inode")?.and_then(|s| s.parse().ok());
            rd.zip(ri)
        }
    };
    let kind_filter = if all { "" } else { " AND kind='d'" };
    let cte = subtree_cte_depth(max_depth);
    let (sql, use_scope) = match root {
        Some(_) => (
            format!(
                "SELECT dev_id, inode, kind, recursive_bytes, blocks, mtime FROM inodes
                 WHERE (dev_id,inode) IN ({cte}){kind_filter}"
            ),
            true,
        ),
        // No root marker: a degenerate index (never scanned, or the markers were
        // lost). There is no root to measure depth from, so the walk can't be
        // pruned — fall back to the whole table and filter by path depth below.
        None => (
            format!(
                "SELECT dev_id, inode, kind, recursive_bytes, blocks, mtime FROM inodes
                 WHERE 1=1{kind_filter}"
            ),
            false,
        ),
    };
    let mut stmt = store.conn.prepare(&sql)?;
    // `stmt` and `pr` both borrow `conn` immutably, so rows can be resolved as they
    // stream — no intermediate Vec of the whole subtree.
    let mut pr = PathResolver::new(&store.conn);
    let map = |r: &rusqlite::Row| -> rusqlite::Result<(i64, i64, String, i64, i64, i64)> {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get::<_, String>(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
        ))
    };
    let rows = if use_scope {
        let (d, i) = root.unwrap();
        stmt.query_map(params![d, i], map)?
    } else {
        stmt.query_map([], map)?
    };
    let mut out: Vec<DuRow> = Vec::new();
    for row in rows {
        let (dev, inode, kind, rbytes, blocks, mtime) = row?;
        let is_dir = kind == "d";
        out.push(DuRow {
            path: pr.resolve(dev, inode),
            bytes: if is_dir { rbytes } else { blocks },
            is_dir,
            mtime,
        });
    }
    // The pruned CTE already applied `max_depth`; only the rootless fallback above
    // still needs it, measured from the shallowest path in the set.
    if !use_scope {
        if let Some(md) = max_depth {
            let root_depth = out
                .iter()
                .map(|r| r.path.trim_end_matches('/').matches('/').count())
                .min()
                .unwrap_or(0);
            out.retain(|r| r.path.trim_end_matches('/').matches('/').count() - root_depth <= md);
        }
    }
    Ok(out)
}

/// (dev,inode) of the index root, from the scan-time meta markers.
pub fn index_root(store: &Store) -> Option<(i64, i64)> {
    let d: Option<i64> = store
        .get_meta("root_dev")
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok());
    let i: Option<i64> = store
        .get_meta("root_inode")
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok());
    d.zip(i)
}

/// Sum of allocated bytes of the subtree rooted at (dev,inode) — used to size a
/// container's writable layer / volume from paths, without re-walking the fs.
pub fn subtree_bytes(store: &Store, dev: i64, inode: i64) -> i64 {
    store
        .conn
        .query_row(
            "SELECT recursive_bytes FROM inodes WHERE dev_id=?1 AND inode=?2",
            params![dev, inode],
            |r| r.get(0),
        )
        .unwrap_or(0)
}

/// Index status summary.
pub fn status(store: &Store, db: &Path) -> Result<String> {
    let root = store.get_meta("last_scan_root")?.unwrap_or_default();
    let ts: i64 = store
        .get_meta("last_scan_ts")?
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let root_dev: Option<i64> = store.get_meta("root_dev")?.and_then(|s| s.parse().ok());
    let root_inode: Option<i64> = store.get_meta("root_inode")?.and_then(|s| s.parse().ok());
    let (count, total): (i64, i64) = match (root_dev, root_inode) {
        (Some(d), Some(i)) => store
            .conn
            .query_row(
                "SELECT recursive_inodes, recursive_bytes FROM inodes WHERE dev_id=?1 AND inode=?2",
                params![d, i],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or((0, 0)),
        _ => {
            let count = store
                .conn
                .query_row("SELECT COUNT(*) FROM inodes", [], |r| r.get(0))
                .unwrap_or(0);
            let total = store
                .conn
                .query_row(
                    "SELECT recursive_bytes FROM inodes WHERE kind='d'
                     ORDER BY recursive_bytes DESC LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            (count, total)
        }
    };
    // Phrase the freshness without the awkward "now ago" / "never ago".
    let age = if ts == 0 {
        "never scanned".to_string()
    } else {
        let a = crate::util::ago(ts);
        if a == "now" {
            "just now".to_string()
        } else {
            format!("{a} ago")
        }
    };

    let mut out = format!("root: {root}\n");
    // live filesystem capacity (df-style) for the scanned root
    if let Some(fs) = crate::util::fs_stat(std::path::Path::new(&root)) {
        out.push_str(&format!(
            "filesystem: root mount {} used / {} total  ({} free, {:.0}% used)\n",
            human(fs.used),
            human(fs.total),
            human(fs.avail),
            fs.use_pct(),
        ));
        out.push_str(&format!(
            "inodes:     {} used / {} total  ({:.0}% used)\n",
            fs.inodes_used,
            fs.inodes_total,
            fs.inode_pct(),
        ));
    }
    out.push_str(&format!(
        "indexed:    {count} nodes, {} (allocated blocks)\nlast scan:  {}\n",
        human(total),
        age
    ));
    // on-disk footprint of the index itself (db + WAL + reclaimable free pages)
    let dbsz = std::fs::metadata(db).map(|m| m.len() as i64).unwrap_or(0);
    let walsz = std::fs::metadata(format!("{}-wal", db.display()))
        .map(|m| m.len() as i64)
        .unwrap_or(0);
    let psize: i64 = store
        .conn
        .query_row("PRAGMA page_size", [], |r| r.get(0))
        .unwrap_or(4096);
    let free: i64 = store
        .conn
        .query_row("PRAGMA freelist_count", [], |r| r.get(0))
        .unwrap_or(0);
    // NB: the per-table FTS size used to come from `dbstat`, which visits EVERY
    // page of the database — ~18s cold on a multi-hundred-MB index. Dropped: the
    // db/WAL file sizes + reclaimable (freelist) are all O(1) PRAGMA/stat calls.
    out.push_str(&format!(
        "index size: {} db + {} WAL  ({} reclaimable)\n",
        human(dbsz),
        human(walsz),
        human(free * psize),
    ));
    // daemon liveness — only "live" if the heartbeat belongs to THIS db
    if crate::util::daemon_live_for(db) {
        out.push_str("daemon:     live (tracks create/delete/rename/growth)");
        if let Some(w) = crate::util::read_watch_status(db) {
            let resolve_pct = if w.events_seen == 0 {
                100.0
            } else {
                w.events_resolved as f64 * 100.0 / w.events_seen as f64
            };
            let queue_state = if w.kernel_empty {
                "drained"
            } else {
                "receiving"
            };
            out.push_str(&format!(
                "\npipeline:   {queue_state}; {} pending / {} cap; {} events ({resolve_pct:.1}% resolved); \
                 {} updates committed; {} dropped; {}ms since caught up",
                w.pending,
                w.max_pending,
                w.events_seen,
                w.updates_committed,
                w.updates_dropped,
                w.behind_ms,
            ));
        }
    } else {
        out.push_str("daemon:     not running — index is a static snapshot (run `dux daemon /`)");
    }
    // Throttled/behind: the CPU/IO governor is deliberately holding the daemon back
    // under sustained event load so it never disturbs the host. The index is LIVE
    // but intentionally stale — explain why, and how stale, so the numbers aren't
    // mistaken for real-time. Self-clears when the daemon catches up.
    if let Some(since) = store
        .get_meta("throttled_since")?
        .and_then(|s| s.parse::<i64>().ok())
    {
        out.push_str(&format!(
            "\nstate:      THROTTLED (protecting host) — capping its own CPU/I/O under heavy \
             filesystem activity, so live updates are delayed. Data is ~{} stale and \
             catches up automatically when load eases. Raise the ceiling with \
             `dux daemon --max-cpu N` for faster updates.",
            crate::util::ago(since)
        ));
    }
    // Transient pause (self-clearing): the resource guardian paused writes because
    // the host is under pressure. Nothing is lost (pending is kept) — distinct from
    // DIRTY; it resumes automatically when the host recovers.
    if let Some(since) = paused_since(store) {
        let reason = store
            .get_meta("pause_reason")
            .ok()
            .flatten()
            .unwrap_or_else(|| "system pressure".into());
        out.push_str(&format!(
            "\nstate:      WRITES PAUSED ({reason}) since {} — resumes when the host recovers",
            crate::util::ago(since)
        ));
    }
    // Known event loss (fanotify overflow / dropped backlog / partial watch) makes
    // the index untrustworthy even while "live"; surface it and recommend a rescan.
    if let Some(since) = store.get_meta("dirty_since")?.and_then(|s| s.parse().ok()) {
        out.push_str(&format!(
            "\nstate:      DIRTY since {} — missed events; rescan with `dux scan <root>`",
            crate::util::ago(since)
        ));
    }
    // A scan running RIGHT NOW (initial build or rescan) publishes live progress —
    // surface it so the user knows to wait rather than trusting a stale snapshot.
    if let Some(p) = crate::util::read_scan_progress() {
        let elapsed = (crate::util::now_secs() - p.started).max(0);
        let phase = if p.indexing {
            "building index"
        } else {
            "scanning"
        };
        out.push_str(&format!(
            "\nscan:       IN PROGRESS ({phase}) — {} files, {} dirs, {} so far, {}s elapsed",
            p.files,
            p.dirs,
            human(p.bytes),
            elapsed,
        ));
    }
    Ok(out)
}

/// Epoch seconds the daemon paused writes for low disk (self-clearing), if set.
pub fn paused_since(store: &Store) -> Option<i64> {
    store
        .get_meta("paused_since")
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
}

/// Epoch seconds the daemon fell behind under CPU/IO governing (self-clearing).
/// While set, the index is LIVE but intentionally stale to protect the host.
pub fn throttled_since(store: &Store) -> Option<i64> {
    store
        .get_meta("throttled_since")
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
}

/// Epoch seconds the index was marked dirty (fanotify overflow), if set.
pub fn dirty_since(store: &Store) -> Option<i64> {
    store
        .get_meta("dirty_since")
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
}

/// True when a watch daemon for THIS db has heart-beaten within the last 30s.
pub fn daemon_live(db: &Path) -> bool {
    crate::util::daemon_live_for(db)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("dux-query-{tag}-{}", std::process::id()))
    }

    fn rm(db: &std::path::Path) {
        for s in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{s}", db.display()));
        }
    }

    /// A linear tree: / -> a -> b -> c, each dir 4096 blocks, plus a file at /a/f.
    /// Depth from the root: / =0, /a =1, /a/b =2, /a/b/c =3.
    fn linear_tree(db: &std::path::Path) -> Store {
        rm(db);
        let store = Store::create_fresh(db).unwrap();
        {
            let c = &store.conn;
            // (inode, kind, blocks, recursive_bytes)
            for (ino, kind, blocks, rb) in [
                (1i64, "d", 4096i64, 20480i64),
                (2, "d", 4096, 16384),
                (3, "d", 4096, 8192),
                (4, "d", 4096, 4096),
                (5, "f", 4096, 4096),
            ] {
                c.execute(
                    "INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime)
                     VALUES(1,?1,?2,?3,?4,1,0,0)",
                    params![ino, kind, blocks, rb],
                )
                .unwrap();
            }
            // (parent_inode, name, inode) — root's dirent points at itself.
            for (p, name, ino) in [
                (1i64, "/", 1i64),
                (1, "a", 2),
                (2, "b", 3),
                (3, "c", 4),
                (2, "f", 5),
            ] {
                c.execute(
                    "INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
                     VALUES(1,?1,?2,1,?3,1)",
                    params![p, name.as_bytes(), ino],
                )
                .unwrap();
            }
            c.execute(
                "INSERT INTO meta(key,value) VALUES('root_dev','1'),('root_inode','1')",
                [],
            )
            .unwrap();
        }
        store
    }

    /// `du --depth N` must not enumerate below N. Without a depth bound the CTE
    /// walks the whole subtree, so a deep tree costs RAM proportional to the tree
    /// rather than to what the caller asked to see.
    #[test]
    fn du_depth_bounds_the_subtree_walk() {
        let db = tmp("du-depth");
        let store = linear_tree(&db);

        let d0 = du(&store, None, false, Some(0)).unwrap();
        assert_eq!(d0.len(), 1, "depth 0 = the root only, got {d0:?}");

        let d1 = du(&store, None, false, Some(1)).unwrap();
        assert_eq!(d1.len(), 2, "depth 1 = root + /a, got {d1:?}");

        let d2 = du(&store, None, false, Some(2)).unwrap();
        assert_eq!(d2.len(), 3, "depth 2 = root + /a + /a/b, got {d2:?}");

        // Unbounded still sees every directory (4 dirs, file excluded by `all=false`).
        let all = du(&store, None, false, None).unwrap();
        assert_eq!(all.len(), 4, "unbounded = every dir, got {all:?}");

        drop(store);
        rm(&db);
    }

    /// The depth bound must not change the reported totals: a directory's
    /// `recursive_bytes` is precomputed at scan time, so pruning the walk below it
    /// is safe — the parent's total already accounts for the descendants we skip.
    #[test]
    fn du_depth_preserves_recursive_totals() {
        let db = tmp("du-depth-totals");
        let store = linear_tree(&db);

        let d1 = du(&store, None, false, Some(1)).unwrap();
        let a = d1
            .iter()
            .find(|r| r.path.ends_with('a'))
            .expect("/a present");
        assert_eq!(
            a.bytes, 16384,
            "/a keeps its full recursive total even though /a/b was pruned"
        );

        drop(store);
        rm(&db);
    }

    /// `all=true` includes files, and files must respect the same depth bound.
    #[test]
    fn du_depth_applies_to_files_too() {
        let db = tmp("du-depth-files");
        let store = linear_tree(&db);

        // /a/f is at depth 2; depth 1 must exclude it.
        let d1 = du(&store, None, true, Some(1)).unwrap();
        assert!(
            !d1.iter().any(|r| r.path.ends_with('f')),
            "/a/f is depth 2, must be pruned at depth 1: {d1:?}"
        );

        let d2 = du(&store, None, true, Some(2)).unwrap();
        assert!(
            d2.iter().any(|r| r.path.ends_with('f')),
            "/a/f must appear at depth 2: {d2:?}"
        );

        drop(store);
        rm(&db);
    }
}

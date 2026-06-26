use crate::store::{PathResolver, Store};
use crate::util::{human, now_secs};
use anyhow::{Context, Result};
use rusqlite::params;
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

pub struct Row {
    pub path: String,
    pub size: i64,
    pub inodes: i64,
    pub mtime: i64,
    pub kind: char,
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
        "SELECT dev_id, inode, blocks, recursive_bytes, recursive_inodes, mtime, kind, {col} AS s
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
        ))
    })?;
    let mut out = Vec::new();
    let mut pr = PathResolver::new(&store.conn);
    for row in rows {
        let (dev, inode, size, inodes, mtime, kind) = row?;
        out.push(Row {
            path: pr.resolve(dev, inode),
            size,
            inodes: if kind == 'd' { inodes } else { 1 },
            mtime,
            kind,
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
        "SELECT d.dev_id, d.inode, i.blocks, i.mtime, i.kind, d.parent_dev, d.parent_inode, d.name
         FROM dirents d JOIN inodes i ON i.dev_id=d.dev_id AND i.inode=d.inode WHERE 1=1",
    );
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(n) = &o.name {
        // GLOB natively supports * and ?; the trigram FTS accelerates it. The
        // external-content FTS shares dirents.rowid, so we match on rowid.
        let pat = if n.contains('*') || n.contains('?') {
            n.clone()
        } else {
            format!("*{n}*") // bare term => substring match
        };
        sql.push_str(" AND d.rowid IN (SELECT rowid FROM names_fts WHERE name GLOB ?)");
        args.push(Box::new(pat));
    }
    if let Some(e) = &o.ext {
        let e = e.trim_start_matches('.');
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
        ))
    })?;
    let mut out = Vec::new();
    let mut pr = PathResolver::new(&store.conn);
    for row in rows {
        let (dev, inode, size, mtime, kind, pdev, pino, name) = row?;
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
    let cutoff = (now_secs() - since_secs) / crate::store::GROWTH_BUCKET_SECS;
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

/// Index status summary.
pub fn status(store: &Store, db: &Path) -> Result<String> {
    let root = store.get_meta("last_scan_root")?.unwrap_or_default();
    let ts: i64 = store
        .get_meta("last_scan_ts")?
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM inodes", [], |r| r.get(0))
        .unwrap_or(0);
    let root_dev: Option<i64> = store.get_meta("root_dev")?.and_then(|s| s.parse().ok());
    let root_inode: Option<i64> = store.get_meta("root_inode")?.and_then(|s| s.parse().ok());
    let total: i64 = match (root_dev, root_inode) {
        (Some(d), Some(i)) => store
            .conn
            .query_row(
                "SELECT recursive_bytes FROM inodes WHERE dev_id=?1 AND inode=?2",
                params![d, i],
                |r| r.get(0),
            )
            .unwrap_or(0),
        _ => store
            .conn
            .query_row(
                "SELECT recursive_bytes FROM inodes WHERE kind='d'
                 ORDER BY recursive_bytes DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0),
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
            "filesystem: {} used / {} total  ({} free, {:.0}% used)\n",
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
    let fts: i64 = store
        .conn
        .query_row(
            "SELECT COALESCE(SUM(pgsize),0) FROM dbstat WHERE name LIKE 'names_fts%'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    out.push_str(&format!(
        "index size: {} db + {} WAL  ({} search, {} reclaimable)\n",
        human(dbsz),
        human(walsz),
        human(fts),
        human(free * psize),
    ));
    // daemon liveness — only "live" if the heartbeat belongs to THIS db
    if crate::util::daemon_live_for(db) {
        out.push_str("daemon:     live (tracks create/delete/rename/growth)");
    } else {
        out.push_str("daemon:     not running — index is a static snapshot (run `dux daemon /`)");
    }
    // Known event loss (fanotify queue overflow) makes the index untrustworthy
    // even while the daemon is "live"; surface it loudly and recommend a rescan.
    if let Some(since) = store.get_meta("dirty_since")?.and_then(|s| s.parse().ok()) {
        out.push_str(&format!(
            "\nstate:      DIRTY since {} — missed events; rescan with `dux scan <root>`",
            crate::util::ago(since)
        ));
    }
    Ok(out)
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

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
            "SELECT 1 FROM nodes WHERE dev_id=?1 AND inode=?2",
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

/// A SQL predicate restricting `nodes` rows to descendants of (dev,inode).
/// Uses a recursive CTE over parent_inode. The two bind params are dev then inode.
// `depth` bound mirrors the 4096 cycle-guard used by the Rust tree walkers — a
// corrupt parent pointer (cycle) must not make this recursion run unbounded.
const SCOPE_PREDICATE: &str = " AND (dev_id,inode) IN (
    WITH RECURSIVE sub(d,i,depth) AS (
        SELECT ?,?,0
        UNION ALL
        SELECT n.dev_id, n.inode, sub.depth+1 FROM nodes n
        JOIN sub ON n.parent_inode=sub.i AND n.parent_dev=sub.d
        WHERE NOT (n.dev_id=n.parent_dev AND n.inode=n.parent_inode) AND sub.depth<4096
    ) SELECT d,i FROM sub
)";

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
         FROM nodes WHERE kind{kind_cmp}'d'"
    );
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some((d, i)) = scope {
        sql.push_str(SCOPE_PREDICATE);
        args.push(Box::new(d));
        args.push(Box::new(i));
    }
    sql.push_str(" ORDER BY s DESC LIMIT ?");
    args.push(Box::new(limit as i64));

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

/// Ultra-fast search over the live index — the locate/find replacement.
pub fn find(store: &Store, o: &FindOpts) -> Result<Vec<Row>> {
    // report allocated blocks (disk usage), consistent with `top`
    let mut sql = String::from("SELECT dev_id, inode, blocks, mtime, kind FROM nodes WHERE 1=1");
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(n) = &o.name {
        // GLOB natively supports * and ?; the trigram FTS accelerates it. The
        // external-content FTS shares nodes.rowid, so we match on rowid.
        let pat = if n.contains('*') || n.contains('?') {
            n.clone()
        } else {
            format!("*{n}*") // bare term => substring match
        };
        sql.push_str(" AND rowid IN (SELECT rowid FROM names_fts WHERE name GLOB ?)");
        args.push(Box::new(pat));
    }
    if let Some(e) = &o.ext {
        let e = e.trim_start_matches('.');
        sql.push_str(" AND rowid IN (SELECT rowid FROM names_fts WHERE name GLOB ?)");
        args.push(Box::new(format!("*.{e}")));
    }
    if let Some(t) = o.newer_than {
        let cutoff = now_secs() - t;
        sql.push_str(" AND mtime >= ?");
        args.push(Box::new(cutoff));
    }
    if let Some(s) = o.larger_than {
        sql.push_str(" AND blocks >= ?");
        args.push(Box::new(s));
    }
    if let Some(u) = o.owner_uid {
        sql.push_str(" AND uid = ?");
        args.push(Box::new(u));
    }
    if let Some((d, i)) = o.scope {
        sql.push_str(SCOPE_PREDICATE);
        args.push(Box::new(d));
        args.push(Box::new(i));
    }
    // newest first when searching by recency, else biggest first
    if o.newer_than.is_some() {
        sql.push_str(" ORDER BY mtime DESC");
    } else {
        sql.push_str(" ORDER BY blocks DESC");
    }
    sql.push_str(" LIMIT ?");
    args.push(Box::new(o.limit as i64));

    let mut stmt = store.conn.prepare(&sql)?;
    let params_ref: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(params_ref.as_slice(), |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, String>(4)?,
        ))
    })?;
    let mut out = Vec::new();
    let mut pr = PathResolver::new(&store.conn);
    for row in rows {
        let (dev, inode, size, mtime, kind) = row?;
        out.push(Row {
            path: pr.resolve(dev, inode),
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
    let cutoff = now_secs() - since_secs;
    let mut sql = String::from("SELECT dev_id, inode, SUM(delta) AS d FROM changes WHERE ts >= ?");
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(cutoff)];
    if let Some((d, i)) = scope {
        // reuse the scope predicate against the changes rows
        sql.push_str(SCOPE_PREDICATE);
        args.push(Box::new(d));
        args.push(Box::new(i));
    }
    sql.push_str(" GROUP BY dev_id, inode HAVING d != 0 ORDER BY d DESC LIMIT ?");
    args.push(Box::new(limit as i64));
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
        "SELECT uid, SUM(blocks) AS s, COUNT(*) FROM nodes
         WHERE kind!='d'
         GROUP BY uid ORDER BY s DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], |r| {
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
    // Extract extension in Rust to avoid brittle SQL string ops.
    let mut stmt = store
        .conn
        .prepare("SELECT name, blocks FROM nodes WHERE kind='f'")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    use std::collections::HashMap;
    let mut map: HashMap<String, (i64, i64)> = HashMap::new();
    for row in rows {
        let (name, size) = row?;
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
pub fn status(store: &Store) -> Result<String> {
    let root = store.get_meta("last_scan_root")?.unwrap_or_default();
    let ts: i64 = store
        .get_meta("last_scan_ts")?
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))
        .unwrap_or(0);
    let root_dev: Option<i64> = store.get_meta("root_dev")?.and_then(|s| s.parse().ok());
    let root_inode: Option<i64> = store.get_meta("root_inode")?.and_then(|s| s.parse().ok());
    let total: i64 = match (root_dev, root_inode) {
        (Some(d), Some(i)) => store
            .conn
            .query_row(
                "SELECT recursive_bytes FROM nodes WHERE dev_id=?1 AND inode=?2",
                params![d, i],
                |r| r.get(0),
            )
            .unwrap_or(0),
        _ => store
            .conn
            .query_row(
                "SELECT recursive_bytes FROM nodes WHERE inode=parent_inode
                 ORDER BY recursive_bytes DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0),
    };
    let age = if ts == 0 {
        "never".to_string()
    } else {
        crate::util::ago(ts)
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
        "indexed:    {count} nodes, {} (allocated blocks)\nlast scan:  {} ago\n",
        human(total),
        age
    ));
    // daemon liveness from the tmpfs heartbeat file
    let hb = crate::util::read_heartbeat();
    let hb_age = crate::util::now_secs() - hb;
    if hb != 0 && hb_age <= 30 {
        out.push_str("daemon:     live (tracks create/delete/rename/growth)");
    } else {
        out.push_str("daemon:     not running — index is a static snapshot (run `dux daemon /`)");
    }
    Ok(out)
}

/// True when the watch daemon has emitted a heartbeat within the last 30s.
pub fn daemon_live(_store: &Store) -> bool {
    let hb = crate::util::read_heartbeat();
    hb != 0 && (crate::util::now_secs() - hb) <= 30
}

use crate::store::{PathResolver, Store};
use crate::util::{human, indexed_identity, now_secs};
use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension};
use std::path::Path;

/// Resolve a filesystem path to its (dev, inode) and confirm it is in the index.
/// Returns None when the path is the index root (so callers query globally).
pub fn resolve_scope(store: &Store, path: &Path) -> Result<Option<(i64, i64)>> {
    let m = std::fs::symlink_metadata(path)
        .with_context(|| format!("cannot stat {}", path.display()))?;
    let (dev, ino) = indexed_identity(&m)
        .with_context(|| format!("filesystem identity for {} is out of range", path.display()))?;
    let in_index = match store.conn.query_row(
        "SELECT 1 FROM inodes WHERE dev_id=?1 AND inode=?2",
        params![dev, ino],
        |_| Ok(()),
    ) {
        Ok(()) => true,
        Err(rusqlite::Error::QueryReturnedNoRows) => false,
        Err(error) => return Err(error.into()),
    };
    if !in_index {
        anyhow::bail!(
            "{} is not in the index — run `dux scan` over a parent path first",
            path.display()
        );
    }
    if index_root(store)? == Some((dev, ino)) {
        Ok(None) // whole index — no subtree filter needed
    } else {
        Ok(Some((dev, ino)))
    }
}

/// Recursive CTE collecting every inode in the subtree rooted at (dev,inode),
/// walking the `dirents` parent→child edges. UNION (not UNION ALL) dedups so a
/// hardlink or a corrupt cycle terminates; the depth bound mirrors the 4096
/// cycle-guard used by the Rust tree walkers. The two bind params are dev, inode.
pub(crate) const SUBTREE_CTE: &str = "WITH RECURSIVE sub(d,i) AS (
        SELECT ?,?
        UNION
        SELECT de.dev_id, de.inode FROM dirents de
        JOIN sub ON de.parent_inode=sub.i AND de.parent_dev=sub.d
        WHERE NOT (de.dev_id=de.parent_dev AND de.inode=de.parent_inode)
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

/// Return literal runs which are guaranteed to occur in a GLOB pattern.
/// Character classes are deliberately excluded: their contents are choices,
/// not a literal substring. The trigram index is only a candidate filter; the
/// original GLOB predicate remains authoritative for case and wildcard rules.
fn glob_literal_runs(pattern: &str) -> Option<Vec<String>> {
    if pattern.contains('[') || pattern.contains(']') {
        return None;
    }
    let mut runs = Vec::new();
    let mut run = String::new();
    for c in pattern.chars() {
        if c == '*' || c == '?' {
            if !run.is_empty() {
                runs.push(std::mem::take(&mut run));
            }
        } else {
            run.push(c);
        }
    }
    if !run.is_empty() {
        runs.push(run);
    }
    Some(runs)
}

/// Build a selective FTS5 trigram filter from fixed literal runs. Very common
/// trigrams can match most of a large filesystem and make an AND query slower
/// than the original GLOB. Three trailing grams from the longest fixed run
/// usually discriminate well; the original GLOB remains the exact filter.
fn trigram_match_query(runs: &[String]) -> Option<String> {
    let run = runs.iter().max_by_key(|run| run.chars().count())?;
    let chars: Vec<char> = run.chars().collect();
    let mut terms = std::collections::BTreeSet::new();
    for window in chars.windows(3).rev().take(3) {
        terms.insert(window.iter().collect::<String>());
    }
    if terms.is_empty() {
        return None;
    }
    Some(
        terms
            .into_iter()
            .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND "),
    )
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
    let Some((dev, inode)) = crate::store::indexed_path(&store.conn, &requested)? else {
        anyhow::bail!(
            "{} is not in the index — wait for the daemon or run `dux scan`",
            path.display()
        );
    };
    // Identity resolution above compares raw path-component bytes. Lossy UTF-8
    // is used only for the user-facing path field, never to find the inode.
    inode_row(store, dev, inode, requested.to_string_lossy().into_owned())
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
            path: pr.resolve_display(dev, inode)?,
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

    let mut fts_runs = Vec::new();
    let mut patterns = Vec::new();
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
        if let Some(runs) = glob_literal_runs(&pat) {
            fts_runs.extend(runs);
        }
        patterns.push(pat);
    }
    if let Some(e) = &o.ext {
        // an extension is LITERAL — escape GLOB metacharacters so `--ext c++` or
        // `--ext d]` match the real extension instead of being treated as a glob.
        let e = glob_escape(e.trim_start_matches('.'));
        let pat = format!("*.{e}");
        if let Some(runs) = glob_literal_runs(&pat) {
            fts_runs.extend(runs);
        }
        patterns.push(pat);
    }
    let match_query = trigram_match_query(&fts_runs);
    let has_trigram_filter = match_query.is_some();
    if let Some(match_query) = match_query {
        sql.push_str(" AND d.rowid IN (SELECT rowid FROM names_fts WHERE names_fts MATCH ?)");
        args.push(Box::new(match_query));
    }
    for pat in patterns {
        if has_trigram_filter {
            sql.push_str(" AND CAST(d.name AS TEXT) GLOB ?");
        } else {
            // Keep FTS5's optimized GLOB path for short patterns that contain
            // no complete trigram (and for patterns with character classes).
            sql.push_str(" AND d.rowid IN (SELECT rowid FROM names_fts WHERE name GLOB ?)");
        }
        args.push(Box::new(pat));
    }
    if let Some(t) = o.newer_than {
        let cutoff = now_secs().saturating_sub(t);
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
            crate::util::display_name(&name),
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
            let prefix = pr.resolve_display(pdev, pino)?;
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
    let cutoff = (now_secs()
        .saturating_sub(since_secs)
        .saturating_add(crate::store::GROWTH_BUCKET_SECS - 1))
        / crate::store::GROWTH_BUCKET_SECS;
    // A single bucket has at most one row per current inode incarnation. Use
    // the covering bucket/delta index to
    // stop after the requested top rows instead of grouping and sorting every
    // changed inode. Multi-bucket windows still aggregate exactly as before.
    let (oldest_bucket, single_bucket) = if scope.is_none() {
        let first: Option<i64> = store
            .conn
            .query_row(
                "SELECT bucket FROM growth_current WHERE bucket>=?1 ORDER BY bucket LIMIT 1",
                params![cutoff],
                |row| row.get(0),
            )
            .optional()?;
        let last: Option<i64> = store
            .conn
            .query_row(
                "SELECT bucket FROM growth_current WHERE bucket>=?1 ORDER BY bucket DESC LIMIT 1",
                params![cutoff],
                |row| row.get(0),
            )
            .optional()?;
        (first, first.filter(|bucket| Some(*bucket) == last))
    } else {
        (None, None)
    };
    let mut sql = String::new();
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(bucket) = single_bucket {
        sql.push_str(
            "SELECT dev_id,inode,delta FROM growth_current
             WHERE bucket=? AND delta!=0 ORDER BY delta DESC LIMIT ?",
        );
        args.push(Box::new(bucket));
    } else if scope.is_none()
        && oldest_bucket.is_some_and(|bucket| bucket >= cutoff)
        && store
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='index' AND name='idx_growth_inode_bucket_delta'",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
    {
        // If the requested window covers the entire retained history, scan the
        // inode-leading covering index. Rows for each inode are adjacent, so
        // SQLite streams the SUM without building a GROUP BY temp B-tree. For a
        // narrower window the bucket-leading index remains more selective.
        sql.push_str(
            "SELECT dev_id,inode,SUM(delta) AS d FROM growth_current
             WHERE bucket >= ?
             GROUP BY dev_id,inode HAVING d!=0 ORDER BY d DESC LIMIT ?",
        );
        args.push(Box::new(cutoff));
    } else {
        sql.push_str(
            "SELECT dev_id,inode,SUM(delta) AS d FROM growth_current WHERE bucket >= ?",
        );
        args.push(Box::new(cutoff));
        if let Some((d, i)) = scope {
            // reuse the scope predicate against the growth rows
            sql.push_str(&scope_predicate());
            args.push(Box::new(d));
            args.push(Box::new(i));
        }
        sql.push_str(" GROUP BY dev_id,inode HAVING d!=0 ORDER BY d DESC LIMIT ?");
    }
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
            path: pr.resolve_display(dev, inode)?,
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
    let mut map: HashMap<Vec<u8>, (i64, i64)> = HashMap::new();
    for row in rows {
        let (name_bytes, size) = row?;
        let ext = match name_bytes.iter().rposition(|byte| *byte == b'.') {
            Some(dot) if dot + 1 < name_bytes.len() && name_bytes.len() - dot - 1 <= 16 => {
                let raw_ext = &name_bytes[dot + 1..];
                if let Ok(valid_ext) = std::str::from_utf8(raw_ext) {
                    valid_ext.to_lowercase().into_bytes()
                } else {
                    raw_ext.iter().map(u8::to_ascii_lowercase).collect()
                }
            }
            _ => b"(none)".to_vec(),
        };
        let e = map.entry(ext).or_insert((0, 0));
        e.0 =
            e.0.checked_add(size)
                .ok_or_else(|| anyhow::anyhow!("extension byte total exceeds i64 range"))?;
        e.1 =
            e.1.checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("extension file count exceeds i64 range"))?;
    }
    let mut v: Vec<ExtRow> = map
        .into_iter()
        .map(|(ext, (bytes, files))| ExtRow {
            ext: crate::util::display_name(&ext),
            bytes,
            files,
        })
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
    let cutoff = (now_secs()
        .saturating_sub(since_secs)
        .saturating_add(crate::store::GROWTH_BUCKET_SECS - 1))
        / crate::store::GROWTH_BUCKET_SECS;
    let mut sql =
        String::from("SELECT dev_id, inode, SUM(delta) AS d FROM growth_current WHERE bucket >= ?");
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(cutoff)];
    if let Some((d, i)) = scope {
        sql.push_str(&scope_predicate());
        args.push(Box::new(d));
        args.push(Box::new(i));
    }
    // Rank by magnitude without SQLite ABS(i64::MIN), which raises an integer
    // overflow error. The minimum signed value has an unrepresentable positive
    // magnitude, so clamp its sort key to i64::MAX while preserving `d`.
    sql.push_str(
        " GROUP BY dev_id, inode HAVING d != 0
         ORDER BY CASE WHEN d=(-9223372036854775807-1)
                       THEN 9223372036854775807 ELSE abs(d) END DESC LIMIT ?",
    );
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
            path: pr.resolve_display(dev, inode)?,
            delta,
        });
    }
    Ok(out)
}

/// (indexed node count, indexed bytes) — the root's recursive totals, or a
/// whole-table fallback when the root marker is missing. Shared by status/metrics.
pub fn index_totals(store: &Store) -> Result<(i64, i64)> {
    if let Some((d, i)) = index_root(store)? {
        return Ok(store.conn.query_row(
            "SELECT recursive_inodes, recursive_bytes FROM inodes WHERE dev_id=?1 AND inode=?2",
            params![d, i],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?);
    }
    // A never-scanned index is empty. A missing marker on a populated legacy
    // index permits the historical fallback; SQL/decoding errors do not.
    Ok(store.conn.query_row(
        "SELECT COUNT(*), COALESCE((SELECT MAX(recursive_bytes) FROM inodes WHERE kind='d'),0) FROM inodes",
        [], |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
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
        None => index_root(store)?,
    };
    let kind_filter = if all { "" } else { " AND i.kind='d'" };
    let cte = subtree_cte_depth(max_depth);
    // A bounded listing is small relative to the complete index. Drive it from
    // the pruned recursive CTE, then join each inode's canonical dirent in the
    // same SQLite statement. PathResolver used to execute a separate indexed
    // SELECT for every output row; at 10m scale, thousands of such random lookups
    // turned a shallow `du` into an I/O bound query. The correlated rowid lookup
    // preserves its prime-first path choice even if a damaged index has multiple
    // primary dirents. Defer path construction until all returned edges are
    // cached, so child rows never trigger lookups for ancestors.
    let chosen_dirent = "LEFT JOIN dirents d ON d.rowid=(
        SELECT de.rowid FROM dirents de
        WHERE de.dev_id=i.dev_id AND de.inode=i.inode
        ORDER BY de.prime DESC LIMIT 1
    )";
    let (sql, use_scope) = match root {
        Some(_) => {
            let relation = if max_depth.is_some() {
                // The depth-aware CTE carries depth to prune recursion. Collapse
                // repeated inode identities across aliases before the join, as
                // the former tuple-IN predicate did implicitly.
                format!("SELECT DISTINCT d,i FROM ({cte})")
            } else {
                cte
            };
            (
                format!(
                    "SELECT i.dev_id,i.inode,i.kind,i.recursive_bytes,i.blocks,i.mtime,
                            d.name,d.parent_dev,d.parent_inode
                     FROM ({relation}) scope
                     JOIN inodes i ON i.dev_id=scope.d AND i.inode=scope.i
                     {chosen_dirent}
                     WHERE 1=1{kind_filter}"
                ),
                true,
            )
        }
        // No root marker: a degenerate index (never scanned, or the markers were
        // lost). There is no root to measure depth from, so the walk can't be
        // pruned — fall back to the whole table and filter by path depth below.
        None => (
            format!(
                "SELECT i.dev_id,i.inode,i.kind,i.recursive_bytes,i.blocks,i.mtime,
                        d.name,d.parent_dev,d.parent_inode
                 FROM inodes i
                 {chosen_dirent}
                 WHERE 1=1{kind_filter}"
            ),
            false,
        ),
    };
    let mut stmt = store.conn.prepare(&sql)?;
    let mut pr = PathResolver::new(&store.conn);
    type DuEntry = (
        i64,
        i64,
        String,
        i64,
        i64,
        i64,
        Option<Vec<u8>>,
        Option<i64>,
        Option<i64>,
    );
    let map = |r: &rusqlite::Row| -> rusqlite::Result<DuEntry> {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get::<_, String>(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get(6)?,
            r.get(7)?,
            r.get(8)?,
        ))
    };
    let rows = if use_scope {
        let (d, i) = root.unwrap();
        stmt.query_map(params![d, i], map)?
    } else {
        stmt.query_map([], map)?
    };
    let mut pending = Vec::new();
    for row in rows {
        let (dev, inode, kind, rbytes, blocks, mtime, name, parent_dev, parent_inode) = row?;
        if let (Some(name), Some(parent_dev), Some(parent_inode)) = (name, parent_dev, parent_inode)
        {
            pr.remember(dev, inode, &name, parent_dev, parent_inode);
        }
        let is_dir = kind == "d";
        pending.push((
            dev,
            inode,
            if is_dir { rbytes } else { blocks },
            is_dir,
            mtime,
        ));
    }
    let mut out: Vec<DuRow> = pending
        .into_iter()
        .map(|(dev, inode, bytes, is_dir, mtime)| -> Result<DuRow> {
            Ok(DuRow {
                path: pr.resolve_display(dev, inode)?,
                bytes,
                is_dir,
                mtime,
            })
        })
        .collect::<Result<_>>()?;
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
pub fn index_root(store: &Store) -> Result<Option<(i64, i64)>> {
    let d = store
        .get_meta("root_dev")?
        .map(|value| value.parse::<i64>().context("invalid root_dev metadata"))
        .transpose()?;
    let i = store
        .get_meta("root_inode")?
        .map(|value| value.parse::<i64>().context("invalid root_inode metadata"))
        .transpose()?;
    anyhow::ensure!(d.is_some() == i.is_some(), "incomplete index root metadata");
    Ok(d.zip(i))
}

/// Sum of allocated bytes of the subtree rooted at (dev,inode) — used to size a
/// container's writable layer / volume from paths, without re-walking the fs.
pub fn subtree_bytes(store: &Store, dev: i64, inode: i64) -> Result<i64> {
    match store.conn.query_row(
        "SELECT recursive_bytes FROM inodes WHERE dev_id=?1 AND inode=?2",
        params![dev, inode],
        |r| r.get(0),
    ) {
        Ok(bytes) => Ok(bytes),
        // A path can disappear between its filesystem stat and its index
        // lookup. A missing row means unknown/not indexed; storage failures and
        // malformed rows must remain visible to the caller.
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
        Err(error) => Err(error.into()),
    }
}

/// Index status summary.
pub fn status(store: &Store, db: &Path) -> Result<String> {
    let root_path = crate::store::indexed_root_path(&store.conn)?;
    let root = root_path
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ts = optional_timestamp(store, "last_scan_ts")?.unwrap_or(0);
    let (count, total): (i64, i64) = match index_root(store)? {
        Some((dev, inode)) => store.conn.query_row(
            "SELECT recursive_inodes, recursive_bytes FROM inodes WHERE dev_id=?1 AND inode=?2",
            params![dev, inode],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?,
        _ => {
            let count = store
                .conn
                .query_row("SELECT COUNT(*) FROM inodes", [], |r| r.get(0))?;
            let total = store.conn.query_row(
                "SELECT COALESCE(MAX(recursive_bytes),0) FROM inodes WHERE kind='d'",
                [],
                |r| r.get(0),
            )?;
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
    if let Some(fs) = root_path.as_deref().and_then(crate::util::fs_stat) {
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
    let dbsz = i64::try_from(file_len_or_zero(db)?)
        .context("database file size does not fit status representation")?;
    let wal = Path::new(&format!("{}-wal", db.display())).to_path_buf();
    let walsz = i64::try_from(file_len_or_zero(&wal)?)
        .context("WAL file size does not fit status representation")?;
    let psize: i64 = store.conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    let free: i64 = store
        .conn
        .query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
    anyhow::ensure!(
        psize > 0 && free >= 0,
        "invalid SQLite page/free-page metrics"
    );
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
        let watch = crate::util::read_watch_status(db);
        if watch.as_ref().is_some_and(|status| status.scan_first) {
            out.push_str("daemon:     live (scan-first observations; fanotify hints)");
            if watch.as_ref().is_some_and(|status| status.uncertain)
                || store.get_meta("dirty_since")?.is_some()
            {
                out.push_str(
                    "\nstate:      last-observed view is stale; reconciliation is scheduled",
                );
            }
            if let Some(w) = watch {
                out.push_str(&format!(
                    "\nhints:      {} events; {} kernel overflows; latest view is not real-time",
                    w.events_seen,
                    w.kernel_overflow_notifications.unwrap_or(0),
                ));
            }
        } else {
            out.push_str("daemon:     live (tracks create/delete/rename/growth)");
            if let Some(w) = watch {
                if w.uncertain && store.get_meta("dirty_since")?.is_none() {
                    out.push_str("\nstate:      DIRTY — event loss or incomplete reconciliation; volatile health remains uncertain");
                }
                let resolve_pct = if w.events_seen == 0 {
                    100.0
                } else {
                    w.events_resolved as f64 * 100.0 / w.events_seen as f64
                };
                let queue_state = if w.kernel_empty {
                    "kernel queue empty"
                } else {
                    "kernel queue has events"
                };
                let last_commit = if w.last_commit_at == 0 {
                    "never".to_string()
                } else {
                    format!("{} ago", crate::util::ago(w.last_commit_at))
                };
                let overflow_note = match w.kernel_overflow_notifications {
                    Some(overflows) if overflows > 0 => {
                        format!("; {} kernel queue overflow notifications", overflows)
                    }
                    None => "; kernel overflow count unavailable for legacy watcher status".into(),
                    _ => String::new(),
                };
                out.push_str(&format!(
                "\nevents:     {queue_state}; {} pending updates / {} cap; {} seen ({resolve_pct:.1}% resolved); \
                 {} updates committed; {} userspace updates dropped; last commit {last_commit}; {}ms since caught up{}",
                w.pending,
                w.max_pending,
                w.events_seen,
                w.updates_committed,
                w.updates_dropped,
                w.behind_ms,
                overflow_note,
            ));
            }
        }
    } else {
        out.push_str("daemon:     not running — index is a static snapshot (run `dux daemon /`)");
    }
    // Throttled/behind: the CPU/IO governor is deliberately holding the daemon back
    // under sustained event load so it never disturbs the host. The index is LIVE
    // but intentionally stale — explain why, and how stale, so the numbers aren't
    // mistaken for real-time. Self-clears when the daemon catches up.
    if let Some(since) = throttled_since(store)? {
        out.push_str(&format!(
            "\nstate:      BEHIND — {} since the pipeline was last caught up. Check the pause reason, \
             pending updates, and flush errors below; this is not the age of every indexed file.",
            crate::util::ago(since)
        ));
    }
    if let Some(error) = store.get_meta("flush_error")? {
        out.push_str(&format!(
            "\nwrite error: {error} — pending changes retained for retry"
        ));
    }
    // Transient pause (self-clearing): the resource guardian paused writes because
    // the host is under pressure. Nothing is lost (pending is kept) — distinct from
    // DIRTY; it resumes automatically when the host recovers.
    if let Some(since) = paused_since(store)? {
        let reason = store
            .get_meta("pause_reason")?
            .unwrap_or_else(|| "system pressure".into());
        out.push_str(&format!(
            "\nstate:      WRITES PAUSED ({reason}) since {} — resumes when the host recovers",
            crate::util::ago(since)
        ));
    }
    let retry_state = store.get_meta("failed_reconciliation_attempts")?;
    let retry_deadline = store.get_meta("reconciliation_retry_after_unix")?;
    let (failed_reconciliation_attempts, automatic_reconciliation_paused) =
        crate::watch::reconciliation_retry_status(retry_state.as_deref());
    if automatic_reconciliation_paused == Some(true) {
        if let Some(attempts) = failed_reconciliation_attempts {
            let interval =
                crate::watch::reconciliation_retry_backoff_seconds(retry_state.as_deref())
                    .unwrap_or_default();
            let remaining = crate::watch::reconciliation_retry_remaining_seconds(
                retry_deadline.as_deref(),
                crate::util::now_secs(),
            );
            let wait_state = match remaining {
                Some(seconds) if seconds > 0 => format!("next retry in {seconds}s"),
                Some(_) => "retry is due; waiting for host/queue gates".to_string(),
                None => {
                    "retry deadline unavailable; bounded automatic retry is scheduled".to_string()
                }
            };
            out.push_str(&format!(
                "\nreconciliation: automatic scans in backoff after {attempts} failed full scans ({interval}s interval); {wait_state}; use `dux scan <root>` to request sooner"
            ));
        } else {
            out.push_str(
                "\nreconciliation: retry metadata is corrupt; automatic recovery uses maximum backoff; inspect the cause and use `dux scan <root>` to request sooner",
            );
        }
    }
    // Known event loss (fanotify overflow / dropped backlog / partial watch) makes
    // the index untrustworthy even while "live"; surface it and recommend a rescan.
    if let Some(since) = dirty_since(store)? {
        out.push_str(&format!(
            "\nstate:      DIRTY since {} — missed events; rescan with `dux scan <root>`",
            crate::util::ago(since)
        ));
    }
    // A scan running RIGHT NOW (initial build or rescan) publishes live progress —
    // surface it so the user knows to wait rather than trusting a stale snapshot.
    if let Some(line) = scan_progress_line(db) {
        out.push('\n');
        out.push_str(&line);
    }
    Ok(out)
}

/// Stable machine-readable status for monitoring and support tooling.
pub fn status_json(store: &Store, db: &Path) -> Result<serde_json::Value> {
    let root = crate::store::indexed_root_path(&store.conn)?
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    let last_scan_at = optional_timestamp(store, "last_scan_ts")?.unwrap_or(0);
    let dirty_since = dirty_since(store)?;
    let paused_since = paused_since(store)?;
    let throttled_since = throttled_since(store)?;
    let write_error = store.get_meta("flush_error")?;
    let pause_reason = store.get_meta("pause_reason")?;
    let dirty_reason = store.get_meta("dirty_reason")?;
    let retry_state = store.get_meta("failed_reconciliation_attempts")?;
    let retry_deadline = store.get_meta("reconciliation_retry_after_unix")?;
    let (failed_reconciliation_attempts, automatic_reconciliation_paused) =
        crate::watch::reconciliation_retry_status(retry_state.as_deref());
    let reconciliation_retry_backoff_seconds =
        crate::watch::reconciliation_retry_backoff_seconds(retry_state.as_deref());
    let reconciliation_retry_remaining_seconds =
        crate::watch::reconciliation_retry_remaining_seconds(
            retry_deadline.as_deref(),
            crate::util::now_secs(),
        );
    let daemon_live = crate::util::daemon_live_for(db);
    let watch = crate::util::read_watch_status(db);
    let scan_progress = crate::util::read_scan_progress_for(db);
    let scan_in_progress = scan_progress.is_some();
    let scan_progress = scan_progress.map(|progress| {
        serde_json::json!({
            "started_at_seconds": progress.started,
            "files_seen": progress.files,
            "directories_seen": progress.dirs,
            "allocated_bytes_seen": progress.bytes,
            "indexing": progress.indexing,
        })
    });
    let dirty = dirty_since.is_some() || watch.as_ref().is_some_and(|w| w.uncertain);
    let paused = paused_since.is_some();
    let caught_up = daemon_live
        && watch
            .as_ref()
            .is_some_and(|w| w.kernel_empty && w.pending == 0 && !w.uncertain && !w.scan_first)
        && !dirty
        && !paused
        && !scan_in_progress;
    let (index_nodes, index_bytes) = index_totals(store)?;
    let index_bytes_on_disk = file_len_or_zero(db)?;
    let wal = Path::new(&format!("{}-wal", db.display())).to_path_buf();
    let wal_bytes = file_len_or_zero(&wal)?;
    Ok(serde_json::json!({
        "status_version": 1,
        "root": root,
        "last_scan_timestamp_seconds": last_scan_at,
        "last_scan_age_seconds": crate::util::now_secs().saturating_sub(last_scan_at).max(0),
        "index_nodes": index_nodes,
        "index_bytes": index_bytes,
        "index_bytes_on_disk": index_bytes_on_disk,
        "wal_bytes": wal_bytes,
        "daemon_live": daemon_live,
        "watch_status_available": watch.is_some(),
        "scan_first": watch.as_ref().is_some_and(|w| w.scan_first),
        "watch_status_updated_at_seconds": watch.as_ref().map(|w| w.updated_at),
        "last_event_commit_timestamp_seconds": watch.as_ref().map(|w| w.last_commit_at),
        "pending_updates": watch.as_ref().map(|w| w.pending),
        "max_pending_updates": watch.as_ref().map(|w| w.max_pending),
        "events_seen": watch.as_ref().map(|w| w.events_seen),
        "events_resolved": watch.as_ref().map(|w| w.events_resolved),
        "updates_committed": watch.as_ref().map(|w| w.updates_committed),
        "events_dropped": watch.as_ref().map(|w| w.updates_dropped),
        "userspace_updates_dropped": watch.as_ref().map(|w| w.updates_dropped),
        "kernel_overflow_notifications": watch
            .as_ref()
            .and_then(|w| w.kernel_overflow_notifications),
        "behind_seconds": watch.as_ref().map(|w| w.behind_ms as f64 / 1000.0),
        "dirty": dirty,
        "dirty_since_timestamp_seconds": dirty_since,
        "dirty_reason": dirty_reason,
        "paused": paused,
        "paused_since_timestamp_seconds": paused_since,
        "pause_reason": pause_reason,
        "automatic_reconciliation_paused": automatic_reconciliation_paused,
        "failed_reconciliation_attempts": failed_reconciliation_attempts,
        "reconciliation_retry_backoff_seconds": reconciliation_retry_backoff_seconds,
        "reconciliation_retry_remaining_seconds": reconciliation_retry_remaining_seconds,
        "reconciliation_retry_due": automatic_reconciliation_paused == Some(true)
            && failed_reconciliation_attempts.is_some()
            && reconciliation_retry_remaining_seconds == Some(0),
        "max_automatic_reconciliation_retries": crate::watch::MAX_AUTOMATIC_RECONCILIATION_RETRIES,
        "reconciliation_retry_state_corrupt": automatic_reconciliation_paused == Some(true)
            && failed_reconciliation_attempts.is_none(),
        "throttled_since_timestamp_seconds": throttled_since,
        "write_error": write_error,
        "scan_in_progress": scan_in_progress,
        "scan_progress": scan_progress,
        "caught_up": caught_up,
    }))
}

/// Format the live scan-progress file (`/run/dux/scan.progress`) into one human
/// line, or None if no scan is running. Shared by `status` (appended to the
/// report) and by the no-index-yet path in `main` — on a fresh box the first
/// scan hasn't produced a `dux.db` yet, so this is the ONLY way to show progress
/// before the index exists.
pub fn scan_progress_line(db: &Path) -> Option<String> {
    Some(format_scan_progress(&crate::util::read_scan_progress_for(
        db,
    )?))
}

/// Pure formatter for a [`crate::util::ScanProgress`] snapshot (no I/O), so the
/// wording is unit-testable without touching the live `/run/dux/scan.progress`.
fn format_scan_progress(p: &crate::util::ScanProgress) -> String {
    let elapsed = crate::util::now_secs().saturating_sub(p.started).max(0);
    let phase = if p.indexing {
        "building index"
    } else {
        "scanning"
    };
    format!(
        "scan:       IN PROGRESS ({phase}) — {} files, {} dirs, {} so far, {}s elapsed",
        p.files,
        p.dirs,
        human(p.bytes),
        elapsed,
    )
}

/// Epoch seconds the daemon paused writes for low disk (self-clearing), if set.
pub(crate) fn optional_timestamp(store: &Store, key: &str) -> Result<Option<i64>> {
    store
        .get_meta(key)?
        .map(|value| {
            value
                .parse::<i64>()
                .with_context(|| format!("invalid {key} timestamp in index metadata: {value:?}"))
        })
        .transpose()
}

/// File size for status metrics. A missing WAL is normal after checkpointing;
/// permission, path, and I/O errors must remain visible instead of appearing as
/// a zero-byte database or WAL.
fn file_len_or_zero(path: &Path) -> Result<u64> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error).with_context(|| format!("cannot stat {}", path.display())),
    }
}

pub fn paused_since(store: &Store) -> Result<Option<i64>> {
    optional_timestamp(store, "paused_since")
}

/// Epoch seconds the daemon fell behind under CPU/IO governing (self-clearing).
/// While set, the index is LIVE but intentionally stale to protect the host.
pub fn throttled_since(store: &Store) -> Result<Option<i64>> {
    optional_timestamp(store, "throttled_since")
}

/// Epoch seconds the index was marked dirty (fanotify overflow), if set.
pub fn dirty_since(store: &Store) -> Result<Option<i64>> {
    optional_timestamp(store, "dirty_since")
}

/// True when a watch daemon for THIS db has heart-beaten within the last 30s.
pub fn daemon_live(db: &Path) -> bool {
    crate::util::daemon_live_for(db)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn extension_byte_aggregation_reports_signed_overflow() {
        let store = Store {
            conn: rusqlite::Connection::open_in_memory().unwrap(),
            schema_version: crate::store::SCHEMA_VERSION,
        };
        store.conn.execute_batch(crate::store::SCHEMA_SQL).unwrap();
        store
            .conn
            .execute_batch(
                "INSERT INTO inodes(dev_id,inode,kind,blocks,uid,mtime)
                 VALUES(1,2,'f',9223372036854775807,0,0),(1,3,'f',1,0,0);
             INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
                 VALUES(1,1,X'612e62696e',1,2,1),(1,1,X'622e62696e',1,3,1);",
            )
            .unwrap();
        let error = match by_ext(&store, 20) {
            Ok(_) => panic!("overflowing extension total must return an error"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("extension byte total exceeds i64 range"));
    }

    #[test]
    fn scope_and_status_queries_do_not_turn_sql_or_root_errors_into_empty_results() {
        let store = Store {
            conn: rusqlite::Connection::open_in_memory().unwrap(),
            schema_version: crate::store::SCHEMA_VERSION,
        };
        store.conn.execute_batch(crate::store::SCHEMA_SQL).unwrap();
        store.set_meta("root_dev", "1").unwrap();
        store.set_meta("root_inode", "not-an-inode").unwrap();
        let root_error = du(&store, None, false, None).unwrap_err().to_string();
        assert!(root_error.contains("invalid root_inode metadata"));

        store.set_meta("root_inode", "1").unwrap();
        store
            .conn
            .execute(
                "INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,uid,mtime)
                 VALUES(1,1,'d',4096,4096,0,0)",
                [],
            )
            .unwrap();
        store.set_meta("last_scan_root", "/").unwrap();
        store.set_meta("last_scan_ts", "not-a-timestamp").unwrap();
        assert!(status(&store, Path::new("/nonexistent/dux.db"))
            .unwrap_err()
            .to_string()
            .contains("invalid last_scan_ts timestamp"));

        store.conn.execute_batch("DROP TABLE inodes").unwrap();
        let path = tmp("scope-sql-error");
        std::fs::create_dir_all(&path).unwrap();
        let error = resolve_scope(&store, &path).unwrap_err().to_string();
        assert!(error.contains("no such table: inodes"));
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn status_file_size_treats_only_missing_files_as_zero() {
        let path = tmp("file-size-parent");
        std::fs::write(&path, b"not a directory").unwrap();
        let missing = path.join("dux.db");
        let error = file_len_or_zero(&missing).unwrap_err().to_string();
        assert!(error.contains("cannot stat"));
        assert_eq!(
            file_len_or_zero(&tmp("file-that-does-not-exist")).unwrap(),
            0
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn status_timestamps_fail_visibly_instead_of_clearing_health_markers() {
        let store = Store {
            conn: rusqlite::Connection::open_in_memory().unwrap(),
            schema_version: crate::store::SCHEMA_VERSION,
        };
        store.conn.execute_batch(crate::store::SCHEMA_SQL).unwrap();
        assert_eq!(dirty_since(&store).unwrap(), None);
        assert_eq!(paused_since(&store).unwrap(), None);
        store.set_meta("root_dev", "1").unwrap();
        store.set_meta("root_inode", "1").unwrap();
        store
            .conn
            .execute(
                "INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,uid,mtime)
             VALUES(1,1,'d',4096,4096,0,0)",
                [],
            )
            .unwrap();

        store.set_meta("dirty_since", "not-a-timestamp").unwrap();
        let error = dirty_since(&store).unwrap_err().to_string();
        assert!(error.contains("invalid dirty_since timestamp"));
        let error = status_json(&store, Path::new("/nonexistent/dux.db"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid dirty_since timestamp"));

        store.set_meta("dirty_since", "1730000000").unwrap();
        store.set_meta("paused_since", "also-invalid").unwrap();
        assert_eq!(dirty_since(&store).unwrap(), Some(1_730_000_000));
        assert!(paused_since(&store).is_err());
    }

    #[test]
    fn index_totals_fail_visibly_on_broken_metadata_and_rows() {
        let store = Store {
            conn: rusqlite::Connection::open_in_memory().unwrap(),
            schema_version: crate::store::SCHEMA_VERSION,
        };
        assert!(index_totals(&store).is_err());
        store.conn.execute_batch(crate::store::SCHEMA_SQL).unwrap();
        assert_eq!(index_totals(&store).unwrap(), (0, 0));
        store.set_meta("root_dev", "1").unwrap();
        assert!(
            index_totals(&store).is_err(),
            "half a root identity is corrupt"
        );
        store.set_meta("root_inode", "bad").unwrap();
        assert!(index_totals(&store).is_err());
        store.set_meta("root_inode", "1").unwrap();
        assert!(index_totals(&store).is_err(), "marked root must exist");
        store.conn.execute_batch("INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime) VALUES(1,1,'d',4096,8192,2,0,0)").unwrap();
        assert_eq!(index_totals(&store).unwrap(), (2, 8192));
        store
            .conn
            .execute("UPDATE inodes SET recursive_bytes='bad'", [])
            .unwrap();
        assert!(
            index_totals(&store).is_err(),
            "bad totals cannot become zero"
        );
    }

    #[test]
    fn subtree_size_preserves_missing_row_semantics_and_propagates_sql_errors() {
        let store = Store {
            conn: rusqlite::Connection::open_in_memory().unwrap(),
            schema_version: crate::store::SCHEMA_VERSION,
        };
        assert!(subtree_bytes(&store, 1, 2).is_err());
        store.conn.execute_batch(crate::store::SCHEMA_SQL).unwrap();
        assert_eq!(subtree_bytes(&store, 1, 2).unwrap(), 0);
        store
            .conn
            .execute_batch(
                "INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,uid,mtime)
                 VALUES(1,2,'d',4096,8192,0,0)",
            )
            .unwrap();
        assert_eq!(subtree_bytes(&store, 1, 2).unwrap(), 8192);
        store
            .conn
            .execute(
                "UPDATE inodes SET recursive_bytes='malformed' WHERE dev_id=1 AND inode=2",
                [],
            )
            .unwrap();
        assert!(subtree_bytes(&store, 1, 2).is_err());
    }

    #[test]
    fn trigram_prefilter_uses_only_guaranteed_literal_runs() {
        assert_eq!(
            glob_literal_runs("*artifact-42?.txt").unwrap(),
            vec!["artifact-42".to_string(), ".txt".to_string()]
        );
        assert!(glob_literal_runs("pre[ab]post").is_none());
        assert!(trigram_match_query(&["ab".to_string()]).is_none());
        let query = trigram_match_query(&["artifact".to_string()]).unwrap();
        assert!(query.contains("\"act\""));
        assert!(query.contains("\"fac\""));
        assert!(query.contains("\"ifa\""));
        assert!(!query.contains("\"art\""));
        assert!(query.contains(" AND "));
    }

    // The scan-progress line must render sensibly during the FIRST scan (before an
    // index exists) — this is what `dux status` shows on a fresh box instead of a
    // hard "no such database" error.
    #[test]
    fn scan_progress_line_wording() {
        let p = crate::util::ScanProgress {
            started: now_secs() - 5,
            files: 1234,
            dirs: 56,
            bytes: 4096,
            indexing: false,
        };
        let s = format_scan_progress(&p);
        assert!(s.contains("IN PROGRESS (scanning)"), "{s}");
        assert!(s.contains("1234 files"), "{s}");
        assert!(s.contains("56 dirs"), "{s}");
        let p2 = crate::util::ScanProgress {
            indexing: true,
            ..p
        };
        assert!(
            format_scan_progress(&p2).contains("building index"),
            "indexing phase must say 'building index'"
        );
    }

    #[test]
    fn inspect_resolves_non_utf8_path_components_by_raw_bytes() {
        use std::os::unix::ffi::OsStringExt;

        let parent = tmp("inspect-nonutf8-parent");
        let _ = std::fs::remove_dir_all(&parent);
        std::fs::create_dir_all(&parent).unwrap();
        let root = parent.join(std::ffi::OsString::from_vec(b"root-\xff".to_vec()));
        let db = tmp("inspect-nonutf8-db");
        rm(&db);
        std::fs::create_dir_all(&root).unwrap();
        let name = std::ffi::OsString::from_vec(b"bad-\xff-name".to_vec());
        let file = root.join(name);
        std::fs::write(&file, b"payload").unwrap();
        crate::scan::rebuild_atomic(
            &db,
            &root,
            &crate::scan::ScanOptions {
                progress: false,
                ..Default::default()
            },
        )
        .unwrap();

        let store = Store::open_ro(&db).unwrap();
        let row = inspect(&store, &file).unwrap();
        assert_eq!(
            row.size,
            std::fs::symlink_metadata(&file).unwrap().blocks() as i64 * 512
        );
        assert_eq!(row.path, file.to_string_lossy());
        let status = status_json(&store, &db).unwrap();
        assert_eq!(status["root"], root.to_string_lossy().as_ref());

        drop(store);
        let _ = std::fs::remove_dir_all(&parent);
        rm(&db);
    }

    #[test]
    fn machine_status_separates_live_freshness_and_uncertainty() {
        let root = tmp("machine-status-root");
        let db = tmp("machine-status-db");
        let _ = std::fs::remove_dir_all(&root);
        for suffix in ["", "-wal", "-shm", ".new", ".new-wal", ".new-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("entry"), b"value").unwrap();
        crate::scan::rebuild_atomic(
            &db,
            &root,
            &crate::scan::ScanOptions {
                progress: false,
                ..Default::default()
            },
        )
        .unwrap();
        crate::util::write_heartbeat(&db);
        crate::util::write_watch_status_with_overflows(
            &db,
            0,
            true,
            10,
            10,
            1,
            crate::util::now_secs(),
            0,
            0,
            500_000,
            2,
            false,
        );
        let store = crate::store::Store::open_rw(&db).unwrap();
        let healthy = status_json(&store, &db).unwrap();
        assert_eq!(healthy["daemon_live"], true);
        assert_eq!(healthy["watch_status_available"], true);
        assert_eq!(healthy["caught_up"], true);
        assert_eq!(healthy["kernel_overflow_notifications"], 2);
        assert_eq!(healthy["userspace_updates_dropped"], 0);
        assert_eq!(
            healthy["automatic_reconciliation_paused"],
            serde_json::Value::Null
        );
        assert_eq!(
            healthy["failed_reconciliation_attempts"],
            serde_json::Value::Null
        );
        assert_eq!(healthy["max_automatic_reconciliation_retries"], 1);
        assert!(
            healthy["last_event_commit_timestamp_seconds"]
                .as_i64()
                .unwrap()
                > 0
        );

        // An empty kernel queue only describes fanotify delivery. It does not
        // mean the userspace pending set has been applied to SQLite.
        crate::util::write_watch_status_with_overflows(
            &db,
            3,
            true,
            10,
            10,
            1,
            crate::util::now_secs(),
            0,
            0,
            500_000,
            2,
            false,
        );
        let human_status = status(&store, &db).unwrap();
        assert!(human_status.contains("events:     kernel queue empty; 3 pending updates"));
        assert!(!human_status.contains("pipeline:   drained"));

        crate::util::write_watch_status_with_mode(
            &db,
            0,
            true,
            12,
            0,
            1,
            crate::util::now_secs(),
            0,
            0,
            500_000,
            0,
            false,
            true,
        );
        let scan_first_json = status_json(&store, &db).unwrap();
        assert_eq!(scan_first_json["scan_first"], true);
        assert_eq!(scan_first_json["caught_up"], false);
        let scan_first_status = status(&store, &db).unwrap();
        assert!(scan_first_status.contains("fanotify hints"));
        assert!(scan_first_status.contains("latest view is not real-time"));

        store
            .set_meta("failed_reconciliation_attempts", "1")
            .unwrap();
        let retry_available = status_json(&store, &db).unwrap();
        assert_eq!(retry_available["failed_reconciliation_attempts"], 1);
        assert_eq!(retry_available["automatic_reconciliation_paused"], false);
        assert_eq!(retry_available["reconciliation_retry_backoff_seconds"], 60);

        store
            .set_meta("failed_reconciliation_attempts", "2")
            .unwrap();
        store
            .set_meta(
                "reconciliation_retry_after_unix",
                &(crate::util::now_secs() + 90).to_string(),
            )
            .unwrap();
        let circuit_open = status_json(&store, &db).unwrap();
        assert_eq!(circuit_open["failed_reconciliation_attempts"], 2);
        assert_eq!(circuit_open["automatic_reconciliation_paused"], true);
        assert_eq!(circuit_open["reconciliation_retry_state_corrupt"], false);
        assert_eq!(circuit_open["reconciliation_retry_backoff_seconds"], 120);
        assert!(
            circuit_open["reconciliation_retry_remaining_seconds"] == 89
                || circuit_open["reconciliation_retry_remaining_seconds"] == 90
        );
        assert_eq!(circuit_open["reconciliation_retry_due"], false);
        let human_circuit_status = status(&store, &db).unwrap();
        assert!(
            human_circuit_status.contains("automatic scans in backoff after 2 failed full scans")
        );
        assert!(human_circuit_status.contains("120s interval"));
        assert!(human_circuit_status.contains("next retry in"));
        assert!(human_circuit_status.contains("dux scan <root>"));

        store
            .set_meta(
                "reconciliation_retry_after_unix",
                &(crate::util::now_secs() - 1).to_string(),
            )
            .unwrap();
        let retry_due = status_json(&store, &db).unwrap();
        assert_eq!(retry_due["reconciliation_retry_remaining_seconds"], 0);
        assert_eq!(retry_due["reconciliation_retry_due"], true);
        assert!(status(&store, &db)
            .unwrap()
            .contains("retry is due; waiting for host/queue gates"));

        store
            .set_meta("failed_reconciliation_attempts", "invalid")
            .unwrap();
        let corrupt_retry_state = status_json(&store, &db).unwrap();
        assert_eq!(
            corrupt_retry_state["failed_reconciliation_attempts"],
            serde_json::Value::Null
        );
        assert_eq!(corrupt_retry_state["automatic_reconciliation_paused"], true);
        assert_eq!(
            corrupt_retry_state["reconciliation_retry_state_corrupt"],
            true
        );
        assert_eq!(
            corrupt_retry_state["reconciliation_retry_backoff_seconds"],
            30_720
        );
        store
            .conn
            .execute(
                "DELETE FROM meta WHERE key='failed_reconciliation_attempts'",
                [],
            )
            .unwrap();

        let status_path = crate::util::runtime_path(&db, "watch.status");
        let canonical_db = db.canonicalize().unwrap_or_else(|_| db.clone());
        std::fs::write(
            &status_path,
            format!(
                "3 {} {} 0 1 10 10 1 0 0 500000 0 {}\n{}",
                crate::util::now_secs(),
                std::process::id(),
                crate::util::now_secs(),
                canonical_db.display()
            ),
        )
        .unwrap();
        let legacy = status_json(&store, &db).unwrap();
        assert_eq!(legacy["watch_status_available"], true);
        assert!(legacy["kernel_overflow_notifications"].is_null());

        store
            .set_meta("last_scan_ts", &i64::MIN.to_string())
            .unwrap();
        let extreme_timestamp = status_json(&store, &db).unwrap();
        assert_eq!(
            extreme_timestamp["last_scan_age_seconds"].as_i64(),
            Some(i64::MAX)
        );

        store
            .set_meta("dirty_since", &crate::util::now_secs().to_string())
            .unwrap();
        let uncertain = status_json(&store, &db).unwrap();
        assert_eq!(uncertain["dirty"], true);
        assert_eq!(uncertain["caught_up"], false);

        drop(store);
        let _ = std::fs::remove_file(crate::util::runtime_path(&db, "heartbeat"));
        let _ = std::fs::remove_file(crate::util::runtime_path(&db, "watch.status"));
        let _ = std::fs::remove_dir_all(&root);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("dux-query-{tag}-{}", std::process::id()))
    }

    fn rm(db: &std::path::Path) {
        for s in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{s}", db.display()));
        }
    }

    #[test]
    fn growth_single_bucket_fast_path_and_multi_bucket_aggregation_match() {
        let db = tmp("growth-buckets");
        rm(&db);
        let store = Store::create_fresh(&db).unwrap();
        store.conn.execute_batch(crate::store::INDEXES_SQL).unwrap();
        store
            .conn
            .execute_batch(
                "INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime,birth_ns)
                   VALUES(1,1,'d',0,80,3,0,0,100),(1,2,'f',30,30,1,0,0,200),(1,3,'f',50,50,1,0,0,300);
                 INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
                   VALUES(1,1,X'2f',1,1,1),(1,1,X'736d616c6c',1,2,1),(1,1,X'6c61726765',1,3,1);
                 INSERT INTO meta(key,value) VALUES('root_dev','1'),('root_inode','1');",
            )
            .unwrap();

        let bucket = crate::util::now_secs() / crate::store::GROWTH_BUCKET_SECS;
        store
            .conn
            .execute(
                "INSERT INTO growth(bucket,dev_id,inode,birth_ns,delta) VALUES(?1,1,2,200,30),(?1,1,3,300,50)",
                params![bucket],
            )
            .unwrap();
        let one_bucket = growth(&store, 3600, 10, None).unwrap();
        assert_eq!(
            one_bucket
                .iter()
                .map(|row| (row.path.as_str(), row.delta))
                .collect::<Vec<_>>(),
            vec![("/large", 50), ("/small", 30)]
        );

        store
            .conn
            .execute(
                "INSERT INTO growth(bucket,dev_id,inode,birth_ns,delta) VALUES(?1,1,2,200,80),(?1,1,3,300,-10)",
                params![bucket - 1],
            )
            .unwrap();
        let multiple_buckets = growth(&store, 3600, 10, None).unwrap();
        assert_eq!(
            multiple_buckets
                .iter()
                .map(|row| (row.path.as_str(), row.delta))
                .collect::<Vec<_>>(),
            vec![("/small", 110), ("/large", 40)]
        );

        // When the requested window covers all retained buckets, the inode-
        // ordered covering index must preserve the same exact aggregate. Once
        // older buckets fall outside the window, the selective bucket-leading
        // path must exclude them.
        store
            .conn
            .execute(
                "INSERT INTO growth(bucket,dev_id,inode,birth_ns,delta) VALUES(?1,1,2,200,999)",
                params![bucket - 30],
            )
            .unwrap();
        let recent = growth(&store, 3600, 10, None).unwrap();
        assert_eq!(
            recent
                .iter()
                .map(|row| (row.path.as_str(), row.delta))
                .collect::<Vec<_>>(),
            vec![("/small", 110), ("/large", 40)]
        );
        let full_history = growth(&store, 86_400, 10, None).unwrap();
        assert_eq!(
            full_history
                .iter()
                .map(|row| (row.path.as_str(), row.delta))
                .collect::<Vec<_>>(),
            vec![("/small", 1109), ("/large", 40)]
        );

        drop(store);
        rm(&db);
    }

    #[test]
    fn growth_history_for_reused_inode_is_filtered_by_birth_generation() {
        let db = tmp("growth-incarnation");
        rm(&db);
        let store = Store::create_fresh(&db).unwrap();
        store.conn.execute_batch(
            "INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime,birth_ns)
             VALUES(1,1,'d',0,8,2,0,0,10),(1,2,'f',8,8,1,0,0,200);
             INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
             VALUES(1,1,X'2f',1,1,1),(1,1,X'66',1,2,1);
             INSERT INTO meta(key,value) VALUES('root_dev','1'),('root_inode','1');",
        ).unwrap();
        let bucket = crate::util::now_secs() / crate::store::GROWTH_BUCKET_SECS;
        store
            .conn
            .execute(
                "INSERT INTO growth(bucket,dev_id,inode,birth_ns,delta) VALUES
             (?1,1,2,100,5000),(?1,1,2,200,30),(?1,1,2,0,9000)",
                params![bucket],
            )
            .unwrap();

        let rows = growth(&store, 3600, 10, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, "/f");
        assert_eq!(
            rows[0].delta, 30,
            "only the current incarnation is attributable"
        );

        drop(store);
        rm(&db);
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

    #[test]
    fn du_shallow_bulk_paths_preserve_many_canonical_siblings() {
        const CHILDREN: i64 = 5_000;
        let db = tmp("du-many-siblings");
        rm(&db);
        let mut store = Store::create_fresh(&db).unwrap();
        {
            let tx = store.conn.transaction().unwrap();
            tx.execute(
                "INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime)
                 VALUES(1,1,'d',4096,?1,?2,0,0)",
                params![4096 * (CHILDREN + 1), CHILDREN + 1],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
                 VALUES(1,1,X'2f',1,1,1)",
                [],
            )
            .unwrap();
            for inode in 2..=CHILDREN + 1 {
                let name = format!("child-{inode:05}");
                tx.execute(
                    "INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime)
                     VALUES(1,?1,'d',4096,4096,1,0,0)",
                    params![inode],
                )
                .unwrap();
                tx.execute(
                    "INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
                     VALUES(1,1,?1,1,?2,1)",
                    params![name.as_bytes(), inode],
                )
                .unwrap();
            }
            tx.execute(
                "INSERT INTO meta(key,value) VALUES('root_dev','1'),('root_inode','1')",
                [],
            )
            .unwrap();
            tx.commit().unwrap();
        }

        let rows = du(&store, None, false, Some(1)).unwrap();
        assert_eq!(rows.len(), CHILDREN as usize + 1);
        let paths: std::collections::HashSet<_> = rows.into_iter().map(|row| row.path).collect();
        assert!(paths.contains("/"));
        for inode in [2, CHILDREN / 2, CHILDREN + 1] {
            assert!(paths.contains(&format!("/child-{inode:05}")));
        }

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

    #[test]
    fn find_returns_blob_dirent_names_from_the_trigram_index() {
        let db = tmp("find-trigram-blob-name");
        rm(&db);
        let store = Store::create_fresh(&db).unwrap();
        store
            .conn
            .execute_batch(crate::store::FTS_TRIGGERS_SQL)
            .unwrap();
        store
            .conn
            .execute_batch(
                "INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime)
                   VALUES(1,1,'d',0,4096,2,0,0),(1,2,'f',4096,4096,1,0,0);
                 INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
                   VALUES(1,1,X'2f',1,1,1),(1,1,X'61727469666163742d303030303030343234322e747874',1,2,1);
                 INSERT INTO meta(key,value) VALUES('root_dev','1'),('root_inode','1');",
            )
            .unwrap();

        let rows = find(
            &store,
            &FindOpts {
                name: Some("artifact-0000004242".into()),
                ext: None,
                newer_than: None,
                larger_than: None,
                owner_uid: None,
                limit: 20,
                scope: None,
            },
        )
        .unwrap();
        assert_eq!(
            rows.len(),
            1,
            "literal substring should find the indexed BLOB name"
        );
        assert_eq!(rows[0].path, "/artifact-0000004242.txt");

        drop(store);
        rm(&db);
    }

    #[test]
    fn find_and_extension_reports_keep_non_utf8_names_distinct() {
        let store = Store {
            conn: rusqlite::Connection::open_in_memory().unwrap(),
            schema_version: crate::store::SCHEMA_VERSION,
        };
        store.conn.execute_batch(crate::store::SCHEMA_SQL).unwrap();
        store
            .conn
            .execute_batch(
                "INSERT INTO inodes(dev_id,inode,kind,blocks,uid,mtime)
                   VALUES(1,1,'d',0,0,0),(1,2,'f',10,0,0),(1,3,'f',11,0,0),
                         (1,4,'f',12,0,0),(1,5,'f',13,0,0);
                 INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
                   VALUES(1,1,X'2f',1,1,1),(1,1,X'612eff',1,2,1),
                         (1,1,X'622efe',1,3,1),(1,1,X'632eefbfbd',1,4,1),
                         (1,1,X'642e5c786666',1,5,1);",
            )
            .unwrap();

        let rows = find(
            &store,
            &FindOpts {
                name: None,
                ext: None,
                newer_than: None,
                larger_than: Some(1),
                owner_uid: None,
                limit: 20,
                scope: None,
            },
        )
        .unwrap();
        let paths: std::collections::HashSet<_> =
            rows.iter().map(|row| row.path.as_str()).collect();
        assert_eq!(paths.len(), 4);
        for path in [r"/a.\xff", r"/b.\xfe", "/c.�", r"/d.\\xff"] {
            assert!(
                paths.contains(path),
                "missing distinct display path {path:?}"
            );
        }

        let extensions: std::collections::HashSet<_> = by_ext(&store, 20)
            .unwrap()
            .into_iter()
            .map(|row| row.ext)
            .collect();
        assert_eq!(extensions.len(), 4);
        for extension in [r"\xff", r"\xfe", "�", r"\\xff"] {
            assert!(
                extensions.contains(extension),
                "missing extension {extension:?}"
            );
        }
    }

    #[test]
    fn find_trigram_filter_preserves_case_extension_and_hardlink_paths() {
        let db = tmp("find-trigram-semantics");
        rm(&db);
        let store = Store::create_fresh(&db).unwrap();
        store
            .conn
            .execute_batch(crate::store::FTS_TRIGGERS_SQL)
            .unwrap();
        store
            .conn
            .execute_batch(
                "INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime)
                   VALUES(1,1,'d',0,16384,4,0,0),(1,2,'f',4096,4096,1,0,0),
                         (1,3,'d',0,8192,2,0,0),(1,4,'f',4096,4096,1,0,0);
                 INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
                   VALUES(1,1,X'2f',1,1,1),(1,1,X'737562',1,3,1),
                         (1,1,X'61727469666163742d303030303030343234322e747874',1,2,1),
                         (1,3,X'61727469666163742d303030303030343234322e747874',1,2,1),
                         (1,1,X'41727469666163742d303030303030343234322e545854',1,2,0),
                         (1,1,X'7265706f72745b66696e616c5d2d6c6f6e672e747874',1,4,1);
                 INSERT INTO meta(key,value) VALUES('root_dev','1'),('root_inode','1');",
            )
            .unwrap();

        let rows = find(
            &store,
            &FindOpts {
                name: Some("artifact-0000004242".into()),
                ext: None,
                newer_than: None,
                larger_than: None,
                owner_uid: None,
                limit: 20,
                scope: None,
            },
        )
        .unwrap();
        let mut paths: Vec<_> = rows.into_iter().map(|row| row.path).collect();
        paths.sort();
        assert_eq!(
            paths,
            ["/artifact-0000004242.txt", "/sub/artifact-0000004242.txt",]
        );

        let rows = find(
            &store,
            &FindOpts {
                name: None,
                ext: Some("TXT".into()),
                newer_than: None,
                larger_than: None,
                owner_uid: None,
                limit: 20,
                scope: None,
            },
        )
        .unwrap();
        assert_eq!(rows.len(), 1, "GLOB matching remains case-sensitive");
        assert_eq!(rows[0].path, "/Artifact-0000004242.TXT");

        let rows = find(
            &store,
            &FindOpts {
                name: Some("report[final]".into()),
                ext: None,
                newer_than: None,
                larger_than: None,
                owner_uid: None,
                limit: 20,
                scope: None,
            },
        )
        .unwrap();
        assert_eq!(rows.len(), 1, "literal brackets remain escaped");
        assert_eq!(rows[0].path, "/report[final]-long.txt");

        drop(store);
        rm(&db);
    }

    #[test]
    fn subtree_cycle_deduplicates_inodes() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE dirents(parent_dev,parent_inode,dev_id,inode); INSERT INTO dirents VALUES(1,10,1,20),(1,20,1,10);").unwrap();
        let mut st = conn.prepare(SUBTREE_CTE).unwrap();
        let rows: Vec<(i64, i64)> = st
            .query_map(params![1, 10], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(rows.len(), 2);
    }
}

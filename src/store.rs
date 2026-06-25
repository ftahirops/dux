use anyhow::{Context, Result};
use rusqlite::{params, Connection, OpenFlags};
use std::collections::HashMap;
use std::path::Path;

/// Memoizing path resolver for bulk result sets. Caches (name, parent) lookups
/// and fully-resolved paths so shared ancestors aren't re-queried per row.
pub struct PathResolver<'a> {
    conn: &'a Connection,
    node: HashMap<(i64, i64), (String, i64, i64)>, // (dev,inode) -> (name, parent_dev, parent_inode)
    full: HashMap<(i64, i64), String>,             // (dev,inode) -> absolute path
}

impl<'a> PathResolver<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        PathResolver {
            conn,
            node: HashMap::new(),
            full: HashMap::new(),
        }
    }

    fn lookup(&mut self, dev: i64, inode: i64) -> Option<(String, i64, i64)> {
        if let Some(v) = self.node.get(&(dev, inode)) {
            return Some(v.clone());
        }
        // an inode may have several dirents (hardlinks); pick the prime one for a
        // stable canonical path. name is a BLOB — decode lossily for display.
        let v: Option<(String, i64, i64)> = self
            .conn
            .query_row(
                "SELECT name, parent_dev, parent_inode FROM dirents
                 WHERE dev_id=?1 AND inode=?2 ORDER BY prime DESC LIMIT 1",
                params![dev, inode],
                |r| {
                    let nb: Vec<u8> = r.get(0)?;
                    Ok((
                        String::from_utf8_lossy(&nb).into_owned(),
                        r.get(1)?,
                        r.get(2)?,
                    ))
                },
            )
            .ok();
        if let Some(ref t) = v {
            self.node.insert((dev, inode), t.clone());
        }
        v
    }

    pub fn resolve(&mut self, dev: i64, inode: i64) -> String {
        self.resolve_d(dev, inode, 0)
    }

    // depth-guarded so a corrupt parent CYCLE can't recurse until the stack
    // overflows (mirrors the 4096 guard in path_of and the SQL CTEs).
    fn resolve_d(&mut self, dev: i64, inode: i64, depth: u32) -> String {
        if depth > 4096 {
            return format!("inode:{inode}");
        }
        if let Some(p) = self.full.get(&(dev, inode)) {
            return p.clone();
        }
        let (name, pdev, pino) = match self.lookup(dev, inode) {
            Some(v) => v,
            None => return format!("inode:{inode}"),
        };
        // root: self-parent — name holds the absolute root path
        let path = if (pdev == dev && pino == inode) || pino == 0 {
            name
        } else {
            let prefix = self.resolve_d(pdev, pino, depth + 1);
            if prefix.ends_with('/') {
                format!("{prefix}{name}")
            } else {
                format!("{prefix}/{name}")
            }
        };
        self.full.insert((dev, inode), path.clone());
        path
    }
}

pub const SCHEMA_VERSION: i64 = 4;

/// Width of a growth history bucket, in seconds (5 minutes).
pub const GROWTH_BUCKET_SECS: i64 = 300;

/// Steady-state schema (v4). Inode metadata and path/directory-entry records are
/// SEPARATE: `inodes` holds one row per (dev,inode) with allocated blocks and (for
/// directories) recursive totals; `dirents` holds one row per name/path and points
/// at an inode. A hardlinked file therefore has ONE `inodes` row and SEVERAL
/// `dirents` rows — every valid path is represented, and the inode's blocks are
/// counted once (attributed to the single `prime` dirent). `name` is a BLOB so two
/// distinct non-UTF-8 filenames never collapse to the same key.
pub const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE IF NOT EXISTS inodes (
    dev_id           INTEGER NOT NULL,
    inode            INTEGER NOT NULL,
    kind             TEXT    NOT NULL,
    blocks           INTEGER NOT NULL,
    recursive_bytes  INTEGER NOT NULL DEFAULT 0,
    recursive_inodes INTEGER NOT NULL DEFAULT 1,
    uid              INTEGER NOT NULL,
    mtime            INTEGER NOT NULL,
    PRIMARY KEY (dev_id, inode)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS dirents (
    parent_dev   INTEGER NOT NULL,
    parent_inode INTEGER NOT NULL,
    name         BLOB    NOT NULL,
    dev_id       INTEGER NOT NULL,
    inode        INTEGER NOT NULL,
    prime        INTEGER NOT NULL DEFAULT 1,   -- 1 = carries the inode's block attribution
    UNIQUE (parent_dev, parent_inode, name)    -- one inode may occupy a given path
);

-- Growth history as fixed 5-minute buckets per inode (delta of allocated
-- blocks), not one row per event. A continuously-written file produces ~288
-- rows/day instead of tens of thousands; queries SUM over the bucket range.
CREATE TABLE IF NOT EXISTS growth (
    bucket  INTEGER NOT NULL,   -- epoch seconds / 300
    dev_id  INTEGER NOT NULL,
    inode   INTEGER NOT NULL,
    delta   INTEGER NOT NULL,
    PRIMARY KEY (bucket, dev_id, inode)
) WITHOUT ROWID;

-- External-content trigram FTS over EVERY path component (dirents.name), so a
-- search finds all hardlink names. rowid == dirents.rowid. Stores no second copy
-- of the names. GLOB/LIKE substring search is accelerated by the trigram index.
CREATE VIRTUAL TABLE IF NOT EXISTS names_fts USING fts5(
    name, content='dirents', tokenize='trigram', detail=none, columnsize=0
);
"#;

/// Triggers that keep the external-content FTS in sync with `dirents`. Created
/// AFTER a bulk scan load (so the load isn't slowed by per-row FTS writes — the
/// scan rebuilds the FTS in one pass instead), and always present for the daemon.
pub const FTS_TRIGGERS_SQL: &str = r#"
CREATE TRIGGER IF NOT EXISTS dirents_ai AFTER INSERT ON dirents BEGIN
    INSERT INTO names_fts(rowid, name) VALUES (new.rowid, new.name);
END;
CREATE TRIGGER IF NOT EXISTS dirents_ad AFTER DELETE ON dirents BEGIN
    INSERT INTO names_fts(names_fts, rowid, name) VALUES('delete', old.rowid, old.name);
END;
CREATE TRIGGER IF NOT EXISTS dirents_au AFTER UPDATE OF name ON dirents BEGIN
    INSERT INTO names_fts(names_fts, rowid, name) VALUES('delete', old.rowid, old.name);
    INSERT INTO names_fts(rowid, name) VALUES (new.rowid, new.name);
END;
"#;

/// Secondary indexes. Partial indexes keep them small: directory ranking only
/// indexes directories, file ranking only indexes non-directories. Created after
/// a bulk load for speed.
pub const INDEXES_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS idx_dirents_target ON dirents(dev_id, inode);
CREATE INDEX IF NOT EXISTS idx_dirents_parent ON dirents(parent_dev, parent_inode);
CREATE INDEX IF NOT EXISTS idx_inodes_lfiles  ON inodes(blocks DESC)           WHERE kind<>'d';
CREATE INDEX IF NOT EXISTS idx_inodes_ldirs   ON inodes(recursive_bytes DESC)  WHERE kind='d';
CREATE INDEX IF NOT EXISTS idx_inodes_linode  ON inodes(recursive_inodes DESC) WHERE kind='d';
"#;

pub struct Store {
    pub conn: Connection,
}

impl Store {
    /// Open (creating if needed) a writable index. Used by scan/daemon.
    pub fn open_rw(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        Self::pragmas(&conn)?;
        let s = Store { conn };
        s.migrate()?;
        Ok(s)
    }

    /// Open read-only for queries. Falls back to RW open if the DB needs creating.
    pub fn open_ro(path: &Path) -> Result<Self> {
        if !path.exists() {
            anyhow::bail!(
                "no index at {} — run `dux scan <PATH>` first",
                path.display()
            );
        }
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("open ro {}", path.display()))?;
        // WAL readers are fine read-only; set busy timeout so we don't fail under the daemon.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Store { conn })
    }

    fn pragmas(conn: &Connection) -> Result<()> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        conn.pragma_update(None, "cache_size", -64_000)?; // ~64MB page cache
                                                          // Cap the WAL: on checkpoint the file is truncated back to this size
                                                          // instead of growing without bound (a long-lived reader + PASSIVE
                                                          // checkpoints could otherwise let the -wal file balloon to many GB).
        conn.pragma_update(None, "journal_size_limit", 128 * 1024 * 1024)?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        Ok(())
    }

    fn migrate(&self) -> Result<()> {
        // NEVER silently wipe a populated index. The v2/v3 layout changes are
        // incompatible with older schemas, but dropping the data here (the daemon
        // opens rw on startup) could turn a full index into an empty one while
        // still reporting "live". Instead refuse loudly — the rebuild path is an
        // ATOMIC rescan (Store::create_fresh + rename), driven by `dux scan` and
        // by the daemon's own self-heal (see needs_rebuild + scan::rebuild_atomic).
        let ver: i64 = self
            .get_meta("schema_version")
            .ok()
            .flatten()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let has_inodes = self
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='inodes'",
                [],
                |_| Ok(()),
            )
            .is_ok();
        if has_inodes && ver != SCHEMA_VERSION {
            anyhow::bail!(
                "index schema is v{ver} but this dux build needs v{}; \
                 rebuild it with `dux scan <root>` (the daemon does this \
                 automatically on start). The existing index was left untouched.",
                SCHEMA_VERSION
            );
        }
        self.conn.execute_batch(SCHEMA_SQL)?;
        self.conn.execute_batch(INDEXES_SQL)?;
        self.conn.execute_batch(FTS_TRIGGERS_SQL)?;
        self.set_meta("schema_version", &SCHEMA_VERSION.to_string())?;
        Ok(())
    }

    /// True if `db` is missing, has no `nodes` table, or carries a schema version
    /// other than the current build's — i.e. it must be atomically rebuilt before
    /// the daemon opens it rw (so an upgrade never yields an empty/wrong index).
    pub fn needs_rebuild(db: &Path) -> bool {
        if !db.exists() {
            return true;
        }
        let conn = match Connection::open_with_flags(
            db,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            Ok(c) => c,
            Err(_) => return true,
        };
        let has_inodes = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='inodes'",
                [],
                |_| Ok(()),
            )
            .is_ok();
        if !has_inodes {
            return true;
        }
        let ver: i64 = conn
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        ver != SCHEMA_VERSION
    }

    /// Create a fresh, empty index with ONLY the table + FTS vtable (no triggers,
    /// no secondary indexes). Used by an atomic rescan to bulk-load into a new
    /// file before `finalize_bulk` adds the FTS rebuild, triggers and indexes.
    pub fn create_fresh(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path).with_context(|| format!("create {}", path.display()))?;
        Self::pragmas(&conn)?;
        conn.execute_batch(SCHEMA_SQL)?;
        let s = Store { conn };
        s.set_meta("schema_version", &SCHEMA_VERSION.to_string())?;
        Ok(s)
    }

    /// After a bulk `nodes` load: rebuild the FTS from content in one pass, then
    /// install the sync triggers and secondary indexes. Order matters — building
    /// FTS and indexes after the load is far faster than maintaining them per row.
    pub fn finalize_bulk(&self) -> Result<()> {
        self.conn
            .execute_batch("INSERT INTO names_fts(names_fts) VALUES('rebuild');")?;
        self.conn.execute_batch(FTS_TRIGGERS_SQL)?;
        self.conn.execute_batch(INDEXES_SQL)?;
        Ok(())
    }

    /// Rebuild the trigram name index from the current `nodes` table.
    #[allow(dead_code)]
    pub fn rebuild_fts(&self) -> Result<()> {
        self.conn
            .execute_batch("INSERT INTO names_fts(names_fts) VALUES('rebuild');")?;
        Ok(())
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta(key,value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        let v = self
            .conn
            .query_row("SELECT value FROM meta WHERE key=?1", params![key], |r| {
                r.get::<_, String>(0)
            })
            .ok();
        Ok(v)
    }

    /// Reconstruct an absolute path for a node by walking parents (across mounts).
    pub fn path_of(&self, dev_id: i64, inode: i64) -> Result<String> {
        let mut parts: Vec<String> = Vec::new();
        let mut cur_dev = dev_id;
        let mut cur = inode;
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > 4096 {
                break; // cycle guard
            }
            let row: Option<(String, i64, i64)> = self
                .conn
                .query_row(
                    "SELECT name, parent_dev, parent_inode FROM dirents
                     WHERE dev_id=?1 AND inode=?2 ORDER BY prime DESC LIMIT 1",
                    params![cur_dev, cur],
                    |r| {
                        let nb: Vec<u8> = r.get(0)?;
                        Ok((
                            String::from_utf8_lossy(&nb).into_owned(),
                            r.get(1)?,
                            r.get(2)?,
                        ))
                    },
                )
                .ok();
            match row {
                Some((name, pdev, pino)) => {
                    if (pdev == cur_dev && pino == cur) || pino == 0 {
                        // root node — name holds the mount/scan root path
                        parts.push(name);
                        break;
                    }
                    parts.push(name);
                    cur_dev = pdev;
                    cur = pino;
                }
                None => break,
            }
        }
        parts.reverse();
        if parts.len() == 1 {
            return Ok(parts.remove(0));
        }
        // first element is the absolute root path; join the rest
        let root = parts.remove(0);
        let rest = parts.join("/");
        if root.ends_with('/') {
            Ok(format!("{root}{rest}"))
        } else {
            Ok(format!("{root}/{rest}"))
        }
    }
}

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
        let v: Option<(String, i64, i64)> = self
            .conn
            .query_row(
                "SELECT name, parent_dev, parent_inode FROM nodes WHERE dev_id=?1 AND inode=?2",
                params![dev, inode],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .ok();
        if let Some(ref t) = v {
            self.node.insert((dev, inode), t.clone());
        }
        v
    }

    pub fn resolve(&mut self, dev: i64, inode: i64) -> String {
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
            let prefix = self.resolve(pdev, pino);
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

/// A node row as stored in the index.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Node {
    pub dev_id: i64,
    pub inode: i64,
    pub parent_dev: i64,
    pub parent_inode: i64,
    pub name: String,
    pub kind: char, // 'f' file, 'd' dir, 'l' symlink, 'o' other
    pub blocks: i64,
    pub recursive_bytes: i64,
    pub recursive_inodes: i64,
    pub uid: i64,
    pub mtime: i64,
}

pub const SCHEMA_VERSION: i64 = 3;

/// Width of a growth history bucket, in seconds (5 minutes).
pub const GROWTH_BUCKET_SECS: i64 = 300;

/// Steady-state schema (v2). `nodes` carries only what queries read; the name
/// search index is an EXTERNAL-CONTENT FTS5 over `nodes.name` (no second copy of
/// the names) kept in sync by triggers, and totals live in recursive_* columns.
pub const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE IF NOT EXISTS nodes (
    dev_id           INTEGER NOT NULL,
    inode            INTEGER NOT NULL,
    parent_dev       INTEGER NOT NULL DEFAULT 0,
    parent_inode     INTEGER NOT NULL,
    name             TEXT    NOT NULL,
    kind             TEXT    NOT NULL,
    blocks           INTEGER NOT NULL,
    recursive_bytes  INTEGER NOT NULL DEFAULT 0,
    recursive_inodes INTEGER NOT NULL DEFAULT 1,
    uid              INTEGER NOT NULL,
    mtime            INTEGER NOT NULL,
    PRIMARY KEY (dev_id, inode)
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

-- External-content trigram FTS: stores only the search index, not a copy of the
-- names (those live in nodes). ~62% smaller than a content-stored FTS. GLOB/LIKE
-- substring search is accelerated by the trigram index; rowid == nodes.rowid.
CREATE VIRTUAL TABLE IF NOT EXISTS names_fts USING fts5(
    name, content='nodes', tokenize='trigram', detail=none, columnsize=0
);
"#;

/// Triggers that keep the external-content FTS in sync with `nodes`. Created
/// AFTER a bulk scan load (so the load isn't slowed by per-row FTS writes — the
/// scan rebuilds the FTS in one pass instead), and always present for the daemon.
pub const FTS_TRIGGERS_SQL: &str = r#"
CREATE TRIGGER IF NOT EXISTS nodes_ai AFTER INSERT ON nodes BEGIN
    INSERT INTO names_fts(rowid, name) VALUES (new.rowid, new.name);
END;
CREATE TRIGGER IF NOT EXISTS nodes_ad AFTER DELETE ON nodes BEGIN
    INSERT INTO names_fts(names_fts, rowid, name) VALUES('delete', old.rowid, old.name);
END;
CREATE TRIGGER IF NOT EXISTS nodes_au AFTER UPDATE OF name ON nodes BEGIN
    INSERT INTO names_fts(names_fts, rowid, name) VALUES('delete', old.rowid, old.name);
    INSERT INTO names_fts(rowid, name) VALUES (new.rowid, new.name);
END;
"#;

/// Secondary indexes. Partial indexes keep them small: directory ranking only
/// indexes directories, file ranking only indexes non-directories. Created after
/// a bulk load for speed.
pub const INDEXES_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS idx_nodes_pparent ON nodes(parent_dev, parent_inode);
CREATE INDEX IF NOT EXISTS idx_nodes_lfiles  ON nodes(blocks DESC)           WHERE kind<>'d';
CREATE INDEX IF NOT EXISTS idx_nodes_ldirs   ON nodes(recursive_bytes DESC)  WHERE kind='d';
CREATE INDEX IF NOT EXISTS idx_nodes_linode  ON nodes(recursive_inodes DESC) WHERE kind='d';
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
        // The v2 schema (external-content FTS, slim rows) is incompatible with
        // the v1 layout (content-stored FTS, extra columns). If we find an older
        // DB, drop the data objects and recreate empty — a `dux scan` repopulates
        // (the install/deploy path forces one). Cheaper and safer than ALTERs.
        let ver: i64 = self
            .get_meta("schema_version")
            .ok()
            .flatten()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let has_nodes = self
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='nodes'",
                [],
                |_| Ok(()),
            )
            .is_ok();
        if has_nodes && ver < SCHEMA_VERSION {
            self.conn.execute_batch(
                "DROP TRIGGER IF EXISTS nodes_ai;
                 DROP TRIGGER IF EXISTS nodes_ad;
                 DROP TRIGGER IF EXISTS nodes_au;
                 DROP TABLE IF EXISTS names_fts;
                 DROP TABLE IF EXISTS nodes;
                 DROP TABLE IF EXISTS changes;
                 DROP TABLE IF EXISTS growth;",
            )?;
        }
        self.conn.execute_batch(SCHEMA_SQL)?;
        self.conn.execute_batch(INDEXES_SQL)?;
        self.conn.execute_batch(FTS_TRIGGERS_SQL)?;
        self.set_meta("schema_version", &SCHEMA_VERSION.to_string())?;
        Ok(())
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
                    "SELECT name, parent_dev, parent_inode FROM nodes WHERE dev_id=?1 AND inode=?2",
                    params![cur_dev, cur],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
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

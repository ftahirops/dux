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
    pub parent_inode: i64,
    pub name: String,
    pub kind: char, // 'f' file, 'd' dir, 'l' symlink, 'o' other
    pub size: i64,
    pub blocks: i64,
    pub recursive_bytes: i64,
    pub uid: i64,
    pub gid: i64,
    pub mode: i64,
    pub mtime: i64,
    pub last_seen: i64,
    pub deleted: bool,
}

pub const SCHEMA_VERSION: i64 = 1;

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
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        Ok(())
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS nodes (
                dev_id          INTEGER NOT NULL,
                inode           INTEGER NOT NULL,
                parent_dev      INTEGER NOT NULL DEFAULT 0,
                parent_inode    INTEGER NOT NULL,
                name            TEXT    NOT NULL,
                kind            TEXT    NOT NULL,
                size            INTEGER NOT NULL,
                blocks          INTEGER NOT NULL,
                recursive_bytes INTEGER NOT NULL DEFAULT 0,
                recursive_inodes INTEGER NOT NULL DEFAULT 1,
                uid             INTEGER NOT NULL,
                gid             INTEGER NOT NULL,
                mode            INTEGER NOT NULL,
                mtime           INTEGER NOT NULL,
                last_seen       INTEGER NOT NULL,
                deleted         INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (dev_id, inode)
            );

            CREATE INDEX IF NOT EXISTS idx_nodes_parent ON nodes(dev_id, parent_inode);
            -- children are looked up by (parent_dev, parent_inode) everywhere
            -- (TUI expand, scope CTE, daemon ancestor walk) — index it or those
            -- queries full-scan the whole table.
            CREATE INDEX IF NOT EXISTS idx_nodes_pparent ON nodes(parent_dev, parent_inode);
            CREATE INDEX IF NOT EXISTS idx_nodes_size   ON nodes(size DESC);
            CREATE INDEX IF NOT EXISTS idx_nodes_rsize  ON nodes(recursive_bytes DESC);
            CREATE INDEX IF NOT EXISTS idx_nodes_rinode ON nodes(recursive_inodes DESC);
            CREATE INDEX IF NOT EXISTS idx_nodes_mtime  ON nodes(mtime DESC);
            CREATE INDEX IF NOT EXISTS idx_nodes_name   ON nodes(name);
            CREATE INDEX IF NOT EXISTS idx_nodes_uid    ON nodes(uid);

            CREATE TABLE IF NOT EXISTS changes (
                ts          INTEGER NOT NULL,
                dev_id      INTEGER NOT NULL,
                inode       INTEGER NOT NULL,
                size_before INTEGER NOT NULL,
                size_after  INTEGER NOT NULL,
                delta       INTEGER NOT NULL,
                event_type  TEXT    NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_changes_ts ON changes(ts);

            -- Trigram FTS over names: accelerates substring/glob search to
            -- locate-class speed. Rebuilt after each scan.
            CREATE VIRTUAL TABLE IF NOT EXISTS names_fts
                USING fts5(name, dev UNINDEXED, ino UNINDEXED, tokenize='trigram');
            "#,
        )?;
        // best-effort upgrades for DBs created before these columns existed
        let _ = self.conn.execute(
            "ALTER TABLE nodes ADD COLUMN recursive_inodes INTEGER NOT NULL DEFAULT 1",
            [],
        );
        let _ = self.conn.execute(
            "ALTER TABLE nodes ADD COLUMN parent_dev INTEGER NOT NULL DEFAULT 0",
            [],
        );
        // FTS docid for this node's name row, so deletes are O(1) by rowid
        // instead of a full scan of the (UNINDEXED) names_fts.dev/ino columns.
        let _ = self
            .conn
            .execute("ALTER TABLE nodes ADD COLUMN fts_rowid INTEGER", []);
        self.set_meta("schema_version", &SCHEMA_VERSION.to_string())?;
        Ok(())
    }

    /// Rebuild the trigram name index from the current `nodes` table.
    /// (Scan now populates FTS inline; kept for manual reindex / recovery.)
    #[allow(dead_code)]
    pub fn rebuild_fts(&self) -> Result<()> {
        self.conn.execute_batch(
            "DELETE FROM names_fts;
             INSERT INTO names_fts(name, dev, ino)
                SELECT name, dev_id, inode FROM nodes WHERE deleted=0;",
        )?;
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

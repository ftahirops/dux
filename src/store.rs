use anyhow::{Context, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

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

    /// Seed a path edge already fetched as part of a bulk query. Bulk listings
    /// should join the chosen dirent once instead of issuing one SQLite lookup
    /// per returned inode; `resolve` can then reuse these edges for all paths.
    pub fn remember(
        &mut self,
        dev: i64,
        inode: i64,
        name: &[u8],
        parent_dev: i64,
        parent_inode: i64,
    ) {
        self.node.insert(
            (dev, inode),
            (
                String::from_utf8_lossy(name).into_owned(),
                parent_dev,
                parent_inode,
            ),
        );
    }

    fn lookup(&mut self, dev: i64, inode: i64) -> Result<Option<(String, i64, i64)>> {
        if let Some(v) = self.node.get(&(dev, inode)) {
            return Ok(Some(v.clone()));
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
            .optional()?;
        if let Some(ref t) = v {
            self.node.insert((dev, inode), t.clone());
        }
        Ok(v)
    }

    /// Resolve display paths without recursive stack growth. Missing objects
    /// (e.g. deleted history) retain an inode label; SQL failures and cycles
    /// must reach callers instead of masquerading as a successfully read path.
    pub fn resolve(&mut self, dev: i64, inode: i64) -> Result<String> {
        let mut current = (dev, inode);
        let mut chain = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut prefix = loop {
            if let Some(path) = self.full.get(&current) {
                break path.clone();
            }
            anyhow::ensure!(visited.insert(current), "cycle in indexed display path");
            anyhow::ensure!(
                visited.len() <= 4096,
                "indexed display path exceeds depth limit"
            );
            let Some((name, pdev, pino)) = self.lookup(current.0, current.1)? else {
                break format!("inode:{}", current.1);
            };
            if (pdev, pino) == current || pino == 0 {
                self.full.insert(current, name.clone());
                break name;
            }
            chain.push((current, name));
            current = (pdev, pino);
        };
        for (id, name) in chain.into_iter().rev() {
            if !prefix.ends_with('/') {
                prefix.push('/');
            }
            prefix.push_str(&name);
            self.full.insert(id, prefix.clone());
        }
        Ok(prefix)
    }
}

pub const SCHEMA_VERSION: i64 = 6;

/// Width of a growth history bucket, in seconds (5 minutes).
pub const GROWTH_BUCKET_SECS: i64 = 300;

/// Resolve a stored inode location without lossy UTF-8 conversion. Mutation
/// reconciliation must never stat a display-only replacement of a byte name.
pub fn indexed_inode_path(
    conn: &Connection,
    dev: i64,
    ino: i64,
) -> Result<Option<std::path::PathBuf>> {
    use std::os::unix::ffi::OsStringExt;
    let mut current = (dev, ino);
    let mut parts = Vec::new();
    let mut visited = std::collections::HashSet::new();
    for _ in 0..4096 {
        anyhow::ensure!(visited.insert(current), "cycle in indexed ancestor chain");
        let entry: Option<(i64,i64,Vec<u8>)> = conn.query_row(
            "SELECT parent_dev,parent_inode,name FROM dirents WHERE dev_id=?1 AND inode=?2 AND prime=1 LIMIT 1",
            params![current.0,current.1],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
        let Some((pd, pi, name)) = entry else {
            return Ok(None);
        };
        if (pd, pi) == current {
            let mut path = std::path::PathBuf::from(std::ffi::OsString::from_vec(name));
            for part in parts.into_iter().rev() {
                path.push(std::ffi::OsString::from_vec(part));
            }
            return Ok(Some(path));
        }
        parts.push(name);
        current = (pd, pi);
    }
    anyhow::bail!("indexed ancestor chain exceeds depth limit")
}

/// Steady-state schema (v6). Inode metadata and path/directory-entry records are
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
    birth_ns         INTEGER NOT NULL DEFAULT 0,
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

-- Growth history as fixed 5-minute buckets per inode incarnation (delta of allocated
-- blocks), not one row per event. A continuously-written file produces ~288
-- rows/day instead of tens of thousands; queries SUM over the bucket range.
CREATE TABLE IF NOT EXISTS growth (
    bucket  INTEGER NOT NULL,   -- epoch seconds / 300
    dev_id  INTEGER NOT NULL,
    inode   INTEGER NOT NULL,
    birth_ns INTEGER NOT NULL DEFAULT 0,
    delta   INTEGER NOT NULL,
    PRIMARY KEY (bucket, dev_id, inode, birth_ns)
) WITHOUT ROWID;

-- SQLite's binary integer addition may promote an overflowing sum to REAL,
-- even for an INTEGER-affinity column. Keep growth deltas strictly integral so
-- rebuild and migration UPSERTs cannot silently persist an out-of-range value.
CREATE TRIGGER IF NOT EXISTS growth_delta_integer_insert
BEFORE INSERT ON growth
WHEN typeof(NEW.delta) != 'integer'
BEGIN
    SELECT RAISE(ABORT, 'growth delta must be an SQLite integer');
END;
CREATE TRIGGER IF NOT EXISTS growth_delta_integer_update
BEFORE UPDATE OF delta ON growth
WHEN typeof(NEW.delta) != 'integer'
BEGIN
    SELECT RAISE(ABORT, 'growth delta must be an SQLite integer');
END;

-- Incarnation-safe growth visible to analytics. Generation-zero rows and rows
-- for a prior inode incarnation are retained only as uninterpreted history.
CREATE VIEW IF NOT EXISTS growth_current AS
SELECT g.bucket,g.dev_id,g.inode,g.birth_ns,g.delta
FROM growth g JOIN inodes i
  ON i.dev_id=g.dev_id AND i.inode=g.inode AND i.birth_ns=g.birth_ns
WHERE g.birth_ns!=0;

-- External-content trigram FTS over EVERY path component (dirents.name), so a
-- search finds all hardlink names. rowid == dirents.rowid. Stores no second copy
-- of the names. GLOB/LIKE substring search is accelerated by the trigram index.
CREATE VIRTUAL TABLE IF NOT EXISTS names_fts USING fts5(
    name, content='dirents', tokenize='trigram', detail=none, columnsize=0
);
"#;

/// Path-instance graph tables for v7 staging. The live schema remains v6 until
/// scanning, event updates, queries, and atomic publication all use this model.
pub const PATH_GRAPH_TABLES_SQL: &str = r#"
CREATE TABLE paths (
    path_id INTEGER PRIMARY KEY,
    parent_path_id INTEGER NOT NULL,
    name BLOB NOT NULL,
    dev_id INTEGER NOT NULL,
    inode INTEGER NOT NULL,
    birth_ns INTEGER NOT NULL DEFAULT 0,
    kind TEXT NOT NULL,
    mount_id INTEGER NOT NULL DEFAULT -1,
    prime INTEGER NOT NULL DEFAULT 0,
    cycle INTEGER NOT NULL DEFAULT 0,
    CHECK (path_id > 0),
    CHECK (parent_path_id >= -1),
    CHECK (prime IN (0,1)),
    CHECK (cycle IN (0,1)),
    UNIQUE(parent_path_id, name)
);
CREATE TABLE dir_totals (
    path_id INTEGER PRIMARY KEY REFERENCES paths(path_id) ON DELETE CASCADE,
    visible_bytes INTEGER NOT NULL,
    visible_entries INTEGER NOT NULL,
    unique_bytes INTEGER NOT NULL,
    unique_inodes INTEGER NOT NULL
);
"#;

pub const PATH_GRAPH_INDEXES_SQL: &str = r#"
CREATE UNIQUE INDEX paths_single_root ON paths(parent_path_id)
    WHERE parent_path_id = -1;
CREATE INDEX paths_target ON paths(dev_id,inode,birth_ns);
CREATE INDEX paths_parent ON paths(parent_path_id);
CREATE INDEX paths_prime ON paths(dev_id,inode,birth_ns,prime);
CREATE UNIQUE INDEX paths_one_prime ON paths(dev_id,inode,birth_ns) WHERE prime=1;
"#;

/// Resolve one v7 path graph row to its exact byte-preserving absolute path.
/// The root row is identified by `parent_path_id = -1` and stores the absolute
/// scan root in `name`. A cycle or overlong chain is an index integrity error.
#[cfg(test)]
pub fn path_graph_resolve(conn: &Connection, path_id: i64) -> Result<Option<std::path::PathBuf>> {
    use std::os::unix::ffi::OsStringExt;
    let mut current = path_id;
    let mut parts = Vec::new();
    let mut visited = std::collections::HashSet::new();
    for _ in 0..4096 {
        anyhow::ensure!(visited.insert(current), "cycle in path graph ancestors");
        let entry: Option<(i64, Vec<u8>)> = conn
            .query_row(
                "SELECT parent_path_id,name FROM paths WHERE path_id=?1",
                [current],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((parent, name)) = entry else {
            return Ok(None);
        };
        if parent == -1 {
            let mut path = std::path::PathBuf::from(std::ffi::OsString::from_vec(name));
            for part in parts.into_iter().rev() {
                path.push(std::ffi::OsString::from_vec(part));
            }
            return Ok(Some(path));
        }
        parts.push(name);
        current = parent;
    }
    anyhow::bail!("path graph ancestor chain exceeds depth limit")
}

/// Find the path-instance row for an absolute or root-relative path without
/// touching the filesystem. Names are compared as raw Unix bytes.
#[cfg(test)]
pub fn path_graph_lookup(conn: &Connection, path: &Path) -> Result<Option<i64>> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let root: Option<(i64, Vec<u8>)> = conn
        .query_row(
            "SELECT path_id,name FROM paths WHERE parent_path_id=-1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((mut current, root_bytes)) = root else {
        return Ok(None);
    };
    let root = std::path::PathBuf::from(std::ffi::OsString::from_vec(root_bytes));
    let relative = if path.is_absolute() {
        let Ok(relative) = path.strip_prefix(&root) else {
            return Ok(None);
        };
        relative
    } else {
        path
    };
    let mut stmt =
        conn.prepare_cached("SELECT path_id FROM paths WHERE parent_path_id=?1 AND name=?2")?;
    for component in relative.components() {
        match component {
            std::path::Component::CurDir | std::path::Component::RootDir => continue,
            std::path::Component::Normal(name) => match stmt
                .query_row(rusqlite::params![current, name.as_bytes()], |row| {
                    row.get(0)
                })
                .optional()?
            {
                Some(next) => current = next,
                None => return Ok(None),
            },
            std::path::Component::ParentDir | std::path::Component::Prefix(_) => return Ok(None),
        }
    }
    Ok(Some(current))
}

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
CREATE INDEX IF NOT EXISTS idx_growth_bucket_delta ON growth(bucket,delta DESC,dev_id,inode,birth_ns);
-- Global multi-bucket growth reports group by inode. This covering order lets
-- SQLite aggregate in one pass without a second GROUP BY temp B-tree. Queries
-- that exclude older history continue to use the bucket-leading index above.
CREATE INDEX IF NOT EXISTS idx_growth_inode_bucket_delta ON growth(dev_id,inode,birth_ns,bucket,delta);
"#;

pub struct Store {
    pub conn: Connection,
    /// Schema version validated on open; v4/v5 snapshots remain readable while
    /// an atomic v6 baseline is built.
    pub schema_version: i64,
}

/// Resolve a name from the index without touching mounts, automounters, or NFS.
/// Suitable for UI grouping and recovery when a deleted parent cannot be stat'd.
pub fn indexed_path(conn: &Connection, path: &Path) -> Result<Option<(i64, i64)>> {
    let Some(root) = indexed_root_path(conn)? else {
        return Ok(None);
    };
    let relative = match path.strip_prefix(&root) {
        Ok(p) => p,
        Err(_) => return Ok(None),
    };
    let root_dev: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key='root_dev'", [], |r| {
            r.get(0)
        })
        .optional()?;
    let root_inode: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key='root_inode'", [], |r| {
            r.get(0)
        })
        .optional()?;
    anyhow::ensure!(
        root_dev.is_some() == root_inode.is_some(),
        "incomplete index root metadata"
    );
    let (Some(root_dev), Some(root_inode)) = (root_dev, root_inode) else {
        return Ok(None);
    };
    let mut id = (
        root_dev
            .parse::<i64>()
            .context("invalid root_dev metadata")?,
        root_inode
            .parse::<i64>()
            .context("invalid root_inode metadata")?,
    );
    let mut stmt = conn.prepare_cached(
        "SELECT dev_id,inode FROM dirents WHERE parent_dev=?1 AND parent_inode=?2 AND name=?3",
    )?;
    for part in relative.components() {
        match part {
            std::path::Component::CurDir => continue,
            std::path::Component::Normal(name) => {
                match stmt.query_row(params![id.0, id.1, name.as_bytes()], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                }) {
                    Ok(next) => id = next,
                    Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
                    Err(e) => return Err(e.into()),
                }
            }
            _ => return Ok(None),
        }
    }
    Ok(Some(id))
}

/// Exact scan root as filesystem bytes. New indexes store a hex-encoded byte
/// representation; older indexes fall back to their UTF-8 metadata field.
pub fn indexed_root_path(conn: &Connection) -> Result<Option<std::path::PathBuf>> {
    let encoded: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key='last_scan_root_bytes'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(encoded) = encoded {
        anyhow::ensure!(
            encoded.len() % 2 == 0,
            "invalid last_scan_root_bytes metadata"
        );
        let mut bytes = Vec::with_capacity(encoded.len() / 2);
        for pair in encoded.as_bytes().as_chunks::<2>().0 {
            let digit = |byte: u8| -> Option<u8> {
                match byte {
                    b'0'..=b'9' => Some(byte - b'0'),
                    b'a'..=b'f' => Some(byte - b'a' + 10),
                    b'A'..=b'F' => Some(byte - b'A' + 10),
                    _ => None,
                }
            };
            let high = digit(pair[0]).context("invalid last_scan_root_bytes metadata")?;
            let low = digit(pair[1]).context("invalid last_scan_root_bytes metadata")?;
            bytes.push((high << 4) | low);
        }
        let path = std::path::PathBuf::from(OsString::from_vec(bytes));
        anyhow::ensure!(
            path.is_absolute() && !path.as_os_str().as_bytes().contains(&0),
            "invalid last_scan_root_bytes metadata"
        );
        return Ok(Some(path));
    }

    let legacy: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key='last_scan_root'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(legacy) = legacy else {
        return Ok(None);
    };
    let path = std::path::PathBuf::from(legacy);
    anyhow::ensure!(
        path.is_absolute(),
        "invalid last_scan_root metadata: expected an absolute path"
    );
    Ok(Some(path))
}

/// Store Unix path bytes in metadata's TEXT column without lossy conversion.
pub fn encode_path_bytes(path: &Path) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = path.as_os_str().as_bytes();
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

impl Store {
    /// Verify global materialized totals and every directory's recursive totals
    /// against its prime-attributed children in one snapshot. SQLite's
    /// integrity_check cannot detect logical rollup drift. Runs periodically,
    /// never on a keyboard or per-event path.
    pub fn totals_consistent(&self) -> Result<bool> {
        self.conn
            .query_row(
                "WITH child_totals AS (
                 SELECT d.parent_dev,d.parent_inode,
                        SUM(ch.recursive_bytes) AS bytes,
                        SUM(ch.recursive_inodes) AS items
                 FROM dirents d
                 JOIN inodes ch ON ch.dev_id=d.dev_id AND ch.inode=d.inode
                 WHERE d.prime=1
                   AND NOT (d.dev_id=d.parent_dev AND d.inode=d.parent_inode)
                 GROUP BY d.parent_dev,d.parent_inode
             ), inode_totals AS (
                 SELECT COALESCE(SUM(blocks),0) AS bytes,COUNT(*) AS items FROM inodes
             )
             SELECT EXISTS (
                 SELECT 1 FROM inodes r CROSS JOIN inode_totals totals
                 WHERE r.dev_id=CAST((SELECT value FROM meta WHERE key='root_dev') AS INTEGER)
                   AND r.inode=CAST((SELECT value FROM meta WHERE key='root_inode') AS INTEGER)
                   AND r.recursive_bytes=totals.bytes
                   AND r.recursive_inodes=totals.items
             ) AND NOT EXISTS (
                 SELECT 1 FROM dirents de
                 LEFT JOIN inodes child
                   ON child.dev_id=de.dev_id AND child.inode=de.inode
                 LEFT JOIN inodes parent
                   ON parent.dev_id=de.parent_dev AND parent.inode=de.parent_inode
                 WHERE child.inode IS NULL OR parent.kind IS NULL OR parent.kind!='d'
             ) AND NOT EXISTS (
                 SELECT 1 FROM inodes d
                 LEFT JOIN child_totals c
                   ON c.parent_dev=d.dev_id AND c.parent_inode=d.inode
                 WHERE d.kind='d'
                   AND (d.recursive_bytes != d.blocks+COALESCE(c.bytes,0)
                     OR d.recursive_inodes != 1+COALESCE(c.items,0))
             ) AND NOT EXISTS (
                 SELECT 1 FROM inodes leaf
                 WHERE leaf.kind!='d'
                   AND (leaf.recursive_bytes!=leaf.blocks OR leaf.recursive_inodes!=1)
             )",
                [],
                |r| r.get(0),
            )
            .context("checking indexed totals")
    }

    /// Open (creating if needed) a writable index. Used by scan/daemon.
    pub fn open_rw(path: &Path) -> Result<Self> {
        Self::has_index_artifacts(path)?;
        crate::util::ensure_parent_dir(path)?;
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        Self::pragmas(&conn)?;
        let mut s = Store {
            conn,
            schema_version: 0,
        };
        s.migrate()?;
        s.schema_version = SCHEMA_VERSION;
        s.conn.execute_batch(
            "CREATE TEMP VIEW growth_current AS
             SELECT g.bucket,g.dev_id,g.inode,g.birth_ns,g.delta
             FROM growth g JOIN inodes i
               ON i.dev_id=g.dev_id AND i.inode=g.inode AND i.birth_ns=g.birth_ns
             WHERE g.birth_ns!=0;",
        )?;
        Ok(s)
    }

    /// Open a known compatible schema read-only. Legacy snapshots remain
    /// navigable while the daemon builds its v6 baseline; ambiguous growth is
    /// suppressed by the connection-local `growth_current` view.
    pub fn open_ro(path: &Path) -> Result<Self> {
        Self::open_ro_versioned(path, true)
    }

    /// Known v4/v5 sources can be read while an atomic v6 baseline is built.
    pub fn open_rebuild_source(path: &Path) -> Result<Self> {
        Self::open_ro_versioned(path, true)
    }

    /// Whether a main index or one of its SQLite sidecars exists, including an
    /// empty main file created before the first schema transaction committed.
    pub fn has_index_artifacts(path: &Path) -> Result<bool> {
        let mut found = entry_metadata_if_exists(path)?.is_some();
        for candidate in sqlite_paths(path, &["-wal", "-shm", "-journal"]) {
            if let Some(metadata) = entry_metadata_if_exists(&candidate)? {
                anyhow::ensure!(
                    !metadata.file_type().is_symlink(),
                    "refusing symlinked SQLite sidecar {}",
                    candidate.display()
                );
                found = true;
            }
        }
        Ok(found)
    }

    /// Whether any file can contain committed/recoverable SQLite pages. A
    /// zero-byte main file is empty only if its WAL and rollback journal are
    /// also empty or absent.
    pub fn has_recoverable_database_state(path: &Path) -> Result<bool> {
        for candidate in sqlite_paths(path, &["", "-wal", "-journal"]) {
            if crate::util::metadata_if_exists(&candidate)?
                .is_some_and(|metadata| metadata.len() > 0)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn open_ro_versioned(path: &Path, allow_v4: bool) -> Result<Self> {
        if !Self::has_index_artifacts(path)? {
            // If the FIRST scan is building the index right now, say so (with live
            // progress) rather than "no index" — the user just needs to wait.
            if let Some(p) = crate::util::read_scan_progress_for(path) {
                let elapsed = crate::util::now_secs().saturating_sub(p.started).max(0);
                anyhow::bail!(
                    "initial scan in progress — {} files, {} dirs scanned, {}s elapsed. \
                     The index will be ready when it finishes; re-run then \
                     (watch with `dux status`).",
                    p.files,
                    p.dirs,
                    elapsed
                );
            }
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
        let ver = read_schema_version(&conn)?;
        if ver != SCHEMA_VERSION && !(allow_v4 && (4..SCHEMA_VERSION).contains(&ver)) {
            if ver > SCHEMA_VERSION {
                anyhow::bail!("index schema v{ver} is newer than this build (v{SCHEMA_VERSION}); install the matching newer executable or restore its matching database backup. Refusing a destructive downgrade.");
            }
            anyhow::bail!(
                "index schema is v{ver} but this dux build needs v{}; \
                 rebuild it with `dux scan <root>` (or start `dux daemon <root>` \
                 and let it rebuild automatically). The existing index was left untouched.",
                SCHEMA_VERSION
            );
        }
        // Legacy growth rows have no incarnation key and cannot be safely
        // assigned to the inode currently occupying the same number. Expose an
        // empty source for those snapshots until their atomic v6 rebuild. On v6,
        // only known birth times matching the current inode incarnation surface.
        if ver >= 6 {
            conn.execute_batch(
                "CREATE TEMP VIEW growth_current AS
                 SELECT g.bucket,g.dev_id,g.inode,g.birth_ns,g.delta
                 FROM growth g JOIN inodes i
                   ON i.dev_id=g.dev_id AND i.inode=g.inode AND i.birth_ns=g.birth_ns
                 WHERE g.birth_ns!=0;",
            )?;
        } else {
            conn.execute_batch(
                "CREATE TEMP VIEW growth_current AS
                 SELECT 0 AS bucket,0 AS dev_id,0 AS inode,0 AS birth_ns,0 AS delta WHERE 0;",
            )?;
        }
        Ok(Store {
            conn,
            schema_version: ver,
        })
    }

    fn pragmas(conn: &Connection) -> Result<()> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        // Approximately 64 MB page cache.
        conn.pragma_update(None, "cache_size", -64_000)?;
        // Limit retained WAL size when SQLite can reset it. This is not a hard
        // active-WAL cap: readers can pin older frames and prevent reset.
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
        let has_inodes = sqlite_table_exists(&self.conn, "inodes")?;
        let ver = if has_inodes {
            read_schema_version(&self.conn)?
        } else {
            0
        };
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
    pub fn needs_rebuild(db: &Path) -> Result<bool> {
        Self::has_index_artifacts(db)?;
        // Do not treat an empty main file as a new index if a committed WAL or
        // recovery journal still contains pages that SQLite must replay.
        if !Self::has_recoverable_database_state(db)? {
            return Ok(true);
        }
        // SQLite may discard an invalid sidecar while opening the database, so
        // remember its presence before opening and do not downgrade a sidecar
        // recovery failure into an ordinary missing-schema rebuild.
        let had_nonempty_recovery_sidecar = has_nonempty_recovery_sidecar(db)?;
        let conn = Connection::open_with_flags(
            db,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("open index {} to check schema", db.display()))?;
        let has_inodes = sqlite_table_exists(&conn, "inodes")?;
        if !has_inodes {
            if had_nonempty_recovery_sidecar {
                anyhow::bail!(
                    "index {} has a nonempty SQLite recovery sidecar but no readable inode schema; refusing to classify it as an empty rebuild source",
                    db.display()
                );
            }
            return Ok(true);
        }
        Ok(read_schema_version(&conn)? != SCHEMA_VERSION)
    }

    /// Create a fresh, empty index with ONLY the table + FTS vtable (no triggers,
    /// no secondary indexes). Used by an atomic rescan to bulk-load into a new
    /// file before `finalize_bulk` adds the FTS rebuild, triggers and indexes.
    pub fn create_fresh(path: &Path) -> Result<Self> {
        Self::has_index_artifacts(path)?;
        crate::util::ensure_parent_dir(path)?;
        let conn = Connection::open(path).with_context(|| format!("create {}", path.display()))?;
        Self::pragmas(&conn)?;
        conn.execute_batch(SCHEMA_SQL)?;
        let s = Store {
            conn,
            schema_version: SCHEMA_VERSION,
        };
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
            .optional()?;
        Ok(v)
    }

    /// Reconstruct an absolute path for a node by walking parents (across mounts).
    pub fn path_of(&self, dev_id: i64, inode: i64) -> Result<String> {
        PathResolver::new(&self.conn).resolve(dev_id, inode)
    }
}

fn sqlite_paths(path: &Path, suffixes: &[&str]) -> Vec<PathBuf> {
    suffixes
        .iter()
        .map(|suffix| {
            let mut candidate = path.as_os_str().to_owned();
            candidate.push(suffix);
            PathBuf::from(candidate)
        })
        .collect()
}

fn entry_metadata_if_exists(path: &Path) -> Result<Option<std::fs::Metadata>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("inspecting {}", path.display())),
    }
}

fn has_nonempty_recovery_sidecar(path: &Path) -> Result<bool> {
    for candidate in sqlite_paths(path, &["-wal", "-journal"]) {
        if crate::util::metadata_if_exists(&candidate)?.is_some_and(|metadata| metadata.len() > 0) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn sqlite_table_exists(conn: &Connection, name: &str) -> Result<bool> {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |_| Ok(()),
    )
    .optional()
    .map(|found| found.is_some())
    .context("checking SQLite table metadata")
}

fn read_schema_version(conn: &Connection) -> Result<i64> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()
        .context("reading index schema version")?;
    match value {
        Some(value) => value
            .parse()
            .with_context(|| format!("invalid index schema_version metadata: {value:?}")),
        None => Ok(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_path_graph_schema(conn: &Connection) {
        conn.execute_batch(PATH_GRAPH_TABLES_SQL).unwrap();
        conn.execute_batch(PATH_GRAPH_INDEXES_SQL).unwrap();
    }

    #[test]
    fn indexed_path_rejects_malformed_or_partial_root_metadata() {
        let conn = Connection::open_in_memory().unwrap();
        create_path_graph_schema(&conn);
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);")
            .unwrap();
        conn.execute(
            "INSERT INTO meta(key,value) VALUES('last_scan_root','/')",
            [],
        )
        .unwrap();
        assert_eq!(indexed_path(&conn, Path::new("/tmp")).unwrap(), None);

        conn.execute(
            "INSERT INTO meta(key,value) VALUES('root_dev','not-a-device')",
            [],
        )
        .unwrap();
        assert!(indexed_path(&conn, Path::new("/tmp"))
            .unwrap_err()
            .to_string()
            .contains("incomplete index root metadata"));
        conn.execute("INSERT INTO meta(key,value) VALUES('root_inode','1')", [])
            .unwrap();
        assert!(indexed_path(&conn, Path::new("/tmp"))
            .unwrap_err()
            .to_string()
            .contains("invalid root_dev metadata"));
    }

    #[test]
    fn indexed_root_path_rejects_empty_relative_and_nul_encoded_roots() {
        let conn = Connection::open_in_memory().unwrap();
        create_path_graph_schema(&conn);
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);")
            .unwrap();
        for encoded in ["", "72656c6174697665", "2f746d702f610062"] {
            conn.execute(
                "INSERT INTO meta(key,value) VALUES('last_scan_root_bytes',?1)",
                [encoded],
            )
            .unwrap();
            assert!(indexed_root_path(&conn).is_err(), "accepted {encoded:?}");
            conn.execute("DELETE FROM meta WHERE key='last_scan_root_bytes'", [])
                .unwrap();
        }
        conn.execute(
            "INSERT INTO meta(key,value) VALUES('last_scan_root','relative')",
            [],
        )
        .unwrap();
        assert!(indexed_root_path(&conn).is_err());
    }

    #[test]
    fn display_path_propagates_sql_and_row_errors() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(PathResolver::new(&conn).resolve(1, 2).is_err());
        conn.execute_batch(SCHEMA_SQL).unwrap();
        conn.execute_batch("INSERT INTO dirents VALUES(1,1,'text-not-blob',1,2,1)")
            .unwrap();
        assert!(PathResolver::new(&conn).resolve(1, 2).is_err());
        assert!(Store {
            conn,
            schema_version: SCHEMA_VERSION
        }
        .path_of(1, 2)
        .is_err());
    }

    #[test]
    fn display_path_rejects_cycles_and_resolves_deep_chains_iteratively() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA_SQL).unwrap();
        conn.execute_batch("INSERT INTO dirents VALUES(1,1,X'2f',1,1,1)")
            .unwrap();
        for inode in 2..=1024 {
            conn.execute(
                "INSERT INTO dirents VALUES(1,?1,X'61',1,?2,1)",
                params![inode - 1, inode],
            )
            .unwrap();
        }
        let mut resolver = PathResolver::new(&conn);
        let path = resolver.resolve(1, 1024).unwrap();
        assert!(path.starts_with("/a/a"));
        assert_eq!(path.matches('a').count(), 1023);
        assert_eq!(resolver.resolve(1, 1024).unwrap(), path);
        conn.execute("UPDATE dirents SET parent_inode=1024 WHERE inode=2", [])
            .unwrap();
        assert!(PathResolver::new(&conn)
            .resolve(1, 1024)
            .unwrap_err()
            .to_string()
            .contains("cycle"));
    }

    #[test]
    fn bundled_sqlite_includes_wal_reset_fix() {
        let conn = Connection::open_in_memory().unwrap();
        let (version, source): (String, String) = conn
            .query_row("SELECT sqlite_version(),sqlite_source_id()", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        eprintln!("bundled SQLite {version}; source {source}");
        assert_eq!(version, rusqlite::version());
        assert!(
            rusqlite::version_number() >= 3_051_003,
            "SQLite {version} predates the required WAL-reset fix"
        );
    }

    #[test]
    fn path_graph_preserves_two_paths_to_one_directory_inode() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA_SQL).unwrap();
        create_path_graph_schema(&conn);
        conn.execute_batch(
            "INSERT INTO paths(path_id,parent_path_id,name,dev_id,inode,kind,mount_id,prime)
             VALUES
               (1,-1,X'2f',1,1,'d',1,1),
               (2,1,X'626f6f74',1,2,'d',1,1),
               (3,2,X'656669',7,1,'d',7,1),
               (4,1,X'656669',7,1,'d',8,0),
               (5,3,X'454649',7,2,'d',7,1),
               (6,4,X'454649',7,2,'d',8,0),
               (7,5,X'424f4f54',7,42,'f',7,1),
               (8,6,X'424f4f54',7,42,'f',8,0);",
        )
        .unwrap();

        assert_eq!(
            path_graph_resolve(&conn, 7).unwrap().unwrap(),
            Path::new("/boot/efi/EFI/BOOT")
        );
        assert_eq!(
            path_graph_resolve(&conn, 8).unwrap().unwrap(),
            Path::new("/efi/EFI/BOOT")
        );
        assert_eq!(
            path_graph_lookup(&conn, Path::new("/boot/efi/EFI/BOOT")).unwrap(),
            Some(7)
        );
        assert_eq!(
            path_graph_lookup(&conn, Path::new("/efi/EFI/BOOT")).unwrap(),
            Some(8)
        );
        let copies: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM paths WHERE dev_id=7 AND inode=42",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(copies, 2, "both visible paths resolve to the same inode");
    }

    #[test]
    fn path_graph_rejects_ancestor_cycles() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA_SQL).unwrap();
        create_path_graph_schema(&conn);
        conn.execute_batch(
            "INSERT INTO paths(path_id,parent_path_id,name,dev_id,inode,kind)
             VALUES (1,-1,X'2f',1,1,'d'), (2,1,X'61',1,2,'d'), (3,2,X'62',1,3,'d');
             UPDATE paths SET parent_path_id=3 WHERE path_id=2;",
        )
        .unwrap();
        let error = path_graph_resolve(&conn, 3).unwrap_err().to_string();
        assert!(error.contains("cycle in path graph ancestors"));
    }

    #[test]
    fn path_graph_directory_totals_require_a_live_path_and_cascade_on_delete() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        create_path_graph_schema(&conn);
        conn.execute(
            "INSERT INTO paths(path_id,parent_path_id,name,dev_id,inode,kind)
             VALUES (1,-1,X'2f',1,1,'d')",
            [],
        )
        .unwrap();

        let orphan = conn.execute(
            "INSERT INTO dir_totals(path_id,visible_bytes,visible_entries,unique_bytes,unique_inodes)
             VALUES (99,0,0,0,0)",
            [],
        );
        assert!(
            orphan.is_err(),
            "directory totals must reference a path row"
        );

        conn.execute(
            "INSERT INTO dir_totals(path_id,visible_bytes,visible_entries,unique_bytes,unique_inodes)
             VALUES (1,0,0,0,0)",
            [],
        )
        .unwrap();
        conn.execute("DELETE FROM paths WHERE path_id=1", [])
            .unwrap();
        let totals: i64 = conn
            .query_row("SELECT COUNT(*) FROM dir_totals", [], |row| row.get(0))
            .unwrap();
        assert_eq!(totals, 0, "deleting a path must not leave orphaned totals");
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("dux-store-{tag}-{}", std::process::id()))
    }

    #[test]
    fn path_of_unresolved_inode_does_not_panic() {
        let db = tmp("path-of");
        for s in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{s}", db.display()));
        }
        let store = Store::create_fresh(&db).unwrap();
        assert_eq!(store.path_of(1, 42).unwrap(), "inode:42");
        drop(store);
        for s in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{s}", db.display()));
        }
    }

    #[test]
    fn open_ro_rejects_incompatible_schema() {
        let db = tmp("old-schema");
        for s in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{s}", db.display()));
        }
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO meta(key,value) VALUES('schema_version','3');",
            )
            .unwrap();
        }
        let err = match Store::open_ro(&db) {
            Ok(_) => panic!("old schema should be rejected"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("schema is v3"));
        for s in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{s}", db.display()));
        }
    }

    #[test]
    fn schema_metadata_errors_are_not_rewritten_as_version_zero() {
        let db = tmp("malformed-schema-version");
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO meta(key,value) VALUES('schema_version','not-a-number');",
            )
            .unwrap();
        }
        let err = match Store::open_ro(&db) {
            Ok(_) => panic!("malformed schema metadata should fail"),
            Err(error) => format!("{error:#}"),
        };
        assert!(
            err.contains("invalid index schema_version metadata"),
            "{err}"
        );
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
    }

    #[test]
    fn schema_metadata_query_errors_are_preserved() {
        let db = tmp("missing-meta-table");
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
        drop(Connection::open(&db).unwrap());
        let err = match Store::open_ro(&db) {
            Ok(_) => panic!("missing metadata table should fail"),
            Err(error) => format!("{error:#}"),
        };
        assert!(err.contains("reading index schema version"), "{err}");
        assert!(err.contains("no such table: meta"), "{err}");
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
    }

    #[test]
    fn rebuild_gate_propagates_unreadable_or_corrupt_database_errors() {
        let db = tmp("corrupt-rebuild-gate");
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
        std::fs::write(&db, b"not a SQLite database").unwrap();
        let err = match Store::needs_rebuild(&db) {
            Ok(value) => panic!("corrupt index must not be classified as rebuildable: {value}"),
            Err(error) => format!("{error:#}"),
        };
        assert!(err.contains("not a database"), "{err}");
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
    }

    #[test]
    fn rebuild_gate_classifies_zero_byte_database_as_empty_first_install() {
        let db = tmp("zero-byte-rebuild-gate");
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
        std::fs::File::create(&db).unwrap();
        assert!(Store::needs_rebuild(&db).unwrap());
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
    }

    #[test]
    fn index_artifact_detection_preserves_dangling_main_symlinks_and_rejects_sidecar_symlinks() {
        let db = tmp("artifact-symlink");
        let target = tmp("artifact-symlink-target");
        let wal_target = tmp("artifact-wal-target");
        let wal = std::path::PathBuf::from(format!("{}-wal", db.display()));
        for path in [&db, &target, &wal, &wal_target] {
            let _ = std::fs::remove_file(path);
        }
        std::os::unix::fs::symlink(&target, &db).unwrap();
        assert!(Store::has_index_artifacts(&db).unwrap());
        std::fs::remove_file(&db).unwrap();

        std::fs::write(&wal_target, b"sidecar target").unwrap();
        std::fs::write(&db, b"main database entry").unwrap();
        std::os::unix::fs::symlink(&wal_target, &wal).unwrap();
        let error = Store::has_index_artifacts(&db).unwrap_err().to_string();
        assert!(
            error.contains("refusing symlinked SQLite sidecar"),
            "{error}"
        );
        for path in [&wal, &wal_target, &db, &target] {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn zero_byte_main_file_with_wal_is_not_treated_as_empty() {
        let db = tmp("zero-main-with-wal");
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
        std::fs::File::create(&db).unwrap();
        let wal = std::path::PathBuf::from(format!("{}-wal", db.display()));
        let wal_bytes = b"nonempty WAL candidate; SQLite must validate it";
        std::fs::write(&wal, wal_bytes).unwrap();
        assert_eq!(std::fs::metadata(&db).unwrap().len(), 0);
        assert!(std::fs::metadata(&wal).unwrap().len() > 0);
        assert!(Store::has_recoverable_database_state(&db).unwrap());
        let error = Store::needs_rebuild(&db).unwrap_err().to_string();
        assert!(error.contains("refusing to classify"), "{error}");
        assert_eq!(std::fs::metadata(&db).unwrap().len(), 0);
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
    }

    #[test]
    fn writable_open_installs_growth_query_indexes_on_existing_v6_store() {
        let db = tmp("growth-index-upgrade");
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
        drop(Store::create_fresh(&db).unwrap());

        let store = Store::open_rw(&db).unwrap();
        for name in [
            "idx_growth_bucket_delta",
            "idx_growth_inode_bucket_delta",
            "growth_delta_integer_insert",
            "growth_delta_integer_update",
        ] {
            let object_type = if name.starts_with("idx_") {
                "index"
            } else {
                "trigger"
            };
            let exists: bool = store
                .conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type=?1 AND name=?2)",
                    [object_type, name],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(exists, "missing growth schema object {name}");
        }
        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
    }

    #[test]
    fn totals_consistent_rejects_dangling_dirent_references() {
        let db = tmp("dangling-dirent");
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
        let store = Store::create_fresh(&db).unwrap();
        store.set_meta("root_dev", "1").unwrap();
        store.set_meta("root_inode", "1").unwrap();
        store.conn.execute(
            "INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime,birth_ns)
             VALUES(1,1,'d',4096,4096,1,0,0,0)", [],
        ).unwrap();
        store
            .conn
            .execute(
                "INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
             VALUES(1,1,X'6f727068616e',1,999,1)",
                [],
            )
            .unwrap();
        assert!(
            !store.totals_consistent().unwrap(),
            "dangling child inode must invalidate totals"
        );
        store.conn.execute("DELETE FROM dirents", []).unwrap();
        store.conn.execute(
            "INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime,birth_ns)
             VALUES(1,2,'f',0,0,1,0,0,0)", [],
        ).unwrap();
        store
            .conn
            .execute(
                "INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
             VALUES(1,999,X'6f727068616e',1,2,1)",
                [],
            )
            .unwrap();
        assert!(
            !store.totals_consistent().unwrap(),
            "missing parent inode must invalidate totals"
        );
        store.conn.execute("DELETE FROM dirents", []).unwrap();
        store
            .conn
            .execute("DELETE FROM inodes WHERE inode != 1", [])
            .unwrap();
        store
            .conn
            .execute("UPDATE inodes SET recursive_inodes=3", [])
            .unwrap();
        for inode in [2, 3] {
            store.conn.execute(
                "INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime,birth_ns)
                 VALUES(1,?1,'f',0,0,1,0,0,0)", [inode],
            ).unwrap();
            store
                .conn
                .execute(
                    "INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
                 VALUES(1,1,?1,1,?2,1)",
                    rusqlite::params![inode.to_string().into_bytes(), inode],
                )
                .unwrap();
        }
        store
            .conn
            .execute(
                "INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
             VALUES(1,2,X'696e76616c6964',1,3,0)",
                [],
            )
            .unwrap();
        assert!(
            !store.totals_consistent().unwrap(),
            "file parent must invalidate directory entries"
        );
        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
    }
}

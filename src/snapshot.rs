//! Compact on-disk snapshot of a folder tree from the walker.
//!
//! One record per directory (never per file), integers as LEB128 varints,
//! written to a temporary file and renamed into place, so a crash leaves
//! either the old snapshot or the new one. A small growth history is kept
//! beside it as JSON lines, one summary per walk.

use crate::walker::{DirNode, FileEntry, Tree, WalkStats};
use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"DUXSNAP1";
/// Top files smaller than this are not stored; they never matter for "what is big".
const MIN_TOP_FILE: u64 = 64 * 1024;
/// History lines older than this are dropped when the file is rewritten.
const HISTORY_KEEP_SECS: i64 = 30 * 86_400;

pub const DEFAULT_DIR: &str = "/var/lib/dux/sentinel";

fn put_u(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn put_i(out: &mut Vec<u8>, v: i64) {
    put_u(out, ((v << 1) ^ (v >> 63)) as u64);
}

fn put_s(out: &mut Vec<u8>, s: &str) {
    put_u(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl Reader<'_> {
    fn u(&mut self) -> Result<u64> {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let byte = *self.b.get(self.i).context("snapshot truncated")?;
            self.i += 1;
            anyhow::ensure!(shift < 64, "snapshot varint too long");
            v |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
        }
    }
    fn i(&mut self) -> Result<i64> {
        let u = self.u()?;
        Ok(((u >> 1) as i64) ^ -((u & 1) as i64))
    }
    fn s(&mut self) -> Result<String> {
        let n = self.u()? as usize;
        anyhow::ensure!(n <= 4096, "snapshot name too long");
        let end = self.i.checked_add(n).context("snapshot overflow")?;
        let bytes = self.b.get(self.i..end).context("snapshot truncated")?;
        self.i = end;
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }
}

fn dec_node(r: &mut Reader, depth: usize) -> Result<DirNode> {
    anyhow::ensure!(depth < 4096, "snapshot nesting too deep");
    let mut n = DirNode {
        name: r.s()?,
        own_bytes: r.u()?,
        total_apparent: r.u()?,
        own_files: r.u()?,
        newest_change: r.i()?,
        errors: r.u()? as u32,
        ..DirNode::default()
    };
    let tops = r.u()? as usize;
    anyhow::ensure!(tops <= 64, "snapshot corrupt: too many top files");
    for _ in 0..tops {
        n.top_files.push(FileEntry {
            name: r.s()?,
            bytes: r.u()?,
        });
    }
    let kids = r.u()? as usize;
    anyhow::ensure!(kids <= 50_000_000, "snapshot corrupt: child count");
    n.children.reserve(kids.min(1 << 16));
    for _ in 0..kids {
        n.children.push(dec_node(r, depth + 1)?);
    }
    // Totals are derived, so they are recomputed rather than stored.
    let own_apparent = n.total_apparent;
    n.total_bytes = n.own_bytes + n.children.iter().map(|c| c.total_bytes).sum::<u64>();
    n.total_apparent = own_apparent + n.children.iter().map(|c| c.total_apparent).sum::<u64>();
    n.total_entries = 1 + n.own_files + n.children.iter().map(|c| c.total_entries).sum::<u64>();
    n.newest_change = n
        .children
        .iter()
        .map(|c| c.newest_change)
        .fold(n.newest_change, i64::max);
    Ok(n)
}

/// Nodes keep cumulative apparent bytes; the snapshot stores each node's own
/// share so totals can be rebuilt on load.
fn own_apparent(n: &DirNode) -> u64 {
    n.total_apparent
        .saturating_sub(n.children.iter().map(|c| c.total_apparent).sum::<u64>())
}

pub fn encode(t: &Tree) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 << 20);
    out.extend_from_slice(MAGIC);
    put_s(&mut out, &t.root_path);
    put_i(&mut out, t.started_at);
    put_u(&mut out, t.stats.entries);
    put_u(&mut out, (t.stats.seconds * 1000.0) as u64);
    fn rec(out: &mut Vec<u8>, n: &DirNode) {
        put_s(out, &n.name);
        put_u(out, n.own_bytes);
        put_u(out, own_apparent(n));
        put_u(out, n.own_files);
        put_i(out, n.newest_change);
        put_u(out, u64::from(n.errors));
        let tops: Vec<&FileEntry> = n
            .top_files
            .iter()
            .filter(|f| f.bytes >= MIN_TOP_FILE)
            .collect();
        put_u(out, tops.len() as u64);
        for f in tops {
            put_s(out, &f.name);
            put_u(out, f.bytes);
        }
        put_u(out, n.children.len() as u64);
        for c in &n.children {
            rec(out, c);
        }
    }
    rec(&mut out, &t.root);
    out
}

pub fn decode(b: &[u8]) -> Result<Tree> {
    anyhow::ensure!(b.len() >= 8 && &b[..8] == MAGIC, "not a dux snapshot");
    let mut r = Reader { b, i: 8 };
    let root_path = r.s()?;
    let started_at = r.i()?;
    let entries = r.u()?;
    let ms = r.u()?;
    let root = dec_node(&mut r, 0)?;
    anyhow::ensure!(r.i == b.len(), "snapshot has trailing bytes");
    Ok(Tree {
        root_path,
        root,
        stats: WalkStats {
            entries,
            seconds: ms as f64 / 1000.0,
            ..WalkStats::default()
        },
        started_at,
    })
}

/// File name for a mount's snapshot, e.g. "/" -> "root.snap", "/var/lib" -> "var_lib.snap".
pub fn file_for(dir: &Path, mount: &str) -> PathBuf {
    let slug: String = mount
        .trim_matches('/')
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    dir.join(format!(
        "{}.snap",
        if slug.is_empty() {
            "root".to_string()
        } else {
            slug
        }
    ))
}

pub fn save(dir: &Path, t: &Tree) -> Result<(PathBuf, usize)> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = file_for(dir, &t.root_path);
    let tmp = path.with_extension("snap.tmp");
    let bytes = encode(t);
    {
        let mut f =
            std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok((path, bytes.len()))
}

pub fn load(dir: &Path, mount: &str) -> Result<Option<Tree>> {
    load_path(&file_for(dir, mount))
}

pub fn load_path(path: &Path) -> Result<Option<Tree>> {
    let mut b = Vec::new();
    match std::fs::File::open(path) {
        Ok(mut f) => {
            f.read_to_end(&mut b)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("opening {}", path.display())),
    }
    decode(&b)
        .with_context(|| format!("reading {}", path.display()))
        .map(Some)
}

/// Append one summary line per walk: totals and the largest folders two
/// levels down. A few KB per walk; old lines are pruned occasionally.
pub fn append_history(dir: &Path, t: &Tree, top: &[(String, u64)]) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("history.jsonl");
    let line = serde_json::json!({
        "t": t.started_at,
        "mount": t.root_path,
        "bytes": t.root.total_bytes,
        "entries": t.stats.entries,
        "walk_s": (t.stats.seconds * 100.0).round() / 100.0,
        "top": top.iter().map(|(p, b)| serde_json::json!([p, b])).collect::<Vec<_>>(),
    })
    .to_string();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(f, "{line}")?;
    // Keep 30 days (spec A2.4): prune when the oldest line is older.
    let cutoff_now = crate::util::now_secs() - HISTORY_KEEP_SECS;
    let oldest = std::fs::File::open(&path)
        .ok()
        .and_then(|fh| {
            use std::io::BufRead;
            std::io::BufReader::new(fh).lines().next()?.ok()
        })
        .and_then(|l| serde_json::from_str::<serde_json::Value>(&l).ok())
        .and_then(|v| v["t"].as_i64());
    if oldest.is_some_and(|t| t < cutoff_now)
        || f.metadata().map(|m| m.len()).unwrap_or(0) > 8 << 20
    {
        let cutoff = crate::util::now_secs() - HISTORY_KEEP_SECS;
        let text = std::fs::read_to_string(&path)?;
        let kept: String = text
            .lines()
            .filter(|l| {
                serde_json::from_str::<serde_json::Value>(l)
                    .ok()
                    .and_then(|v| v["t"].as_i64())
                    .is_some_and(|t| t >= cutoff)
            })
            .map(|l| format!("{l}\n"))
            .collect();
        let tmp = path.with_extension("jsonl.tmp");
        std::fs::write(&tmp, kept)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Tree {
        let leaf = |name: &str, bytes: u64| DirNode {
            name: name.into(),
            own_bytes: bytes,
            total_bytes: bytes,
            total_apparent: bytes / 2,
            own_files: 3,
            total_entries: 4,
            newest_change: 1_795_000_000,
            top_files: vec![
                FileEntry {
                    name: "big.log".into(),
                    bytes: bytes / 2,
                },
                FileEntry {
                    name: "tiny".into(),
                    bytes: 10,
                },
            ],
            ..DirNode::default()
        };
        let mut root = DirNode {
            name: "/".into(),
            own_bytes: 4096,
            own_files: 1,
            newest_change: -5,
            ..DirNode::default()
        };
        root.children = vec![leaf("home", 5 << 20), leaf("var é", 9 << 30)];
        root.total_apparent = 100 + root.children.iter().map(|c| c.total_apparent).sum::<u64>();
        root.total_bytes =
            root.own_bytes + root.children.iter().map(|c| c.total_bytes).sum::<u64>();
        root.total_entries =
            1 + root.own_files + root.children.iter().map(|c| c.total_entries).sum::<u64>();
        root.newest_change = 1_795_000_000;
        Tree {
            root_path: "/".into(),
            root,
            stats: WalkStats {
                entries: 9,
                seconds: 1.25,
                ..WalkStats::default()
            },
            started_at: 1_795_000_100,
        }
    }

    #[test]
    fn varints_roundtrip() {
        for v in [0u64, 1, 127, 128, 300, u32::MAX as u64, u64::MAX] {
            let mut b = Vec::new();
            put_u(&mut b, v);
            assert_eq!(Reader { b: &b, i: 0 }.u().unwrap(), v);
        }
        for v in [0i64, -1, 1, i64::MIN, i64::MAX, -1_795_000_000] {
            let mut b = Vec::new();
            put_i(&mut b, v);
            assert_eq!(Reader { b: &b, i: 0 }.i().unwrap(), v);
        }
    }

    #[test]
    fn snapshot_roundtrip_keeps_totals_and_drops_tiny_top_files() {
        let t = sample();
        let b = encode(&t);
        let d = decode(&b).unwrap();
        assert_eq!(d.root_path, "/");
        assert_eq!(d.started_at, t.started_at);
        assert_eq!(d.root.total_bytes, t.root.total_bytes);
        assert_eq!(d.root.total_apparent, t.root.total_apparent);
        assert_eq!(d.root.total_entries, t.root.total_entries);
        let var = d.root.find("var é").unwrap();
        assert_eq!(
            var.top_files,
            vec![FileEntry {
                name: "big.log".into(),
                bytes: (9 << 30) / 2
            }]
        );
        // Stable: re-encoding the decoded tree gives identical bytes.
        assert_eq!(encode(&d), b);
    }

    #[test]
    fn corrupt_snapshots_are_rejected_not_panicking() {
        let b = encode(&sample());
        assert!(decode(&b[..b.len() - 3]).is_err());
        assert!(decode(b"NOTASNAP").is_err());
        let mut extra = b.clone();
        extra.push(0);
        assert!(decode(&extra).is_err());
        let mut bad = b.clone();
        bad[8] = 0xff; // corrupt the root path length varint
        assert!(decode(&bad).is_err());
    }

    #[test]
    fn save_load_and_history() {
        let dir = std::env::temp_dir().join(format!("dux-snap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let t = sample();
        let (path, size) = save(&dir, &t).unwrap();
        assert!(path.ends_with("root.snap"));
        assert!(size < 400);
        let back = load(&dir, "/").unwrap().unwrap();
        assert_eq!(back.root.total_bytes, t.root.total_bytes);
        assert!(load(&dir, "/nope").unwrap().is_none());
        append_history(&dir, &t, &[("/var".into(), 9 << 30)]).unwrap();
        let h = std::fs::read_to_string(dir.join("history.jsonl")).unwrap();
        let v: serde_json::Value = serde_json::from_str(h.trim()).unwrap();
        assert_eq!(v["top"][0][0], "/var");
        assert_eq!(
            file_for(&dir, "/var/lib/docker"),
            dir.join("var_lib_docker.snap")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

//! The "never touch" list: paths dux must never clean, whatever any plan,
//! fix list or flag says. One path per line in /etc/dux/never-touch
//! (`#` comments allowed). A path protects itself and everything under it.

use anyhow::{Context, Result};
use std::path::Path;

pub const FILE: &str = "/etc/dux/never-touch";

pub fn load_from(file: &Path) -> Vec<String> {
    std::fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter(|l| l.starts_with('/'))
        .map(normalize)
        .filter(|l| l != "/")
        .collect()
}

pub fn load() -> Vec<String> {
    load_from(Path::new(FILE))
}

/// Is `path` on the list, or inside a listed path?
pub fn covered(list: &[String], path: &str) -> bool {
    let p = path.trim_end_matches('/');
    list.iter()
        .any(|x| p == x || p.starts_with(&format!("{x}/")))
}

/// Checked on the path as written (with `.`/`..` resolved) and on its real
/// location, so a symlinked parent folder cannot route around the list.
pub fn is_protected(path: &Path) -> bool {
    let list = load();
    if list.is_empty() {
        return false;
    }
    let text = normalize(&path.to_string_lossy());
    if covered(&list, &text) {
        return true;
    }
    // The real path of the parent (the entry itself may be a symlink, which
    // is removed as a link and never followed).
    let p = Path::new(&text);
    let real = match (p.parent(), p.file_name()) {
        (Some(dir), Some(name)) => std::fs::canonicalize(dir).ok().map(|d| d.join(name)),
        _ => None,
    };
    real.is_some_and(|r| covered(&list, &r.to_string_lossy()))
}

fn save_to(file: &Path, list: &[String]) -> Result<()> {
    if let Some(d) = file.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut text =
        String::from("# Paths dux never cleans (one per line). Managed by `dux never-touch`.\n");
    for x in list {
        text.push_str(x);
        text.push('\n');
    }
    let tmp = file.with_extension("tmp");
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, file).with_context(|| format!("saving {}", file.display()))
}

/// Resolve `.` and `..` without touching the disk, so `/a/b/../c` is stored
/// (and compared) as `/a/c`.
pub fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for c in path.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            x => parts.push(x),
        }
    }
    format!("/{}", parts.join("/"))
}

pub fn add_to(file: &Path, path: &str) -> Result<bool> {
    anyhow::ensure!(path.starts_with('/'), "use an absolute path");
    let mut list = load_from(file);
    let p = normalize(path);
    anyhow::ensure!(
        p != "/",
        "protecting / would block every cleanup; protect specific folders"
    );
    if list.contains(&p) {
        return Ok(false);
    }
    list.push(p);
    list.sort();
    save_to(file, &list)?;
    Ok(true)
}

pub fn remove_from(file: &Path, path: &str) -> Result<bool> {
    let mut list = load_from(file);
    let p = normalize(path);
    let before = list.len();
    list.retain(|x| *x != p);
    if list.len() == before {
        return Ok(false);
    }
    save_to(file, &list)?;
    Ok(true)
}

pub fn add(path: &str) -> Result<bool> {
    add_to(Path::new(FILE), path)
}

pub fn remove(path: &str) -> Result<bool> {
    remove_from(Path::new(FILE), path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symlinked_parent_cannot_bypass_the_list() {
        let d = std::env::temp_dir().join(format!("dux-protect-l-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("real/keep")).unwrap();
        std::os::unix::fs::symlink(d.join("real"), d.join("alias")).unwrap();
        let list = vec![normalize(&d.join("real/keep").to_string_lossy())];
        let via = d.join("alias/keep/file");
        let dir = std::fs::canonicalize(via.parent().unwrap()).unwrap();
        assert!(covered(&list, &dir.join("file").to_string_lossy()));
        assert!(
            !covered(&list, &via.to_string_lossy()),
            "text alone would miss it"
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn paths_are_normalized_and_root_refused() {
        assert_eq!(normalize("/a/b/../c/./d/"), "/a/c/d");
        assert_eq!(normalize("/../.."), "/");
        let d = std::env::temp_dir().join(format!("dux-protect-n-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("never-touch");
        assert!(add_to(&f, "/srv/app/../data/").unwrap());
        assert_eq!(load_from(&f), vec!["/srv/data".to_string()]);
        assert!(add_to(&f, "/").is_err());
        assert!(add_to(&f, "rel/x").is_err());
        assert!(remove_from(&f, "/srv/./data").unwrap());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn list_roundtrip_and_coverage() {
        let d = std::env::temp_dir().join(format!("dux-protect-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("never-touch");
        assert!(add_to(&f, "/home/app/node_modules/").unwrap());
        assert!(
            !add_to(&f, "/home/app/node_modules").unwrap(),
            "no duplicates"
        );
        assert!(add_to(&f, "/srv/data").unwrap());
        assert!(add_to(&f, "relative").is_err());
        let l = load_from(&f);
        assert_eq!(l, vec!["/home/app/node_modules", "/srv/data"]);
        assert!(covered(&l, "/srv/data"));
        assert!(covered(&l, "/srv/data/x/y.log"));
        assert!(
            !covered(&l, "/srv/database"),
            "prefix must end at a path boundary"
        );
        assert!(remove_from(&f, "/srv/data").unwrap());
        assert!(!covered(&load_from(&f), "/srv/data/x"));
        std::fs::remove_dir_all(&d).unwrap();
    }
}

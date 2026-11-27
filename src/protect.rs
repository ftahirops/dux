//! The "never touch" list: paths dux must never clean, whatever any plan,
//! fix list or flag says. One absolute path per line in /etc/dux/never-touch
//! (`#` comments allowed).
//!
//! Rules, enforced by [`check`] for every destructive action:
//! - a listed path protects itself and everything under it;
//! - removing a folder that *contains* a listed path is refused too;
//! - the target is checked as written and at its real location, so a
//!   symlinked parent cannot route around the list;
//! - the list fails closed: if the file exists but cannot be read, every
//!   destructive action is refused until it is fixed. Only a missing file
//!   means "nothing protected".

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub const FILE: &str = "/etc/dux/never-touch";

/// Resolve `.` and `..` lexically, so `/a/b/../c` is stored as `/a/c`.
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

fn parse(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| l.starts_with('/'))
        .map(normalize)
        .filter(|l| l != "/")
        .collect()
}

/// The list as stored. A missing file is an empty list; any other read
/// error (permissions, invalid UTF-8, I/O) is an error.
pub fn policy_from(file: &Path) -> Result<Vec<String>> {
    match std::fs::read_to_string(file) {
        Ok(t) => Ok(parse(&t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => {
            Err(e).with_context(|| format!("cannot read the never-touch list {}", file.display()))
        }
    }
}

/// The list for display. Unreadable lists show as empty here; enforcement
/// uses [`policy_from`] and fails closed.
pub fn load_from(file: &Path) -> Vec<String> {
    policy_from(file).unwrap_or_default()
}

pub fn load() -> Vec<String> {
    load_from(Path::new(FILE))
}

/// Is `path` on the list, or inside a listed path? (Text only.)
pub fn covered(list: &[String], path: &str) -> bool {
    let p = path.trim_end_matches('/');
    let p = if p.is_empty() { "/" } else { p };
    list.iter()
        .any(|x| p == x || p.starts_with(&format!("{x}/")))
}

/// Does removing `path` touch a listed path: is it listed, inside one, or
/// does it contain one? (Text only; a protected sibling does not count.)
pub fn conflicts(list: &[String], path: &str) -> bool {
    let p = path.trim_end_matches('/');
    let p = if p.is_empty() { "/" } else { p };
    covered(list, p)
        || list
            .iter()
            .any(|x| p == "/" || x.starts_with(&format!("{p}/")))
}

/// The spellings of `path` to check: lexical, and the real location (the
/// parent resolved by the kernel, exactly as written, plus the final name,
/// which is never followed so a symlink is judged as the link itself).
fn forms(path: &Path) -> Vec<String> {
    let text = path.to_string_lossy();
    let mut out = vec![normalize(&text)];
    if let (Some(dir), Some(name)) = (path.parent(), path.file_name()) {
        let dir = if dir.as_os_str().is_empty() {
            Path::new("/")
        } else {
            dir
        };
        if let Ok(real) = std::fs::canonicalize(dir) {
            out.push(normalize(&real.join(name).to_string_lossy()));
        }
    }
    out.dedup();
    out
}

/// Refuse if removing or rewriting `path` would touch a protected path, or
/// if the list cannot be read.
pub fn check_in(file: &Path, path: &Path) -> Result<()> {
    let list = policy_from(file)?;
    if list.is_empty() {
        return Ok(());
    }
    for f in forms(path) {
        anyhow::ensure!(
            !conflicts(&list, &f),
            "{} is on, inside or contains a path on the never-touch list",
            path.display()
        );
    }
    Ok(())
}

pub fn check(path: &Path) -> Result<()> {
    check_in(Path::new(FILE), path)
}

/// True when dux must not touch `path` (fails closed on an unreadable list).
pub fn is_protected(path: &Path) -> bool {
    check(path).is_err()
}

fn save_to(file: &Path, list: &[String]) -> Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut text = String::from(
        "# Paths dux must never delete, truncate or offer as a fix.\n\
         # One absolute path per line; a path protects everything under it.\n",
    );
    for x in list {
        text.push_str(x);
        text.push('\n');
    }
    let tmp = file.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, file).with_context(|| format!("saving {}", file.display()))
}

/// Hold an exclusive lock for a read-modify-write of the list, so two
/// editors (CLI and UI) cannot lose each other's change.
fn locked<T>(file: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    use std::os::unix::io::AsRawFd;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let lock_path = file.with_extension("lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("opening {}", lock_path.display()))?;
    // SAFETY: flock on a file descriptor we own; released when `lock` drops.
    let rc = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
    anyhow::ensure!(rc == 0, "cannot lock {}", lock_path.display());
    f()
}

/// A path the list can store and read back unchanged.
fn storable(path: &str) -> Result<String> {
    anyhow::ensure!(path.starts_with('/'), "use an absolute path");
    anyhow::ensure!(
        !path.chars().any(char::is_control) && path.trim() == path,
        "this path has control characters or surrounding spaces; the list cannot store it exactly"
    );
    let p = normalize(path);
    anyhow::ensure!(
        p != "/",
        "protecting / would block every cleanup; protect specific folders"
    );
    Ok(p)
}

/// The real location of `p`, when it differs from the text (a symlinked
/// parent or the path itself is a symlink to a folder).
fn real_form(p: &str) -> Option<String> {
    let real = std::fs::canonicalize(p).ok()?;
    let r = normalize(&real.to_string_lossy());
    (r != p && r != "/").then_some(r)
}

/// Add a path (and its real location, when different). Ok(false) when it
/// was already listed.
pub fn add_to(file: &Path, path: &str) -> Result<bool> {
    let p = storable(path)?;
    locked(file, || {
        let mut list = policy_from(file)?;
        let mut changed = false;
        for x in std::iter::once(p.clone()).chain(real_form(&p)) {
            if !list.contains(&x) {
                list.push(x);
                changed = true;
            }
        }
        if changed {
            list.sort();
            save_to(file, &list)?;
        }
        Ok(changed)
    })
}

/// Remove a path (and its real location). Ok(false) when it was not listed.
pub fn remove_from(file: &Path, path: &str) -> Result<bool> {
    anyhow::ensure!(path.starts_with('/'), "use an absolute path");
    let p = normalize(path);
    locked(file, || {
        let mut list = policy_from(file)?;
        let real = real_form(&p);
        let before = list.len();
        list.retain(|x| *x != p && Some(x) != real.as_ref());
        if list.len() == before {
            return Ok(false);
        }
        save_to(file, &list)?;
        Ok(true)
    })
}

pub fn add(path: &str) -> Result<bool> {
    add_to(Path::new(FILE), path)
}

pub fn remove(path: &str) -> Result<bool> {
    remove_from(Path::new(FILE), path)
}

/// Folders a cleanup command changes, so the list can veto it. Unknown
/// commands return None.
pub fn command_scope(program: &str, _args: &[String]) -> Option<Vec<PathBuf>> {
    let name = Path::new(program)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dirs: &[&str] = match name.as_str() {
        "apt-get" | "apt" => &["/var/cache/apt"],
        "dnf" => &["/var/cache/dnf"],
        "yum" => &["/var/cache/yum"],
        "journalctl" => &["/var/log/journal", "/run/log/journal"],
        "docker" => &["/var/lib/docker"],
        "podman" => &["/var/lib/containers"],
        "logrotate" => &["/var/log"],
        "systemd-tmpfiles" => &["/tmp", "/var/tmp"],
        "snap" => &["/var/lib/snapd"],
        _ => return None,
    };
    Some(dirs.iter().map(PathBuf::from).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dux-protect-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn symlinked_parent_cannot_bypass_the_list() {
        let d = tmp("l");
        std::fs::create_dir_all(d.join("real/keep")).unwrap();
        std::os::unix::fs::symlink(d.join("real"), d.join("alias")).unwrap();
        let f = d.join("never-touch");
        add_to(&f, &d.join("real/keep").to_string_lossy()).unwrap();
        assert!(check_in(&f, &d.join("alias/keep/file")).is_err());
        assert!(check_in(&f, &d.join("real/other")).is_ok());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn an_alias_entry_also_protects_the_real_folder() {
        let d = tmp("a");
        std::fs::create_dir_all(d.join("real/keep")).unwrap();
        std::os::unix::fs::symlink(d.join("real"), d.join("alias")).unwrap();
        let f = d.join("never-touch");
        assert!(add_to(&f, &d.join("alias/keep").to_string_lossy()).unwrap());
        assert!(check_in(&f, &d.join("real/keep/report")).is_err());
        assert!(remove_from(&f, &d.join("alias/keep").to_string_lossy()).unwrap());
        assert!(policy_from(&f).unwrap().is_empty(), "both forms removed");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn removing_a_parent_of_a_listed_path_is_refused() {
        let d = tmp("p");
        let f = d.join("never-touch");
        add_to(&f, "/srv/app/node_modules/fixtures").unwrap();
        assert!(check_in(&f, Path::new("/srv/app/node_modules")).is_err());
        assert!(check_in(&f, Path::new("/srv/app")).is_err());
        assert!(check_in(&f, Path::new("/srv/app/node_modules/fixtures/x")).is_err());
        assert!(
            check_in(&f, Path::new("/srv/app/node_modules/other")).is_ok(),
            "siblings are fine"
        );
        assert!(
            check_in(&f, Path::new("/srv/application")).is_ok(),
            "path boundary"
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn an_unreadable_list_fails_closed() {
        let d = tmp("u");
        let f = d.join("never-touch");
        std::fs::write(&f, b"/srv/a\n\xff\xfe\n").unwrap();
        assert!(policy_from(&f).is_err());
        assert!(check_in(&f, Path::new("/tmp/anything")).is_err());
        assert!(
            add_to(&f, "/srv/b").is_err(),
            "do not overwrite a list we cannot read"
        );
        assert!(check_in(&d.join("missing"), Path::new("/tmp/x")).is_ok());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn paths_are_normalized_and_unstorable_ones_refused() {
        assert_eq!(normalize("/a/b/../c/./d/"), "/a/c/d");
        assert_eq!(normalize("/../.."), "/");
        let d = tmp("n");
        let f = d.join("never-touch");
        assert!(add_to(&f, "/srv/app/../data/").unwrap());
        assert!(!add_to(&f, "/srv/data").unwrap(), "no duplicates");
        assert_eq!(load_from(&f), vec!["/srv/data".to_string()]);
        assert!(add_to(&f, "/").is_err());
        assert!(add_to(&f, "rel/x").is_err());
        assert!(add_to(&f, "/srv/archive ").is_err());
        assert!(add_to(&f, "/srv/a\nb").is_err());
        assert!(remove_from(&f, "/srv/./data").unwrap());
        assert!(!remove_from(&f, "/srv/typo").unwrap());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn coverage_respects_path_boundaries() {
        let l = vec![
            "/home/app/node_modules".to_string(),
            "/srv/data".to_string(),
        ];
        assert!(covered(&l, "/srv/data"));
        assert!(covered(&l, "/srv/data/x/y.log"));
        assert!(!covered(&l, "/srv/database"));
        assert!(conflicts(&l, "/home/app"));
        assert!(!conflicts(&l, "/home/application"));
        assert!(conflicts(&l, "/"));
    }

    #[test]
    fn commands_have_a_scope() {
        let s = command_scope("apt-get", &["clean".into()]).unwrap();
        assert_eq!(s, vec![PathBuf::from("/var/cache/apt")]);
        assert!(command_scope("/usr/bin/journalctl", &[]).is_some());
        assert!(command_scope("whatever", &[]).is_none());
    }
}

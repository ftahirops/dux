use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::Command;

const SOURCE_TREE_EXCLUDES: &[&[u8]] = &[
    b"PRODUCTION_READINESS_AUDIT.md",
    b".claude/settings.local.json",
];

fn source_tree_sha256() -> Option<String> {
    let output = Command::new("git")
        .args(["ls-files", "-co", "--exclude-standard", "-z"])
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let mut names: Vec<Vec<u8>> = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty() && !SOURCE_TREE_EXCLUDES.contains(name))
        .map(ToOwned::to_owned)
        .collect();
    names.sort();

    let mut hasher = Sha256::new();
    let mut rerun_paths = BTreeSet::from([PathBuf::from(".")]);
    for name in names {
        let path = PathBuf::from(OsStr::from_bytes(&name));
        let metadata = fs::symlink_metadata(&path).ok()?;
        if metadata.file_type().is_symlink() {
            return None;
        }
        if !metadata.is_file() {
            continue;
        }
        let contents = fs::read(&path).ok()?;
        hasher.update((name.len() as u64).to_be_bytes());
        hasher.update(&name);
        hasher.update((contents.len() as u64).to_be_bytes());
        hasher.update(&contents);

        let mut parent = path.parent();
        while let Some(directory) = parent {
            rerun_paths.insert(directory.to_path_buf());
            parent = directory.parent();
        }
        rerun_paths.insert(path.clone());
    }
    for path in rerun_paths {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    let digest = format!("{:x}", hasher.finalize());
    Some(digest)
}

fn main() {
    let revision = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|| "source-archive".into());
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .is_some_and(|o| o.status.success() && !o.stdout.is_empty());
    println!(
        "cargo:rustc-env=DUX_BUILD_REVISION={revision}{}",
        if dirty { "+modified" } else { "" }
    );
    let source_digest = source_tree_sha256().unwrap_or_else(|| "unavailable".into());
    println!("cargo:rustc-env=DUX_BUILD_SOURCE_SHA256={source_digest}");
    let kind = if std::env::var_os("CARGO_FEATURE_FAULT_INJECTION").is_some() {
        "fault-injection"
    } else {
        "production"
    };
    println!("cargo:rustc-env=DUX_BUILD_KIND={kind}");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
    println!("cargo:rerun-if-changed=.git/refs");
    println!("cargo:rerun-if-changed=.git/packed-refs");
    println!("cargo:rerun-if-changed=build.rs");
}

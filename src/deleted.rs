use anyhow::Result;
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

pub struct DeletedOpen {
    pub pid: i32,
    pub process: String,
    pub uid: u32,
    pub size: i64,
    pub path: String,
}

/// Scan /proc/<pid>/fd for deleted-but-open files still consuming disk.
/// MVP: no eBPF. Requires root to see other users' fds.
pub fn deleted_open() -> Result<Vec<DeletedOpen>> {
    // Aggregate by (pid, dev, inode) so dup fds aren't double counted — inode
    // numbers collide across devices, so dev must be part of the key.
    let mut seen: HashMap<(i32, u64, u64), DeletedOpen> = HashMap::new();

    for entry in fs::read_dir("/proc")?.flatten() {
        let name = entry.file_name();
        let pid: i32 = match name.to_string_lossy().parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let fd_dir = entry.path().join("fd");
        let rd = match fs::read_dir(&fd_dir) {
            Ok(r) => r,
            Err(_) => continue, // process gone or no permission
        };
        let process = read_comm(pid);
        let uid = read_uid(&entry.path());

        for fd in rd.flatten() {
            let link = match fs::read_link(fd.path()) {
                Ok(l) => l,
                Err(_) => continue,
            };
            let s = link.to_string_lossy();
            if !s.ends_with(" (deleted)") {
                continue;
            }
            // stat the fd to get real size + inode (link target is gone)
            let meta = match fs::metadata(fd.path()) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !meta.is_file() {
                continue;
            }
            let ino = meta.ino();
            let dev = meta.dev();
            // report ALLOCATED disk (blocks*512), not apparent size — a sparse
            // deleted file doesn't pin its apparent size on disk.
            let size = (meta.blocks() as i64) * 512;
            let clean = s.trim_end_matches(" (deleted)").to_string();
            seen.entry((pid, dev, ino)).or_insert(DeletedOpen {
                pid,
                process: process.clone(),
                uid,
                size,
                path: clean,
            });
        }
    }

    let mut out: Vec<DeletedOpen> = seen.into_values().collect();
    out.sort_by_key(|d| std::cmp::Reverse(d.size));
    Ok(out)
}

fn read_comm(pid: i32) -> String {
    fs::read_to_string(format!("/proc/{pid}/comm"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "?".to_string())
}

fn read_uid(proc_path: &PathBuf) -> u32 {
    fs::metadata(proc_path).map(|m| m.uid()).unwrap_or(0)
}

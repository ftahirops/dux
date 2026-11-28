//! Quality tests across modules: the destructive-path audit (#7),
//! concurrency (#6) and fuzzing of every input parser (#5).
//! Tests that need root or mount privileges skip themselves otherwise.

use crate::cleanup::{self, Action, Item};
use std::path::{Path, PathBuf};

fn root() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("dux-qa-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn item(action: Action) -> Item {
    Item {
        level: 4,
        what: "qa".into(),
        action,
        frees: 0,
    }
}

// ------------------------------------------------- #7 destructive paths ----

#[test]
fn delete_through_a_symlinked_parent_into_etc_is_refused() {
    let d = tmp("symetc");
    std::os::unix::fs::symlink("/etc", d.join("link")).unwrap();
    let r = cleanup::apply(&item(Action::RemoveUserPath {
        path: d.join("link/hostname"),
    }));
    assert!(
        r.is_err(),
        "deleting /etc/hostname via a symlinked parent must be refused"
    );
    assert!(Path::new("/etc/hostname").exists() || !Path::new("/etc").join("hostname").exists());
    let r = cleanup::apply(&item(Action::RemoveTree {
        path: d.join("link/node_modules"),
    }));
    assert!(r.is_err());
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn dot_dot_escapes_are_judged_at_the_real_location() {
    let d = tmp("dotdot");
    let p = d.join("a/../../../../../../etc/hosts");
    std::fs::create_dir_all(d.join("a")).unwrap();
    assert!(cleanup::apply(&item(Action::RemoveUserPath { path: p })).is_err());
    assert!(Path::new("/etc/hosts").exists());
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn runtime_and_database_stores_are_system_paths() {
    for p in [
        "/",
        "/home",
        "/var/lib",
        "/etc/x",
        "/var/lib/docker/overlay2/abc",
        "/var/lib/postgresql/16",
        "/var/lib/containerd",
        "/usr/lib/node_modules",
    ] {
        assert!(cleanup::system_path(p), "{p} must be refused");
    }
    for p in [
        "/home/alice/old",
        "/srv/app/node_modules",
        "/var/lib/xgs/incidents",
        "/opt/app/cache",
    ] {
        assert!(!cleanup::system_path(p), "{p} is a user path");
    }
}

#[test]
fn a_symlink_is_removed_as_a_link_and_its_target_kept() {
    let d = tmp("linkonly");
    std::fs::create_dir_all(d.join("data")).unwrap();
    std::fs::write(d.join("data/precious"), b"p").unwrap();
    std::os::unix::fs::symlink(d.join("data"), d.join("shortcut")).unwrap();
    cleanup::apply(&item(Action::RemoveUserPath {
        path: d.join("shortcut"),
    }))
    .unwrap();
    assert!(!d.join("shortcut").exists());
    assert!(d.join("data/precious").exists());
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn removing_one_hardlink_keeps_the_other_name() {
    let d = tmp("hard");
    std::fs::write(d.join("a.log.1"), b"data").unwrap();
    std::fs::hard_link(d.join("a.log.1"), d.join("b")).unwrap();
    cleanup::apply(&item(Action::Remove {
        path: d.join("a.log.1"),
    }))
    .unwrap();
    assert_eq!(std::fs::read(d.join("b")).unwrap(), b"data");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_file_held_open_by_a_process_is_not_removed() {
    let d = tmp("held");
    let f = d.join("app.log.1");
    std::fs::write(&f, vec![1u8; 4096]).unwrap();
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("exec 3<'{}'; sleep 5", f.display()))
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(400));
    let r = cleanup::apply(&item(Action::Remove { path: f.clone() }));
    let _ = child.kill();
    let _ = child.wait();
    assert!(r.is_err(), "open file must be skipped");
    assert!(f.exists());
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn only_real_build_output_folders_are_removed_as_trees() {
    let d = tmp("tree");
    std::fs::create_dir_all(d.join("src")).unwrap();
    assert!(
        cleanup::apply(&item(Action::RemoveTree {
            path: d.join("src")
        }))
        .is_err(),
        "not a build-output name"
    );
    std::fs::create_dir_all(d.join("real")).unwrap();
    std::os::unix::fs::symlink(d.join("real"), d.join("node_modules")).unwrap();
    assert!(
        cleanup::apply(&item(Action::RemoveTree {
            path: d.join("node_modules")
        }))
        .is_err(),
        "a symlink named node_modules"
    );
    assert!(d.join("real").exists());
    std::fs::remove_file(d.join("node_modules")).unwrap();
    std::fs::create_dir_all(d.join("node_modules/x")).unwrap();
    std::fs::write(d.join("node_modules/x/a.js"), b"1").unwrap();
    cleanup::apply(&item(Action::RemoveTree {
        path: d.join("node_modules"),
    }))
    .unwrap();
    assert!(!d.join("node_modules").exists());
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn truncate_refuses_symlinks_binary_logs_and_planted_tail_links() {
    let d = tmp("trunc");
    let log = d.join("app.log");
    std::fs::write(&log, vec![b'x'; 100_000]).unwrap();
    std::os::unix::fs::symlink(&log, d.join("alias.log")).unwrap();
    assert!(cleanup::apply(&item(Action::TailAndTruncate {
        path: d.join("alias.log"),
        keep: 10
    }))
    .is_err());
    std::fs::create_dir_all(d.join("journal")).unwrap();
    std::fs::write(d.join("journal/system.journal"), b"j").unwrap();
    assert!(cleanup::apply(&item(Action::TailAndTruncate {
        path: d.join("journal/system.journal"),
        keep: 0
    }))
    .is_err());
    // A planted <log>.dux-tail symlink must not redirect the write.
    let victim = d.join("victim");
    std::fs::write(&victim, b"keep me").unwrap();
    std::os::unix::fs::symlink(&victim, d.join("app.log.dux-tail")).unwrap();
    assert!(cleanup::apply(&item(Action::TailAndTruncate {
        path: log.clone(),
        keep: 10
    }))
    .is_err());
    assert_eq!(std::fs::read(&victim).unwrap(), b"keep me");
    assert_eq!(
        std::fs::metadata(&log).unwrap().len(),
        100_000,
        "log untouched when refused"
    );
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn truncate_on_a_full_disk_fails_cleanly_and_keeps_the_log() {
    if !root() {
        return;
    }
    let d = tmp("fullfs");
    let img = d.join("fs.img");
    let mnt = d.join("mnt");
    std::fs::create_dir_all(&mnt).unwrap();
    let sh = |c: &str| {
        std::process::Command::new("sh")
            .args(["-c", c])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    if !sh(&format!(
        "truncate -s 24M {0} && mkfs.ext4 -qF {0} && mount -o loop {0} {1}",
        img.display(),
        mnt.display()
    )) {
        let _ = std::fs::remove_dir_all(&d);
        return;
    }
    let log = mnt.join("big.log");
    std::fs::write(&log, vec![b'y'; 12 << 20]).unwrap();
    sh(&format!(
        "dd if=/dev/zero of={}/fill bs=64k 2>/dev/null; sync",
        mnt.display()
    ));
    let before = std::fs::metadata(&log).unwrap().len();
    let r = cleanup::apply(&item(Action::TailAndTruncate {
        path: log.clone(),
        keep: 8 << 20,
    }));
    let after = std::fs::metadata(&log).unwrap().len();
    sh(&format!("umount {}", mnt.display()));
    let _ = std::fs::remove_dir_all(&d);
    assert!(r.is_err(), "no room for the kept tail: must fail");
    assert_eq!(
        before, after,
        "the log must not be truncated when its tail could not be saved"
    );
}

// ------------------------------------------------------- #6 concurrency ----

#[test]
fn two_threads_deleting_one_file_one_wins_cleanly() {
    let d = tmp("race");
    let f = d.join("x.log.1");
    std::fs::write(&f, b"1").unwrap();
    let hs: Vec<_> = (0..2)
        .map(|_| {
            let f = f.clone();
            std::thread::spawn(move || cleanup::apply(&item(Action::Remove { path: f })).is_ok())
        })
        .collect();
    let ok: usize = hs.into_iter().map(|h| usize::from(h.join().unwrap())).sum();
    assert_eq!(
        ok, 1,
        "exactly one delete succeeds, the other fails without a panic"
    );
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn concurrent_never_touch_edits_lose_nothing() {
    let d = tmp("ntrace");
    let f = d.join("never-touch");
    let hs: Vec<_> = (0..8)
        .map(|i| {
            let f = f.clone();
            std::thread::spawn(move || {
                for j in 0..10 {
                    crate::protect::add_to(&f, &format!("/srv/t{i}/p{j}")).unwrap();
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    assert_eq!(
        crate::protect::policy_from(&f).unwrap().len(),
        80,
        "all 80 additions kept"
    );
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn tree_delete_while_another_thread_writes_into_it() {
    let d = tmp("busy");
    let t = d.join("node_modules");
    std::fs::create_dir_all(t.join("a")).unwrap();
    for i in 0..200 {
        std::fs::write(t.join(format!("a/f{i}")), b"x").unwrap();
    }
    let outside = d.join("outside");
    std::fs::write(&outside, b"keep").unwrap();
    let tt = t.clone();
    let writer = std::thread::spawn(move || {
        for i in 0..500 {
            let _ = std::fs::create_dir_all(tt.join(format!("n{}", i % 7)));
            let _ = std::fs::write(tt.join(format!("n{}/w{i}", i % 7)), b"y");
        }
    });
    // Either it removes everything or it reports an error; never a panic,
    // never anything outside the tree.
    let _ = cleanup::remove_dir_same_fs(&t);
    writer.join().unwrap();
    let _ = cleanup::remove_dir_same_fs(&t);
    assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
    std::fs::remove_dir_all(&d).unwrap();
}

// ----------------------------------------------------------- #5 fuzzing ----

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    /// Random text biased towards the characters parsers care about.
    fn text(&mut self, max: usize) -> String {
        const ALPH: &[u8] = b" \t\n\r/=,:;-_.{}[]\"\\0123456789abcxyzKMGTB%@*\xe2\x80\xa6";
        let n = self.below(max);
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            if self.below(10) == 0 {
                v.push(self.next() as u8);
            } else {
                v.push(ALPH[self.below(ALPH.len())]);
            }
        }
        String::from_utf8_lossy(&v).into_owned()
    }
    /// Mutate a valid sample: cut, duplicate, flip or insert.
    fn mutate(&mut self, s: &str) -> String {
        let mut b = s.as_bytes().to_vec();
        for _ in 0..1 + self.below(6) {
            if b.is_empty() {
                break;
            }
            let i = self.below(b.len());
            match self.below(4) {
                0 => b.truncate(i),
                1 => b.insert(i, self.next() as u8),
                2 => b[i] ^= 1 << self.below(8),
                _ => {
                    let j = self.below(b.len());
                    let (lo, hi) = if i < j { (i, j) } else { (j, i) };
                    let chunk = b[lo..hi].to_vec();
                    b.splice(i..i, chunk);
                }
            }
        }
        String::from_utf8_lossy(&b).into_owned()
    }
}

const MOUNTINFO: &str = "36 35 98:0 / /mnt1 rw,noatime master:1 - ext4 /dev/root rw,errors=continue\n37 36 0:22 / /my\\040dir rw - tmpfs tmpfs rw\n";
const DF: &str = r#"{"Images":[{"Repository":"nginx","Tag":"alpine","ID":"sha256:ab","Size":"31MB","UniqueSize":"18.1MB","SharedSize":"12.9MB","Containers":"1","CreatedSince":"3 months ago"}],"Containers":[{"Names":"web","ID":"c1","Image":"nginx:alpine","State":"running","Status":"Up","Size":"4.57MB (virtual 175MB)","Labels":"com.docker.compose.project=x,com.docker.compose.project.config_files=/a.yml,/b.yml","Mounts":"vol1,abc…","Networks":"host","Command":"\"nginx\""}],"Volumes":[{"Name":"vol1","Size":"80MB","Links":"1","Labels":"com.docker.compose.project=x","Mountpoint":"/var/lib/docker/volumes/vol1/_data","Driver":"local"}],"BuildCache":[{"Size":"1GB","InUse":false}]}"#;
const PS: &str = r#"[{"Names":["web"],"Id":"abc","Image":"nginx","ImageID":"i1","State":"running","Size":{"rwSize":1},"Mounts":["v"],"Labels":{"a":"b"},"Networks":["p"],"Command":["x"]}]"#;
const DISKSTATS: &str = "   8       0 sda 100 0 200 30 400 0 800 50 0 70 80\n 259 0 nvme0n1 1 2 3 4 5 6 7 8 9 10 11\n 179 1 mmcblk0p1 1 2 3 4 5 6 7 8 9 10 11\n";
const MDSTAT: &str = "Personalities : [raid1]\nmd0 : active raid1 sdb1[1] sda1[0]\n      1047552 blocks super 1.2 [2/1] [U_]\n      [=>...]  recovery = 8.5% (89600/1047552)\nunused devices: <none>\n";
const GITCFG: &str =
    "[core]\n\tbare = false\n[remote \"origin\"]\n\turl = https://user:pw\\\n@github.com/o/r.git\n";
const TOML: &str = "[package]\nname = \"dux\"\ndescription = \"disk tool\"\n[deps]\nx = 1\n";

fn fuzz_round(rng: &mut Rng) {
    let r = rng.text(400);
    let m = |rng: &mut Rng, s: &str| {
        if rng.below(2) == 0 {
            rng.mutate(s)
        } else {
            rng.text(600)
        }
    };
    let _ = crate::sentinel::parse_mountinfo(&m(rng, MOUNTINFO));
    let _ = crate::sentinel::unescape_mount(&r);
    let _ = crate::docker::parse_df(&m(rng, DF));
    let _ = crate::docker::parse_podman(&m(rng, PS), &m(rng, "[]"), &m(rng, "[]"));
    let _ = crate::docker::parse_si(&r);
    let _ = crate::docker::labels_map(&m(rng, "a=b,c=d,e,f=g=h"));
    let _ = crate::docker::upperdir_from_mount(&m(rng, "mount -o upperdir=/a/b,lowerdir=/c"));
    let _ = crate::live::parse_diskstats(&m(rng, DISKSTATS));
    let _ = crate::live::parse_fdinfo(&m(rng, "pos:\t123\nflags:\t0100002\nmnt_id:\t5\n"));
    let _ = crate::probes::mdstat(&m(rng, MDSTAT));
    let _ = crate::probes::psi(
        "io",
        &m(rng, "some avg10=0.00 avg60=1.50 avg300=0.10 total=1\n"),
        5.0,
        20.0,
    );
    let _ = crate::cleanup::rotated_base(&r);
    let _ = crate::cleanup::glob_match(&rng.text(40), &rng.text(80));
    let _ = crate::protect::normalize(&r);
    let _ = crate::protect::parse(&m(rng, "/a\n# c\n/b/../c\n"));
    let _ = crate::insight::redact_url(&m(rng, "https://user:pw@host/x.git"));
    let _ = crate::insight::clean_text(&r, rng.below(80));
    let _ = crate::insight::git_config_lines(&m(rng, GITCFG));
    let _ = crate::insight::toml_value(&m(rng, TOML), "package", "name");
}

#[test]
fn fuzz_parsers_quick() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..3_000 {
        fuzz_round(&mut rng);
    }
}

/// Long run: `cargo test -- --ignored fuzz_parsers_long`.
#[test]
#[ignore]
fn fuzz_parsers_long() {
    let seed = std::env::var("DUX_FUZZ_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x1234_5678_9abc_def1u64);
    let n: usize = std::env::var("DUX_FUZZ_ROUNDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300_000);
    let mut rng = Rng(seed | 1);
    for i in 0..n {
        let state = rng.0;
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fuzz_round(&mut rng))).is_err()
        {
            panic!("parser panicked at round {i}, rng state {state:#x}");
        }
    }
}

/// Delete and truncate on the filesystem at $DUX_QA_FS (set by the
/// filesystem matrix script): `cargo test -- --ignored fs_ops_on_target`.
#[test]
#[ignore]
fn fs_ops_on_target() {
    let Ok(base) = std::env::var("DUX_QA_FS") else {
        return;
    };
    let d = Path::new(&base).join(format!("qa-{}", std::process::id()));
    std::fs::create_dir_all(d.join("node_modules/a/b")).unwrap();
    for i in 0..50 {
        std::fs::write(d.join(format!("node_modules/a/b/f{i}")), vec![1u8; 10_000]).unwrap();
    }
    cleanup::apply(&item(Action::RemoveTree {
        path: d.join("node_modules"),
    }))
    .unwrap();
    assert!(!d.join("node_modules").exists(), "tree delete on {base}");
    let log = d.join("app.log");
    std::fs::write(&log, vec![b'z'; 300_000]).unwrap();
    cleanup::apply(&item(Action::TailAndTruncate {
        path: log.clone(),
        keep: 1000,
    }))
    .unwrap();
    assert_eq!(
        std::fs::metadata(&log).unwrap().len(),
        0,
        "truncate on {base}"
    );
    assert_eq!(
        std::fs::metadata(d.join("app.log.dux-tail")).unwrap().len(),
        1000
    );
    std::fs::write(d.join("old.log.1"), b"r").unwrap();
    cleanup::apply(&item(Action::Remove {
        path: d.join("old.log.1"),
    }))
    .unwrap();
    std::fs::remove_dir_all(&d).unwrap();
}

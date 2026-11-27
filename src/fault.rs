//! Deterministic crash boundaries, compiled out of default production builds.

/// Fault-build-only pressure input for the isolated recovery qualification.
/// Production binaries never read this environment variable.
#[cfg(not(feature = "fault-injection"))]
pub fn force_critical_pressure() -> bool {
    false
}

#[cfg(not(feature = "fault-injection"))]
pub fn force_elevated_pressure() -> bool {
    false
}

#[cfg(not(feature = "fault-injection"))]
pub fn force_cpu_elevated_pressure() -> bool {
    false
}

#[cfg(not(feature = "fault-injection"))]
pub fn force_high_load_elevated_pressure() -> bool {
    false
}

#[cfg(feature = "fault-injection")]
pub fn force_critical_pressure() -> bool {
    std::env::var_os("DUX_TEST_CRITICAL_PRESSURE_FILE")
        .is_some_and(|path| std::path::Path::new(&path).exists())
}

#[cfg(feature = "fault-injection")]
pub fn force_elevated_pressure() -> bool {
    std::env::var_os("DUX_TEST_ELEVATED_PRESSURE_FILE")
        .is_some_and(|path| std::path::Path::new(&path).exists())
}

#[cfg(feature = "fault-injection")]
pub fn force_cpu_elevated_pressure() -> bool {
    std::env::var_os("DUX_TEST_CPU_ELEVATED_PRESSURE_FILE")
        .is_some_and(|path| std::path::Path::new(&path).exists())
}

#[cfg(feature = "fault-injection")]
pub fn force_high_load_elevated_pressure() -> bool {
    std::env::var_os("DUX_TEST_HIGH_LOAD_ELEVATED_PRESSURE_FILE")
        .is_some_and(|path| std::path::Path::new(&path).exists())
}

#[cfg(not(feature = "fault-injection"))]
pub fn checkpoint(_stage: &str) {}

#[cfg(not(feature = "fault-injection"))]
pub fn delay_accounting_check() {}

#[cfg(not(feature = "fault-injection"))]
pub fn fail_scan(_stage: &str) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(feature = "fault-injection")]
pub fn checkpoint(stage: &str) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static TRIPPED: AtomicBool = AtomicBool::new(false);
    if std::env::var("DUX_TEST_PAUSE_AT").ok().as_deref() != Some(stage)
        || TRIPPED.swap(true, Ordering::SeqCst)
    {
        return;
    }
    let Some(marker) = std::env::var_os("DUX_TEST_MARKER") else {
        return;
    };
    if std::fs::write(marker, format!("{} {stage}\n", std::process::id())).is_ok() {
        // The test supervisor either kills us here or resumes with SIGCONT.
        // SAFETY: `raise` takes a valid signal number and no pointers; SIGSTOP
        // is available on Linux and its default action stops this test process.
        unsafe {
            libc::raise(libc::SIGSTOP);
        }
    }
}

#[cfg(feature = "fault-injection")]
pub fn delay_accounting_check() {
    let Some(milliseconds) = std::env::var("DUX_TEST_ACCOUNTING_CHECK_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (1..=10_000).contains(value))
    else {
        return;
    };
    std::thread::sleep(std::time::Duration::from_millis(milliseconds));
}

#[cfg(feature = "fault-injection")]
pub fn fail_scan(stage: &str) -> anyhow::Result<()> {
    let selected = std::env::var("DUX_TEST_FAIL_SCAN_AT").ok();
    let trigger = std::env::var_os("DUX_TEST_FAIL_SCAN_FILE");
    if selected.as_deref() == Some(stage)
        && trigger.is_some_and(|path| std::path::Path::new(&path).exists())
    {
        anyhow::bail!("test-injected scan failure at {stage}");
    }
    Ok(())
}

pub fn backup_step_pages() -> i32 {
    #[cfg(feature = "fault-injection")]
    if std::env::var("DUX_TEST_PAUSE_AT").ok().as_deref() == Some("scan.install_midway") {
        return 1;
    }
    256
}

#[cfg(all(test, feature = "fault-injection"))]
mod tests {
    use super::{
        backup_step_pages, checkpoint, fail_scan, force_cpu_elevated_pressure,
        force_critical_pressure, force_elevated_pressure, force_high_load_elevated_pressure,
    };
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::{Mutex, MutexGuard};
    use std::time::{SystemTime, UNIX_EPOCH};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path =
                std::env::temp_dir().join(format!("dux-fault-{}-{nonce}", std::process::id()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct EnvRestore {
        _lock: MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<OsString>)>,
    }

    impl EnvRestore {
        fn new(names: &[&'static str]) -> Self {
            let lock = ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let saved = names
                .iter()
                .map(|name| (*name, std::env::var_os(name)))
                .collect();
            Self { _lock: lock, saved }
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            for (name, value) in &self.saved {
                if let Some(value) = value {
                    std::env::set_var(name, value);
                } else {
                    std::env::remove_var(name);
                }
            }
        }
    }

    #[test]
    fn pressure_flags_follow_marker_file_lifecycle() {
        let _env = EnvRestore::new(&[
            "DUX_TEST_CRITICAL_PRESSURE_FILE",
            "DUX_TEST_ELEVATED_PRESSURE_FILE",
            "DUX_TEST_CPU_ELEVATED_PRESSURE_FILE",
            "DUX_TEST_HIGH_LOAD_ELEVATED_PRESSURE_FILE",
        ]);
        let dir = TempDir::new();
        let cases = [
            (
                "DUX_TEST_CRITICAL_PRESSURE_FILE",
                force_critical_pressure as fn() -> bool,
            ),
            ("DUX_TEST_ELEVATED_PRESSURE_FILE", force_elevated_pressure),
            (
                "DUX_TEST_CPU_ELEVATED_PRESSURE_FILE",
                force_cpu_elevated_pressure,
            ),
            (
                "DUX_TEST_HIGH_LOAD_ELEVATED_PRESSURE_FILE",
                force_high_load_elevated_pressure,
            ),
        ];

        for (name, check) in cases {
            let marker = dir.path().join(name);
            std::env::set_var(name, &marker);
            assert!(!check(), "{name} must be false while its marker is absent");
            std::fs::write(&marker, b"pressure").unwrap();
            assert!(check(), "{name} must be true while its marker exists");
            std::fs::remove_file(marker).unwrap();
            assert!(!check(), "{name} must reset after marker removal");
        }
    }

    #[test]
    fn scan_failure_requires_matching_stage_and_existing_trigger() {
        let _env = EnvRestore::new(&["DUX_TEST_FAIL_SCAN_AT", "DUX_TEST_FAIL_SCAN_FILE"]);
        std::env::remove_var("DUX_TEST_FAIL_SCAN_AT");
        std::env::remove_var("DUX_TEST_FAIL_SCAN_FILE");
        assert!(fail_scan("scan.build_complete").is_ok());

        let dir = TempDir::new();
        let marker = dir.path().join("trigger");
        std::env::set_var("DUX_TEST_FAIL_SCAN_AT", "scan.build_complete");
        std::env::set_var("DUX_TEST_FAIL_SCAN_FILE", &marker);
        assert!(fail_scan("scan.build_complete").is_ok());

        std::fs::write(&marker, b"fail").unwrap();
        assert!(fail_scan("scan.other_stage").is_ok());
        let error = fail_scan("scan.build_complete").unwrap_err();
        assert!(error.to_string().contains("test-injected scan failure"));
    }

    #[test]
    fn checkpoint_and_install_batch_size_respect_test_controls() {
        let _env = EnvRestore::new(&["DUX_TEST_PAUSE_AT", "DUX_TEST_MARKER"]);
        std::env::set_var("DUX_TEST_PAUSE_AT", "scan.install_midway");
        std::env::remove_var("DUX_TEST_MARKER");
        // With no supervisor marker, a matching checkpoint returns normally.
        checkpoint("scan.install_midway");
        assert_eq!(backup_step_pages(), 1);

        std::env::set_var("DUX_TEST_PAUSE_AT", "some.other.stage");
        assert_eq!(backup_step_pages(), 256);
        checkpoint("scan.install_midway");
    }
}

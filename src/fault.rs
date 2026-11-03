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
        unsafe {
            libc::raise(libc::SIGSTOP);
        }
    }
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

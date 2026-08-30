use crate::query;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Safety {
    DoNotDelete,
    ManagedOnly,
    Review,
    Safeish,
}

impl Safety {
    pub fn label(self) -> &'static str {
        match self {
            Self::DoNotDelete => "DO NOT DELETE",
            Self::ManagedOnly => "MANAGED ONLY",
            Self::Review => "REVIEW",
            Self::Safeish => "SAFE-ISH",
        }
    }
}

pub struct Insight {
    pub file_type: &'static str,
    pub belongs_to: &'static str,
    pub purpose: &'static str,
    pub importance: &'static str,
    pub safety: Safety,
    pub reason: &'static str,
    pub action: &'static str,
}

fn insight(
    file_type: &'static str,
    belongs_to: &'static str,
    purpose: &'static str,
    importance: &'static str,
    safety: Safety,
    reason: &'static str,
    action: &'static str,
) -> Insight {
    Insight {
        file_type,
        belongs_to,
        purpose,
        importance,
        safety,
        reason,
        action,
    }
}

/// Conservative file provenance/safety classification. Rules are ordered from
/// most dangerous/specific to generic. "Safe-ish" still names the owning tool;
/// unknown files are always REVIEW, never guessed safe.
pub fn classify(row: &query::Row) -> Insight {
    classify_path(&row.path, row.kind)
}

pub fn classify_path(p: &str, kind: char) -> Insight {
    let lower = p.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);

    if matches!(p, "/swap.img" | "/swapfile") || name.starts_with("swapfile") {
        return insight(
            "swap backing file",
            "Linux virtual memory",
            "Provides swap space when RAM pressure rises.",
            "Critical while enabled",
            Safety::DoNotDelete,
            "Deleting active swap can destabilize or crash workloads.",
            "Check `swapon --show`; use `swapoff PATH`, update /etc/fstab, then remove only if capacity is intentionally retired.",
        );
    }
    if p.starts_with("/var/lib/postgresql/") {
        return insight(
            "database data file",
            "PostgreSQL",
            "Stores live database pages, indexes, or transaction state.",
            "Critical application data",
            Safety::DoNotDelete,
            "Direct deletion corrupts the PostgreSQL cluster.",
            "Use SQL retention, DROP/VACUUM, partition management, or a documented PostgreSQL backup/restore procedure.",
        );
    }
    if p.starts_with("/var/lib/mysql/") || p.starts_with("/var/lib/mariadb/") {
        return insight(
            "database data file",
            "MySQL / MariaDB",
            "Stores live tables, indexes, or database logs.",
            "Critical application data",
            Safety::DoNotDelete,
            "Direct deletion can corrupt the database instance.",
            "Use SQL retention/table maintenance or the database's documented administration tools.",
        );
    }
    if p.starts_with("/var/lib/containerd/")
        || p.starts_with("/var/lib/docker/")
        || p.starts_with("/var/lib/containers/")
    {
        return insight(
            "container runtime data",
            "Docker / containerd / Podman",
            "Stores image content, snapshots, writable layers, or runtime metadata.",
            "Runtime-managed",
            Safety::ManagedOnly,
            "Deleting individual runtime files breaks reference accounting and may damage running containers.",
            "Use `dux docker` to identify ownership, then the runtime's image/container/builder prune commands after verification.",
        );
    }
    if lower.contains("/.git/objects/pack/") && name.ends_with(".pack") {
        return insight(
            "Git pack object",
            "Git repository history",
            "Compressed commits, trees, and file contents used by the repository.",
            "Critical to that repository",
            Safety::DoNotDelete,
            "Deleting a pack directly can corrupt repository history.",
            "Use `git count-objects -vH`, expire only intended refs, then run `git gc`; clone/backup before destructive history cleanup.",
        );
    }
    if lower.contains("/.terraform/providers/") {
        return insight(
            "Terraform provider",
            "Terraform working directory",
            "Downloaded provider executable required by `terraform plan/apply`.",
            "Reproducible dependency",
            Safety::Safeish,
            "It can be downloaded again, but removing it makes the next Terraform run reinitialize.",
            "When no Terraform process is active, remove the containing `.terraform` directory and run `terraform init` later; keep lock files.",
        );
    }
    if lower.contains("/target/debug/") || lower.contains("/target/release/") {
        return insight(
            "Rust build artifact",
            "Cargo / Rust build output",
            "Compiler output, dependency binary, incremental graph, or linked build product.",
            "Reproducible build data",
            Safety::Safeish,
            "It can be rebuilt from source, though cleanup makes the next build slower.",
            "Ensure no build/test is running; use `cargo clean` in the owning project instead of deleting individual dependency files.",
        );
    }
    if lower.contains("/.venv/") || lower.contains("/venv/") {
        return insight(
            "virtualenv dependency",
            "Python virtual environment",
            "Installed Python package or native library for one application environment.",
            "Usually reproducible",
            Safety::ManagedOnly,
            "Deleting one library leaves the environment inconsistent.",
            "If the environment is unused/reproducible, remove the whole venv and recreate it from requirements/lock files.",
        );
    }
    if lower.contains("/.cache/ms-playwright/") {
        return insight(
            "browser cache",
            "Microsoft Playwright",
            "Downloaded browser runtime used by browser automation/tests.",
            "Reproducible cache",
            Safety::Safeish,
            "Playwright can reinstall it, but active tests may depend on this exact browser version.",
            "Ensure no Playwright job is running; remove unused browser versions or run Playwright's supported install/uninstall workflow.",
        );
    }
    if lower.contains("/.codex/packages/standalone/releases/") {
        return insight(
            "tool release cache",
            "Codex installation",
            "Cached standalone Codex release binary.",
            "Reproducible; current version may be active",
            Safety::Review,
            "Removing the active release can break the installed command.",
            "Confirm the currently selected release, then remove only older unreferenced release directories.",
        );
    }
    if lower.contains("/.local/share/rtk/") && name.ends_with(".db") {
        return insight(
            "history database",
            "RTK",
            "Stores RTK local history/state for the owning user.",
            "User application history",
            Safety::Review,
            "Deleting it may erase history or state even if RTK recreates an empty database.",
            "Use RTK retention/export settings if available; otherwise stop RTK, back up the database, then remove only if losing its history is acceptable.",
        );
    }
    if lower.contains("/.local/share/opencode/") && name.ends_with(".db") {
        return insight(
            "application database",
            "OpenCode",
            "Stores OpenCode local sessions, history, metadata, or application state.",
            "User application state",
            Safety::Review,
            "It may contain the only local copy of useful session/history data.",
            "Use OpenCode's cleanup/export features or retention policy; stop the application and back up the DB before manual removal.",
        );
    }
    if lower.contains("/backup/")
        || name.ends_with(".tar.gz")
        || name.ends_with(".tgz")
        || name.ends_with(".tar")
    {
        return insight(
            "backup / archive",
            "Backup or migration workflow",
            "Point-in-time copy kept for recovery, transfer, or rollback.",
            "Potentially unique recovery data",
            Safety::Review,
            "Age and size do not prove another valid copy exists.",
            "Verify checksum, retention policy, restore value, and at least one independent copy before deleting; remove duplicate/expired sets as a unit.",
        );
    }
    if p.starts_with("/var/log/") || p.starts_with("/run/log/") || name.ends_with(".log") {
        let rotated = name.ends_with(".1")
            || name.ends_with(".gz")
            || name
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit())
                .count()
                > 0;
        if lower.contains("audit") {
            return insight(
                "audit log",
                if lower.contains("/vault/") {
                    "HashiCorp Vault audit logging"
                } else {
                    "Security/compliance logging"
                },
                "Records security-sensitive activity for investigation or compliance.",
                "Policy-controlled evidence",
                Safety::Review,
                "Manual deletion can violate retention requirements or remove incident evidence.",
                "Check the service/audit retention policy; rotate and vacuum through the logging system after archival if required.",
            );
        }
        if lower.contains("/xhelix/") {
            return insight(
                "application output log",
                "XHelix",
                "Captures XHelix service stdout/stderr or application diagnostics.",
                "Operational diagnostics",
                Safety::ManagedOnly,
                "The service may still hold it open; direct deletion may not free space and loses diagnostics.",
                "Configure XHelix/logrotate retention, rotate the file, and use `dux leaks` if deleted space remains allocated.",
            );
        }
        return if rotated {
            insight(
                "rotated log",
                "System or application logging",
                "Older log generation retained for troubleshooting.",
                "Historical diagnostics",
                Safety::Safeish,
                "Usually not actively written, but may be required by retention policy.",
                "Prefer logrotate/journal retention settings; verify the date and policy before removing old generations.",
            )
        } else {
            insight(
                "active log",
                "System or application logging",
                "Current diagnostic/output stream, possibly still open by a process.",
                "Operationally important",
                Safety::ManagedOnly,
                "Deleting/truncating an active log can lose evidence and may not free space if the process keeps it open.",
                "Identify the service, fix verbosity/retention, and rotate through logrotate or the service; use `dux leaks` for deleted-open files.",
            )
        };
    }
    if name.ends_with(".db") || name.ends_with(".sqlite") || name.ends_with(".sqlite3") {
        return insight(
            "application database",
            "Application named in parent path",
            "Stores application state, history, cache metadata, or user data.",
            "Unknown until application is identified",
            Safety::Review,
            "A `.db` file may be disposable cache or the application's only durable state.",
            "Stop/identify the owning application, inspect its retention/export options and backups; do not remove while open.",
        );
    }
    if p.starts_with("/usr/bin/") || p.starts_with("/usr/sbin/") || p.starts_with("/usr/local/bin/")
    {
        let owner = if name == "vault" {
            "HashiCorp Vault installation"
        } else if name == "openobserve" {
            "OpenObserve installation"
        } else if name == "alloy" {
            "Grafana Alloy installation"
        } else {
            "Operating system or installed application"
        };
        return insight(
            "installed executable",
            owner,
            "Program binary invoked by users, services, or automation.",
            "Installed software",
            Safety::ManagedOnly,
            "Direct deletion bypasses package ownership and can break services or upgrades.",
            "Find package/service ownership, then uninstall with the package manager or application installer.",
        );
    }
    if p.starts_with("/tmp/") || p.starts_with("/var/tmp/") {
        return insight(
            "temporary workspace file",
            "A process, migration, build, or interactive session",
            "Temporary archive, scratch output, download, or build artifact.",
            "Often disposable, ownership must be verified",
            Safety::Review,
            "Files in temporary directories can still belong to running jobs or pending migrations.",
            "Check owner, age, open handles (`lsof -- PATH`) and related process/job; delete only when the workflow is finished.",
        );
    }

    let file_type = if name.ends_with(".so") || name.contains(".so.") {
        "shared library"
    } else if name.ends_with(".img") || name.ends_with(".iso") {
        "disk image"
    } else if name.ends_with(".zip") || name.ends_with(".gz") || name.ends_with(".xz") {
        "compressed archive"
    } else if kind == 'd' {
        "directory"
    } else {
        "regular file"
    };
    insight(
        file_type,
        "Unknown / path-dependent application",
        "No high-confidence ownership rule matched this path.",
        "Unknown",
        Safety::Review,
        "Size, age, and extension alone cannot prove a file is disposable.",
        "Inspect the parent directory, owner, open handles, package ownership, backups, and application documentation before removal.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str) -> query::Row {
        query::Row {
            path: path.into(),
            size: 1,
            inodes: 1,
            mtime: 0,
            kind: 'f',
            uid: 0,
        }
    }

    #[test]
    fn dangerous_managed_data_is_never_called_safe() {
        for path in [
            "/swap.img",
            "/var/lib/postgresql/16/main/base/1/2",
            "/var/lib/containerd/io.containerd.snapshotter.v1.overlayfs/snapshots/1/fs/x",
            "/repo/.git/objects/pack/a.pack",
        ] {
            let safety = classify(&row(path)).safety;
            assert!(matches!(safety, Safety::DoNotDelete | Safety::ManagedOnly));
        }
    }

    #[test]
    fn reproducible_caches_name_the_owning_tool() {
        let terraform = classify(&row("/work/.terraform/providers/example"));
        assert_eq!(terraform.safety, Safety::Safeish);
        assert!(terraform.belongs_to.contains("Terraform"));
        let browser = classify(&row("/root/.cache/ms-playwright/chromium/chrome"));
        assert_eq!(browser.safety, Safety::Safeish);
        let cargo = classify(&row("/work/target/debug/deps/app-123"));
        assert_eq!(cargo.belongs_to, "Cargo / Rust build output");
    }
}

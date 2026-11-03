//! Optional reverse-SSH supervisor, run separately with `dux tunnel run`.
//!
//! A dux host behind NAT can't be reached inbound, so it dials OUT to a home
//! endpoint the operator controls and opens a reverse forward
//! (`ssh -R <port>:localhost:22`). The operator then reaches the box by hopping
//! through the home endpoint (`ProxyJump`) to the host's *own* sshd — dux never
//! binds a shell itself; SSH does the auth.
//!
//! Nothing secret or operator-specific is compiled into the executable.
//! Endpoints live in a root-owned `0600` config file and the private key stays
//! in a separate `0600` file. Both are required to activate the optional tunnel.

use anyhow::{bail, Context, Result};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Root-owned operator configuration. No remote endpoint is embedded in the
/// executable; the optional service remains inert until both this and a key
/// have been provisioned.
pub const CONFIG_PATH: &str = "/etc/dux/tunnel.conf";
/// Operator-verified SSH host keys for the configured home endpoints.
pub const KNOWN_HOSTS_PATH: &str = "/etc/dux/known_hosts";
/// The login user on the home endpoints (restricted to tunnel-only there).
pub const USER: &str = "dux";
/// Fixed path to the tunnel identity. NOT embedded in the binary — see module
/// docs. It is required by the explicitly enabled optional tunnel service.
pub const KEY_PATH: &str = "/etc/dux/ed25519";
/// Fallback local sshd port if auto-detection fails. The tunnel actually
/// forwards to whatever `local_ssh_port()` finds (this box may run ssh on a
/// non-standard port), falling back to this.
pub const LOCAL_PORT: u16 = 22;
/// SSH keepalive: probe every N seconds, drop after 3 missed (see ssh_args).
pub const KEEPALIVE_SECS: u32 = 30;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Endpoint {
    host: String,
    port: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TunnelConfig {
    homes: Vec<Endpoint>,
}

fn parse_config(input: &str) -> Result<TunnelConfig> {
    let mut homes = Vec::new();
    for (line_no, line) in input.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 2 {
            bail!(
                "{}:{}: expected exactly HOST PORT",
                CONFIG_PATH,
                line_no + 1
            );
        }
        let host = fields[0];
        let valid_domain = !host.is_empty()
            && host.len() <= 253
            && !host.starts_with('-')
            && host.is_ascii()
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':'))
            && host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            });
        let valid_host = valid_domain || host.parse::<std::net::IpAddr>().is_ok();
        if !valid_host {
            bail!("{}:{}: invalid host", CONFIG_PATH, line_no + 1);
        }
        let port = fields[1]
            .parse::<u16>()
            .ok()
            .filter(|port| *port != 0)
            .with_context(|| format!("{}:{}: invalid TCP port", CONFIG_PATH, line_no + 1))?;
        if homes.iter().any(|endpoint: &Endpoint| {
            endpoint.host.eq_ignore_ascii_case(host) && endpoint.port == port
        }) {
            bail!("{}:{}: duplicate endpoint", CONFIG_PATH, line_no + 1);
        }
        homes.push(Endpoint {
            host: host.to_string(),
            port,
        });
        if homes.len() > 8 {
            bail!("{}: at most 8 endpoints are allowed", CONFIG_PATH);
        }
    }
    if homes.is_empty() {
        bail!("{}: configure at least one HOST PORT endpoint", CONFIG_PATH);
    }
    Ok(TunnelConfig { homes })
}

fn load_config() -> Result<TunnelConfig> {
    load_config_from(Path::new(CONFIG_PATH), 0)
}

fn load_config_from(path: &Path, expected_uid: u32) -> Result<TunnelConfig> {
    let path_text = path.display().to_string();
    let dir = path.parent().context("tunnel config path has no parent")?;
    let dir_meta = std::fs::symlink_metadata(dir)
        .with_context(|| format!("checking tunnel config directory {}", dir.display()))?;
    if !dir_meta.file_type().is_dir()
        || dir_meta.uid() != expected_uid
        || dir_meta.mode() & 0o7777 != 0o700
    {
        bail!(
            "tunnel config directory {} must be owned by uid {expected_uid} with mode 0700",
            dir.display(),
        );
    }
    // Open without following symlinks, then validate and read from the same
    // descriptor so replacing the pathname cannot race the metadata check.
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .with_context(|| format!("opening tunnel config {path_text}"))?;
    let meta = file
        .metadata()
        .with_context(|| format!("checking tunnel config {path_text}"))?;
    validate_protected_file_metadata(&path_text, &meta, expected_uid)?;
    let mut text = String::new();
    std::io::Read::read_to_string(&mut file, &mut text)
        .with_context(|| format!("reading tunnel config {path_text}"))?;
    parse_config(&text)
}

/// Break-glass shell: dux supervises a dropbear SSH server on this loopback port,
/// running ALONGSIDE the system sshd (not a replacement). The tunnel forwards a
/// second remote port to it, so if the main sshd is down you can still get in via
/// dropbear over the tunnel. Bound to 127.0.0.1 only; key-authed against the
/// target user's ~/.ssh/authorized_keys (same keys as sshd — no separate mgmt),
/// passwords disabled. NOT unauthenticated.
pub const DROPBEAR_LOCAL_PORT: u16 = 2022;
/// Dropbear host key (dropbear's own format), generated by `dux tunnel keygen`.
pub const DROPBEAR_KEY_PATH: &str = "/etc/dux/dropbear_key";
/// dropbear/dropbearkey come from the `dropbear-bin` package (binaries only, no
/// competing service). Absent = break-glass shell simply isn't offered.
const DROPBEAR_BIN: &str = "/usr/sbin/dropbear";
const DROPBEARKEY_BIN: &str = "/usr/bin/dropbearkey";
/// Run only the distribution-provided OpenSSH client. This process may run as
/// root, so resolving `ssh` through an inherited PATH is an execution boundary.
const SSH_BIN: &str = "/usr/bin/ssh";
/// Read the installed server configuration without trusting an inherited PATH.
const SSHD_BIN: &str = "/usr/sbin/sshd";
/// Generate tunnel identity keys with the packaged OpenSSH utility.
const SSH_KEYGEN_BIN: &str = "/usr/bin/ssh-keygen";

/// Reconnect backoff bounds. A tunnel that dies is retried fast at first, then
/// backs off so a home endpoint that's down doesn't get hammered.
const BACKOFF_MIN: Duration = Duration::from_secs(2);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// A connection that stayed up at least this long is "healthy" — its next
/// failure resets the backoff to the minimum instead of continuing to grow.
const HEALTHY_AFTER: Duration = Duration::from_secs(60);

/// Is the tunnel configured on this host? All operator-provisioned files must
/// exist before starting. This is the opt-in: a stock install remains inert.
pub fn configured() -> bool {
    Path::new(KEY_PATH).exists()
        && Path::new(CONFIG_PATH).exists()
        && Path::new(KNOWN_HOSTS_PATH).exists()
}

fn entry_exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("inspecting {}", path.display())),
    }
}

fn remove_entry_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("removing {}", path.display())),
    }
}

fn prepare_identity_paths(private: &Path, public: &Path, force: bool) -> Result<()> {
    let private_exists = entry_exists(private)?;
    let public_exists = entry_exists(public)?;
    if (private_exists || public_exists) && !force {
        bail!(
            "tunnel key material already exists at {} or {}; refusing to overwrite (use --force to replace)",
            private.display(),
            public.display()
        );
    }
    if force {
        remove_entry_if_present(private)?;
        remove_entry_if_present(public)?;
    }
    Ok(())
}

/// FNV-1a over the given bytes. A fixed hash (not `DefaultHasher`, whose output
/// isn't guaranteed stable across toolchains) so a host's derived port is the
/// same for the life of the box regardless of which dux build computed it.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Map a seed to a stable port in 20000–59999.
fn derive_port(seed: &str) -> u16 {
    20000 + (fnv1a(seed.as_bytes()) % 40000) as u16
}

/// The stable remote port this host binds on the home endpoint for the MAIN sshd,
/// derived from its identity so the SAME binary on many hosts picks DISTINCT,
/// repeatable ports (no per-host config, no collisions on reconnect).
///
/// `machine_id` should be the contents of /etc/machine-id (falling back to the
/// hostname); taken as an argument so it's unit-testable.
pub fn remote_port(machine_id: &str) -> u16 {
    derive_port(machine_id.trim())
}

/// The stable remote port for the dropbear BREAK-GLASS shell — a second port,
/// distinct from [`remote_port`], derived from the same identity with a salt so
/// both are stable and (almost surely) different.
pub fn dropbear_remote_port(machine_id: &str) -> u16 {
    let p = derive_port(&format!("{}:dropbear", machine_id.trim()));
    // vanishingly unlikely, but never collide with the sshd port
    if p == remote_port(machine_id) {
        20000 + ((p as u32 + 1 - 20000) % 40000) as u16
    } else {
        p
    }
}

/// True when the break-glass shell can run: the dropbear binary is installed and
/// a host key has been generated. Absent binary/key = feature simply off.
fn dropbear_available() -> bool {
    Path::new(DROPBEAR_BIN).exists() && secret_file_usable(DROPBEAR_KEY_PATH).is_ok()
}

/// The local sshd port to forward the tunnel to. Asks `sshd -T` what it's
/// configured to listen on (so a box running ssh on a non-standard port still
/// exposes a working shell), falling back to [`LOCAL_PORT`] (22) if detection
/// fails. Best-effort: any error just yields the fallback.
fn local_ssh_port() -> u16 {
    if let Ok(out) = Command::new(SSHD_BIN).arg("-T").output() {
        if out.status.success() {
            // `sshd -T` prints one `port <n>` line per configured port.
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if let Some(p) = line.strip_prefix("port ") {
                    if let Ok(n) = p.trim().parse::<u16>() {
                        return n;
                    }
                }
            }
        }
    }
    LOCAL_PORT
}

/// This host's identity for port derivation: /etc/machine-id, else the hostname,
/// else a fixed fallback (all hosts then share a port — logged as a warning).
fn host_identity() -> String {
    if let Ok(id) = std::fs::read_to_string("/etc/machine-id") {
        if !id.trim().is_empty() {
            return id.trim().to_string();
        }
    }
    if let Ok(h) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
        if !h.trim().is_empty() {
            return h.trim().to_string();
        }
    }
    "dux-fallback".to_string()
}

/// Build the argv for the reverse-tunnel `ssh` invocation to `home`.
///
/// - `-N -T`: no remote command, no pty — this connection ONLY carries the
///   forward. (The operator's actual shell is a separate SSH through the tunnel.)
/// - `BatchMode`: never prompt — unattended, key-only.
/// - `ExitOnForwardFailure`: if the port can't bind on the home end, drop the
///   whole connection so the supervisor retries instead of sitting up uselessly.
/// - `ServerAlive*`: detect a dead link and exit within ~3×keepalive.
/// - Strict host-key checking uses only the operator-provisioned known-hosts
///   file; an endpoint key is never trusted automatically on first contact.
pub fn ssh_args(
    home: &str,
    home_port: u16,
    remote_port: u16,
    local_port: u16,
    dropbear_remote: Option<u16>,
    key_path: &str,
) -> Vec<String> {
    // Request loopback explicitly so this matches the generated per-key
    // `permitlisten` restriction instead of relying on sshd defaults.
    let forward = format!("localhost:{remote_port}:localhost:{local_port}");
    let dest = format!("{USER}@{home}");
    let hport = home_port.to_string();
    let mut a: Vec<String> = [
        "-N",
        "-T",
        // The tunnel has explicit endpoint, key, and host-key configuration.
        // Do not let ambient root SSH config inject ProxyCommand/LocalCommand.
        "-F",
        "/dev/null",
        "-o",
        "BatchMode=yes",
        "-o",
        "ExitOnForwardFailure=yes",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "UpdateHostKeys=no",
        "-o",
        &format!("UserKnownHostsFile={KNOWN_HOSTS_PATH}"),
        "-o",
        "GlobalKnownHostsFile=/dev/null",
        // Do not offer unrelated root keys or agent identities to the endpoint;
        // this tunnel is authorized only by its explicitly configured identity.
        "-o",
        "IdentitiesOnly=yes",
        // Health check: app-level probe every KEEPALIVE_SECS, drop after 3 missed
        // (~90s to notice a dead link), plus TCP-level keepalive as a backstop.
        "-o",
        &format!("ServerAliveInterval={KEEPALIVE_SECS}"),
        "-o",
        "ServerAliveCountMax=3",
        "-o",
        "TCPKeepAlive=yes",
        // Bail fast on a stuck connect so the supervisor can retry/failover.
        "-o",
        "ConnectTimeout=15",
        // The home endpoint's sshd port (may be non-standard).
        "-p",
        &hport,
        "-i",
        key_path,
        "-R",
        &forward,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    // Second forward to the dux break-glass dropbear, so sshd AND dropbear are both
    // reachable through this one tunnel simultaneously. ExitOnForwardFailure only
    // fails on a REMOTE bind failure, so a momentarily-down dropbear won't drop the
    // whole tunnel — its forward just has nothing behind it until it's back.
    if let Some(dbr) = dropbear_remote {
        a.push("-R".into());
        a.push(format!("localhost:{dbr}:localhost:{DROPBEAR_LOCAL_PORT}"));
    }
    a.push(dest);
    a
}

/// Next backoff after a connection to `home` ended. `uptime` is how long that
/// connection stayed up; a healthy connection resets to the minimum, otherwise
/// the delay doubles up to the cap. Pure so the policy is testable.
fn next_backoff(prev: Duration, uptime: Duration) -> Duration {
    if uptime >= HEALTHY_AFTER {
        BACKOFF_MIN
    } else {
        (prev * 2).min(BACKOFF_MAX).max(BACKOFF_MIN)
    }
}

/// Stop a supervised child and always collect its exit status. Dropping a
/// `Child` without waiting can leave a terminated process as a zombie.
fn kill_and_reap(child: &mut Child) -> std::io::Result<ExitStatus> {
    let _ = child.kill();
    child.wait()
}

/// Verify the key exists and is not group/other-readable. ssh refuses a loose
/// key anyway; we check first to emit a clear, actionable error instead of a
/// cryptic ssh failure in a reconnect loop.
fn validate_protected_file_metadata(
    path: &str,
    meta: &std::fs::Metadata,
    expected_uid: u32,
) -> Result<()> {
    if !meta.file_type().is_file() {
        bail!("protected file {path} must be a regular file, not a symlink or special file");
    }
    if meta.uid() != expected_uid {
        bail!(
            "protected file {path} must be owned by uid {expected_uid} (found uid {})",
            meta.uid()
        );
    }
    let mode = meta.permissions().mode();
    if mode & 0o7777 != 0o600 {
        bail!(
            "protected file {path} must have mode 600 (found {:o}); run `chmod 600 {path}`",
            mode & 0o7777
        );
    }
    Ok(())
}

fn key_usable(path: &str) -> Result<()> {
    // lstat rather than stat: a symlink can otherwise make the service consume
    // an unrelated private key placed elsewhere on the machine.
    let meta = std::fs::symlink_metadata(path)
        .with_context(|| format!("tunnel key {path} not found — run `dux tunnel keygen`"))?;
    validate_protected_file_metadata(path, &meta, 0)
}

fn secret_file_usable(path: &str) -> Result<()> {
    let meta =
        std::fs::symlink_metadata(path).with_context(|| format!("secret file {path} not found"))?;
    validate_protected_file_metadata(path, &meta, 0)
}

fn known_hosts_usable(path: &str, expected_uid: u32) -> Result<()> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .with_context(|| format!("opening SSH known-hosts file {path}"))?;
    let meta = file
        .metadata()
        .with_context(|| format!("checking SSH known-hosts file {path}"))?;
    validate_protected_file_metadata(path, &meta, expected_uid)?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .with_context(|| format!("reading SSH known-hosts file {path}"))?;
    anyhow::ensure!(
        contents.lines().any(|line| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with('#')
        }),
        "SSH known-hosts file {path} contains no host keys"
    );
    Ok(())
}

/// Build the restricted authorized_keys entry for this host's tunnel key.
/// `restrict` disables forwarding first; only the required remote listeners
/// are re-enabled, each on this host's stable loopback port. `permitopen`
/// confines the other forwarding direction to a reserved, non-routable name.
fn authorized_keys_line(pub_key: &str, ssh_port: u16, dropbear_port: Option<u16>) -> String {
    let mut options = format!("restrict,port-forwarding,permitlisten=\"localhost:{ssh_port}\"");
    if let Some(port) = dropbear_port {
        options.push_str(&format!(",permitlisten=\"localhost:{port}\""));
    }
    format!("{options},permitopen=\"dux-tunnel-disabled.invalid:1\",command=\"true\" {pub_key}")
}

/// Run the tunnel supervisor until `shutdown` is set. Maintains ONE persistent
/// reverse tunnel PER home endpoint concurrently (not failover) — so the host
/// stays reachable through EVERY home at once, and losing one home doesn't take
/// away the others. Blocks the calling thread until all per-home workers stop.
pub fn supervise(shutdown: Arc<AtomicBool>) {
    if let Err(e) = key_usable(KEY_PATH) {
        tracing::error!("tunnel disabled: {e:#}");
        return;
    }
    if let Err(e) = known_hosts_usable(KNOWN_HOSTS_PATH, 0) {
        tracing::error!("tunnel disabled: {e:#}");
        return;
    }
    let config = match load_config() {
        Ok(config) => config,
        Err(error) => {
            tracing::error!("tunnel disabled: {error:#}");
            return;
        }
    };
    let id = host_identity();
    let port = remote_port(&id);
    let local = local_ssh_port();

    // Break-glass shell: if dropbear is installed + keyed, run it and forward a
    // second remote port to it, so sshd and dropbear are both reachable at once.
    let dropbear = if dropbear_available() {
        let dbr = dropbear_remote_port(&id);
        tracing::info!(
            "break-glass dropbear enabled: remote_port={dbr} -> localhost:{DROPBEAR_LOCAL_PORT}"
        );
        Some(dbr)
    } else {
        tracing::info!("break-glass dropbear not configured (no binary/key) — sshd tunnel only");
        None
    };

    tracing::info!(
        "reverse tunnel starting: homes={:?} remote_port={port} -> localhost:{local} \
         (one persistent tunnel per endpoint)",
        config
            .homes
            .iter()
            .map(|endpoint| (&endpoint.host, endpoint.port))
            .collect::<Vec<_>>()
    );

    let mut workers = Vec::new();

    // Only advertise the break-glass forward if its local listener supervisor
    // actually started. Otherwise OpenSSH could successfully bind a dead
    // remote listener and operators would see a port that can never connect.
    let dropbear = if let Some(port) = dropbear {
        let s = shutdown.clone();
        match std::thread::Builder::new()
            .name("dux-dropbear".into())
            .spawn(move || supervise_dropbear(s))
        {
            Ok(handle) => {
                workers.push(handle);
                Some(port)
            }
            Err(error) => {
                tracing::error!("break-glass: could not start supervisor: {error}");
                None
            }
        }
    } else {
        None
    };

    // One worker thread per home endpoint; each keeps its own tunnel up.
    for endpoint in config.homes {
        let home = endpoint.host;
        let worker_home = home.clone();
        let home_port = endpoint.port;
        let s = shutdown.clone();
        match std::thread::Builder::new()
            .name(format!("dux-tunnel-{home}"))
            .spawn(move || supervise_one(worker_home, home_port, port, local, dropbear, s))
        {
            Ok(h) => workers.push(h),
            Err(e) => tracing::error!("tunnel: could not start worker for {home}: {e}"),
        }
    }
    for h in workers {
        let _ = h.join();
    }
    tracing::info!("tunnel: all endpoints shut down");
}

/// Supervise the dropbear break-glass shell: run it in the foreground on
/// 127.0.0.1:[`DROPBEAR_LOCAL_PORT`], key-only (passwords disabled), restarting it
/// with backoff if it ever dies. Authenticates against each user's normal
/// ~/.ssh/authorized_keys, so it needs no separate key management.
fn supervise_dropbear(shutdown: Arc<AtomicBool>) {
    let bind = format!("127.0.0.1:{DROPBEAR_LOCAL_PORT}");
    let mut backoff = BACKOFF_MIN;
    while !shutdown.load(Ordering::Relaxed) {
        let started = Instant::now();
        tracing::info!("break-glass: starting dropbear on {bind}");
        // -F foreground, -E log to stderr, -s no password auth, -p bind, -r hostkey
        let mut child = match Command::new(DROPBEAR_BIN)
            .args(["-F", "-E", "-s", "-p", &bind, "-r", DROPBEAR_KEY_PATH])
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("break-glass: failed to spawn dropbear: {e}");
                sleep_interruptible(BACKOFF_MAX, &shutdown);
                continue;
            }
        };
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    tracing::warn!("break-glass: dropbear exited ({status})");
                    backoff = next_backoff(backoff, started.elapsed());
                    break;
                }
                Ok(None) => {
                    if shutdown.load(Ordering::Relaxed) {
                        if let Err(e) = kill_and_reap(&mut child) {
                            tracing::error!("break-glass: failed to reap dropbear child: {e}");
                        }
                        tracing::info!("break-glass: dropbear shutting down");
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
                Err(e) => {
                    tracing::error!("break-glass: wait failed: {e}");
                    if let Err(wait_error) = kill_and_reap(&mut child) {
                        tracing::error!("break-glass: failed to reap dropbear child: {wait_error}");
                    }
                    break;
                }
            }
        }
        sleep_interruptible(backoff, &shutdown);
    }
}

/// Maintain a single reverse tunnel to `home`, restarting the `ssh` child
/// whenever it exits, with backoff. Returns when `shutdown` is set.
fn supervise_one(
    home: String,
    home_port: u16,
    port: u16,
    local_port: u16,
    dropbear_remote: Option<u16>,
    shutdown: Arc<AtomicBool>,
) {
    let mut backoff = BACKOFF_MIN;
    while !shutdown.load(Ordering::Relaxed) {
        let args = ssh_args(
            &home,
            home_port,
            port,
            local_port,
            dropbear_remote,
            KEY_PATH,
        );
        let started = Instant::now();

        tracing::info!("tunnel: connecting to {home} (remote port {port})");
        let mut child = match Command::new(SSH_BIN).args(&args).spawn() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(
                    "tunnel: failed to spawn ssh for {home}: {e} (is openssh-client installed?)"
                );
                sleep_interruptible(BACKOFF_MAX, &shutdown);
                continue;
            }
        };

        // Wait for the child, polling shutdown so we can kill it on stop.
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let up = started.elapsed();
                    tracing::warn!("tunnel to {home} ended ({status}) after {up:?}");
                    backoff = next_backoff(backoff, up);
                    break;
                }
                Ok(None) => {
                    if shutdown.load(Ordering::Relaxed) {
                        if let Err(e) = kill_and_reap(&mut child) {
                            tracing::error!("tunnel: failed to reap ssh child for {home}: {e}");
                        }
                        tracing::info!("tunnel to {home}: shutting down");
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
                Err(e) => {
                    tracing::error!("tunnel: wait failed for {home}: {e}");
                    if let Err(wait_error) = kill_and_reap(&mut child) {
                        tracing::error!(
                            "tunnel: failed to reap ssh child for {home}: {wait_error}"
                        );
                    }
                    break;
                }
            }
        }

        sleep_interruptible(backoff, &shutdown);
    }
}

/// Sleep up to `dur`, waking early (every 200ms) if shutdown is requested.
fn sleep_interruptible(dur: Duration, shutdown: &AtomicBool) {
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(Duration::from_millis(200).min(deadline - Instant::now()));
    }
}

/// Spawn the optional supervisor if its identity is configured. Returns the
/// shutdown flag + join handle so the dedicated command can stop it,
/// or None if there's no key (tunnel not configured on this host).
pub fn spawn() -> Option<(Arc<AtomicBool>, std::thread::JoinHandle<()>)> {
    if !configured() {
        return None;
    }
    let shutdown = Arc::new(AtomicBool::new(false));
    let s = shutdown.clone();
    let handle = std::thread::Builder::new()
        .name("dux-tunnel".into())
        .spawn(move || supervise(s))
        .ok()?;
    Some((shutdown, handle))
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn stop_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

/// This process has its own lifetime and service privileges; an indexer never
/// starts it merely because a key happens to exist on the host.
pub fn run() -> Result<()> {
    key_usable(KEY_PATH)?;
    known_hosts_usable(KNOWN_HOSTS_PATH, 0)?;
    load_config()?;
    unsafe {
        libc::signal(
            libc::SIGTERM,
            stop_signal as *const () as libc::sighandler_t,
        );
        libc::signal(libc::SIGINT, stop_signal as *const () as libc::sighandler_t);
    }
    let (shutdown, handle) = spawn().context("cannot start tunnel supervisor")?;
    while !STOP.load(Ordering::SeqCst) && !handle.is_finished() {
        std::thread::sleep(Duration::from_millis(100));
    }
    shutdown.store(true, Ordering::SeqCst);
    handle
        .join()
        .map_err(|_| anyhow::anyhow!("tunnel supervisor panicked"))?;
    if !STOP.load(Ordering::SeqCst) {
        bail!("tunnel supervisor exited unexpectedly");
    }
    Ok(())
}

/// `dux tunnel keygen`: create the tunnel identity at KEY_PATH (0600) and print
/// the restricted `authorized_keys` line to paste on each home endpoint. Never
/// overwrites an existing key unless `force`.
pub fn keygen(force: bool) -> Result<()> {
    let dir = Path::new(KEY_PATH)
        .parent()
        .unwrap_or_else(|| Path::new("/etc/dux"));
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    // Directory 0700 root — it holds the private key.
    let dir_meta = std::fs::symlink_metadata(dir)
        .with_context(|| format!("checking tunnel key directory {}", dir.display()))?;
    if !dir_meta.file_type().is_dir() || dir_meta.uid() != 0 {
        bail!(
            "tunnel key directory {} must be a real directory owned by root",
            dir.display()
        );
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("securing tunnel key directory {}", dir.display()))?;

    let private_path = Path::new(KEY_PATH);
    let public_path = Path::new(&format!("{KEY_PATH}.pub")).to_path_buf();
    prepare_identity_paths(private_path, &public_path, force)?;

    let status = Command::new(SSH_KEYGEN_BIN)
        .args([
            "-t",
            "ed25519",
            "-N",
            "",
            "-C",
            "dux-tunnel",
            "-f",
            KEY_PATH,
        ])
        .status()
        .context("running ssh-keygen (is openssh-client installed?)")?;
    if !status.success() {
        bail!("ssh-keygen failed");
    }
    std::fs::set_permissions(KEY_PATH, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 600 {KEY_PATH}"))?;
    key_usable(KEY_PATH)?;

    let pub_key = std::fs::read_to_string(format!("{KEY_PATH}.pub"))
        .with_context(|| format!("reading {KEY_PATH}.pub"))?;
    let pub_key = pub_key.trim();
    let id = host_identity();
    let port = remote_port(&id);

    // Break-glass shell host key (dropbear's own format), if dropbear is installed.
    // Generated once; the tunnel forwards a second port to it.
    let dbr = if Path::new(DROPBEARKEY_BIN).exists() {
        if !entry_exists(Path::new(DROPBEAR_KEY_PATH))? {
            let st = Command::new(DROPBEARKEY_BIN)
                .args(["-t", "ed25519", "-f", DROPBEAR_KEY_PATH])
                .status();
            match st {
                Ok(s) if s.success() => {
                    if let Err(e) = std::fs::set_permissions(
                        DROPBEAR_KEY_PATH,
                        std::fs::Permissions::from_mode(0o600),
                    ) {
                        eprintln!("dux: securing dropbear host key failed: {e}");
                    }
                }
                _ => eprintln!("dux: dropbearkey failed — break-glass shell will be off"),
            }
        }
        match secret_file_usable(DROPBEAR_KEY_PATH) {
            Ok(()) => Some(dropbear_remote_port(&id)),
            Err(e) => {
                eprintln!("dux: dropbear host key is not safely usable: {e:#}");
                None
            }
        }
    } else {
        eprintln!("dux: dropbear not installed (apt install dropbear-bin) — break-glass shell off");
        None
    };

    println!("\nTunnel key created at {KEY_PATH} (0600).\n");
    match dbr {
        Some(d) => println!(
            "This host binds remote port {port} (sshd) and {d} (break-glass dropbear) \
             on each configured endpoint.\n"
        ),
        None => {
            println!("This host binds remote port {port} (sshd) on each configured endpoint.\n")
        }
    }
    println!("Configure operator-controlled endpoints in {CONFIG_PATH}, one `HOST PORT` per line.");
    println!("The file and its parent directory must be root-owned, with modes 0600 and 0700.");
    println!("On EACH configured endpoint, append this ONE line to");
    println!("~{USER}/.ssh/authorized_keys. It denies shell/PTY/agent/X11 access,");
    println!("limits reverse forwarding to this host's loopback ports, and blocks");
    println!("useful local forwarding. Keep");
    println!("GatewayPorts disabled (or set to clientspecified) on each endpoint:\n");
    println!("{}\n", authorized_keys_line(pub_key, port, dbr));
    println!("After reviewing the endpoints and config, enable the separate optional service:\n  systemctl enable --now dux-tunnel.service\n");
    println!("Then from your laptop, reach this box through one configured endpoint:\n");
    println!("  ssh -J root@<endpoint> -p {port} root@localhost      # via main sshd");
    if let Some(d) = dbr {
        println!("  ssh -J root@<endpoint> -p {d} root@localhost      # via break-glass dropbear");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_and_reap_collects_terminated_child() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id() as libc::pid_t;
        let status = kill_and_reap(&mut child).unwrap();

        assert!(!status.success(), "killed child unexpectedly succeeded");
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[test]
    fn keygen_refuses_existing_key_halves_and_dangling_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!("dux-keygen-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let private = dir.join("id_ed25519");
        let public = dir.join("id_ed25519.pub");
        prepare_identity_paths(&private, &public, false).unwrap();

        std::fs::write(&public, b"existing public key").unwrap();
        assert!(prepare_identity_paths(&private, &public, false).is_err());
        assert_eq!(std::fs::read(&public).unwrap(), b"existing public key");
        prepare_identity_paths(&private, &public, true).unwrap();
        assert!(!entry_exists(&private).unwrap());
        assert!(!entry_exists(&public).unwrap());

        let missing_target = dir.join("missing-target");
        symlink(&missing_target, &private).unwrap();
        assert!(prepare_identity_paths(&private, &public, false).is_err());
        assert!(entry_exists(&private).unwrap());
        prepare_identity_paths(&private, &public, true).unwrap();
        assert!(!entry_exists(&private).unwrap());
        assert!(!missing_target.exists());

        std::fs::create_dir(&public).unwrap();
        assert!(prepare_identity_paths(&private, &public, true).is_err());
        assert!(public.is_dir());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tunnel_config_accepts_valid_dns_and_ip_endpoints() {
        let parsed = parse_config(
            "# operator-controlled endpoints\nrelay.example.net 22000\n203.0.113.9 22\n2001:db8::1 2222\n",
        )
        .unwrap();
        assert_eq!(parsed.homes.len(), 3);
        assert_eq!(parsed.homes[0].host, "relay.example.net");
        assert_eq!(parsed.homes[2].host, "2001:db8::1");
    }

    #[test]
    fn tunnel_config_rejects_ambiguous_or_unsafe_endpoints() {
        for invalid in [
            "",
            "relay.example.net",
            "relay.example.net 0",
            "relay.example.net 65536",
            "-oProxyCommand=evil 22",
            "bad..example 22",
            "relay.example.net 22\nrelay.example.net 22",
            &format!("{}.example 22", "a".repeat(64)),
            "relay.example.net 22\nRELAY.EXAMPLE.NET 22",
            "relay.example.net 22 extra",
        ] {
            assert!(parse_config(invalid).is_err(), "accepted {invalid:?}");
        }
        let too_many = (0..9)
            .map(|n| format!("relay{n}.example 22"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(parse_config(&too_many).is_err());
    }

    #[test]
    fn tunnel_config_file_requires_private_nofollow_rooted_storage() {
        use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
        let dir =
            std::env::temp_dir().join(format!("dux-tunnel-config-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.join("tunnel.conf");
        std::fs::write(&path, "relay.example.net 22000\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let uid = std::fs::metadata(&dir).unwrap().uid();
        assert_eq!(load_config_from(&path, uid).unwrap().homes.len(), 1);
        assert!(
            load_config_from(&path, uid ^ 1).is_err(),
            "config directory owned by a different uid accepted"
        );

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            load_config_from(&path, uid).is_err(),
            "group-readable config accepted"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let link = dir.join("link.conf");
        symlink(&path, &link).unwrap();
        assert!(
            load_config_from(&link, uid).is_err(),
            "symlink config accepted"
        );

        let linked_dir = dir.with_file_name(format!(
            "{}-parent-link",
            dir.file_name().unwrap().to_string_lossy()
        ));
        symlink(&dir, &linked_dir).unwrap();
        assert!(
            load_config_from(&linked_dir.join("tunnel.conf"), uid).is_err(),
            "symlinked config directory accepted"
        );

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            load_config_from(&path, uid).is_err(),
            "permissive parent accepted"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&linked_dir);
    }

    #[test]
    fn known_hosts_requires_a_private_regular_file_with_at_least_one_key() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = std::env::temp_dir().join(format!(
            "dux-known-hosts-{}-{}",
            std::process::id(),
            crate::util::now_secs()
        ));
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let uid = std::fs::metadata(&dir).unwrap().uid();
        let path = dir.join("known_hosts");
        std::fs::write(&path, "[relay.example.net]:2222 ssh-ed25519 AAAAexample\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(known_hosts_usable(path.to_str().unwrap(), uid).is_ok());

        std::fs::write(&path, "# no trusted keys yet\n\n").unwrap();
        assert!(known_hosts_usable(path.to_str().unwrap(), uid).is_err());
        std::fs::write(&path, "[relay.example.net]:2222 ssh-ed25519 AAAAexample\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(known_hosts_usable(path.to_str().unwrap(), uid).is_err());

        let link = dir.join("known_hosts_link");
        symlink(&path, &link).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(known_hosts_usable(link.to_str().unwrap(), uid).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn ssh_helpers_are_resolved_by_absolute_package_path() {
        assert_eq!(SSH_BIN, "/usr/bin/ssh");
        assert_eq!(SSHD_BIN, "/usr/sbin/sshd");
        assert_eq!(SSH_KEYGEN_BIN, "/usr/bin/ssh-keygen");
        assert!(Path::new(SSH_BIN).is_absolute());
        assert!(Path::new(SSHD_BIN).is_absolute());
        assert!(Path::new(SSH_KEYGEN_BIN).is_absolute());
    }

    #[test]
    fn ssh_args_builds_reverse_forward() {
        let a = ssh_args("203.0.113.9", 2222, 24680, 22000, None, "/etc/dux/ed25519");
        // the reverse forward maps home:24680 -> this host's local sshd (the
        // detected local port, here 22000 — proving it isn't hardcoded to 22)
        let r = a.iter().position(|s| s == "-R").expect("-R present");
        assert_eq!(a[r + 1], "localhost:24680:localhost:22000");
        // connects to the home endpoint's own ssh port (may be non-standard)
        let p = a.iter().position(|s| s == "-p").expect("-p present");
        assert_eq!(a[p + 1], "2222");
        // no dropbear port requested -> only ONE reverse forward
        assert_eq!(a.iter().filter(|s| *s == "-R").count(), 1);
        // key path is passed through -i
        let i = a.iter().position(|s| s == "-i").expect("-i present");
        assert_eq!(a[i + 1], "/etc/dux/ed25519");
        assert!(a
            .windows(2)
            .any(|w| w[0] == "-o" && w[1] == "IdentitiesOnly=yes"));
        assert!(a
            .windows(2)
            .any(|w| w[0] == "-o" && w[1] == "StrictHostKeyChecking=yes"));
        assert!(a
            .windows(2)
            .any(|w| w[0] == "-o" && w[1] == "UpdateHostKeys=no"));
        assert!(a
            .windows(2)
            .any(|w| { w[0] == "-o" && w[1] == format!("UserKnownHostsFile={KNOWN_HOSTS_PATH}") }));
        assert!(a
            .windows(2)
            .any(|w| w[0] == "-o" && w[1] == "GlobalKnownHostsFile=/dev/null"));
        // destination is user@home, last arg
        assert_eq!(a.last().unwrap(), "dux@203.0.113.9");
        // never carries a remote command / pty
        assert!(a.contains(&"-N".to_string()) && a.contains(&"-T".to_string()));
        // unattended: must not prompt
        assert!(a
            .windows(2)
            .any(|w| w[0] == "-o" && w[1] == "BatchMode=yes"));
        assert!(a.windows(2).any(|w| w[0] == "-F" && w[1] == "/dev/null"));
    }

    #[test]
    fn remote_port_is_stable_distinct_and_in_range() {
        // same identity -> same port every time (survives reconnect/rebuild)
        assert_eq!(remote_port("machine-A"), remote_port("machine-A"));
        // different hosts -> different ports (no collision for the same binary)
        assert_ne!(remote_port("machine-A"), remote_port("machine-B"));
        for id in ["a", "b", "some-long-machine-id-0123456789", ""] {
            let p = remote_port(id);
            assert!((20000..60000).contains(&p), "{id} -> {p} out of range");
        }
    }

    #[test]
    fn dropbear_port_is_stable_distinct_from_sshd_port() {
        for id in ["machine-A", "machine-B", "uptimekuma", ""] {
            let sshd = remote_port(id);
            let db = dropbear_remote_port(id);
            assert!((20000..60000).contains(&db), "{id} -> {db} out of range");
            assert_ne!(sshd, db, "sshd and break-glass ports must differ for {id}");
            // stable across calls
            assert_eq!(db, dropbear_remote_port(id));
        }
    }

    #[test]
    fn ssh_args_adds_second_forward_for_dropbear() {
        let a = ssh_args("203.0.113.9", 22, 100, 22, Some(200), "/etc/dux/ed25519");
        // two reverse forwards now: sshd AND break-glass, both live at once
        let fwds: Vec<&String> = a
            .iter()
            .enumerate()
            .filter(|(i, s)| *s == "-R" && *i + 1 < a.len())
            .map(|(i, _)| &a[i + 1])
            .collect();
        assert_eq!(fwds.len(), 2, "expected sshd + dropbear forwards: {a:?}");
        assert!(fwds
            .iter()
            .any(|f| f.as_str() == "localhost:100:localhost:22"));
        assert!(fwds
            .iter()
            .any(|f| f.as_str() == format!("localhost:200:localhost:{DROPBEAR_LOCAL_PORT}")));
        // destination stays last
        assert_eq!(a.last().unwrap(), "dux@203.0.113.9");
    }

    #[test]
    fn authorized_key_only_allows_this_hosts_loopback_listener_ports() {
        let line = authorized_keys_line("ssh-ed25519 AAAATEST dux-tunnel", 24000, Some(24001));
        assert_eq!(
            line,
            "restrict,port-forwarding,permitlisten=\"localhost:24000\",permitlisten=\"localhost:24001\",permitopen=\"dux-tunnel-disabled.invalid:1\",command=\"true\" ssh-ed25519 AAAATEST dux-tunnel"
        );

        let sshd_only = authorized_keys_line("ssh-ed25519 AAAATEST dux-tunnel", 24000, None);
        assert!(sshd_only.contains("permitlisten=\"localhost:24000\""));
        assert!(!sshd_only.contains("24001"));
        assert!(sshd_only.contains("permitopen=\"dux-tunnel-disabled.invalid:1\""));
    }

    #[test]
    fn backoff_grows_then_resets_when_healthy() {
        // an immediate failure doubles the delay, capped
        let b1 = next_backoff(BACKOFF_MIN, Duration::from_secs(1));
        assert_eq!(b1, BACKOFF_MIN * 2);
        let capped = next_backoff(Duration::from_secs(45), Duration::from_secs(1));
        assert_eq!(capped, BACKOFF_MAX);
        // a connection that stayed up long enough resets to the floor
        let reset = next_backoff(BACKOFF_MAX, HEALTHY_AFTER);
        assert_eq!(reset, BACKOFF_MIN);
    }

    #[test]
    fn key_usable_rejects_loose_perms() {
        let dir = std::env::temp_dir().join(format!("dux-tun-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let key = dir.join("ed25519");
        std::fs::write(&key, b"x").unwrap();

        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
        let meta = std::fs::symlink_metadata(&key).unwrap();
        assert!(
            validate_protected_file_metadata(key.to_str().unwrap(), &meta, meta.uid()).is_err(),
            "0644 must be rejected"
        );

        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        let meta = std::fs::symlink_metadata(&key).unwrap();
        assert!(validate_protected_file_metadata(key.to_str().unwrap(), &meta, meta.uid()).is_ok());
        assert!(
            validate_protected_file_metadata(key.to_str().unwrap(), &meta, meta.uid() + 1).is_err()
        );

        let symlink = dir.join("symlink-key");
        std::os::unix::fs::symlink(&key, &symlink).unwrap();
        let meta = std::fs::symlink_metadata(&symlink).unwrap();
        assert!(
            validate_protected_file_metadata(symlink.to_str().unwrap(), &meta, meta.uid()).is_err()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

use crate::store::Store;
use crate::util::now_secs;
use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension};
use std::collections::{HashMap, HashSet};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Set by the SIGTERM/SIGINT handler so the daemon loop can flush pending events
/// and exit cleanly (systemd sends SIGTERM on stop). Async-signal-safe: the
/// handler only stores to this flag.
static SHUTDOWN: AtomicBool = AtomicBool::new(false);
/// Rate-limit slow-event diagnostics so a pathological notification stream
/// cannot turn the diagnostic itself into an I/O storm.
static LAST_SLOW_EVENT_LOG_AT: AtomicU64 = AtomicU64::new(0);
static LAST_UNRESOLVED_EVENT_LOG_AT: AtomicU64 = AtomicU64::new(0);
static UNRESOLVED_EVENTS_SUPPRESSED: AtomicU64 = AtomicU64::new(0);
const UNRESOLVED_EVENT_LOG_INTERVAL_SECS: u64 = 60;
static LAST_SLOW_FLUSH_LOG_AT: AtomicU64 = AtomicU64::new(0);
static LAST_WAL_WARNING_AT: AtomicU64 = AtomicU64::new(0);
static LAST_DIRTY_MARKER_FAILURE_LOG_AT: AtomicU64 = AtomicU64::new(0);

struct RawFdGuard(RawFd);

impl Drop for RawFdGuard {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

/// Fanotify's kernel queue is drained by a dedicated thread in scan-first
/// mode. Event mode forwards bounded chunks; scan-first mode only counts valid
/// metadata and discards chunks because the fixed-cadence full walk is the
/// source of truth. The latter avoids a second lossy queue and handle resolver.
struct FanotifyHintReader {
    rx: Receiver<Vec<u8>>,
    notify: RawFdGuard,
    kernel_empty: Arc<AtomicBool>,
    queued_chunks: Arc<std::sync::atomic::AtomicUsize>,
    dropped: Arc<AtomicBool>,
    observed_events: Arc<AtomicU64>,
    kernel_overflows: Arc<AtomicU64>,
    discard_chunks: bool,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

struct FanotifyReaderState {
    kernel_empty: Arc<AtomicBool>,
    queued_chunks: Arc<std::sync::atomic::AtomicUsize>,
    dropped: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    observed_events: Arc<AtomicU64>,
    kernel_overflows: Arc<AtomicU64>,
}

const HINT_READER_CHANNEL_CAPACITY: usize = 128;
const HINT_RESOLVER_CHANNEL_CAPACITY: usize = 128;
const HINT_READER_POLL_MS: libc::c_int = 100;
const HINT_READER_MAX_READS_PER_TICK: usize = 48;
const HINT_READER_DRAIN_SLICE: Duration = Duration::from_millis(100);
// A 32 KiB reader chunk can contain fewer than 1,400 minimum-size metadata
// records. Keep one parse result per reader chunk so its outstanding-work
// counter stays exact while the resolver runs asynchronously.
const MAX_HINT_EVENTS_PER_CHUNK: usize = 2048;

#[derive(Debug)]
struct HintResolutionJob {
    handles: Vec<(Vec<u8>, u64)>,
}

struct HintResolutionResult {
    seen: u64,
    lost: bool,
    priority_paths: Vec<PathBuf>,
}

fn queue_hint_resolution_result(
    tx: &SyncSender<HintResolutionResult>,
    result: HintResolutionResult,
    queued_chunks: &std::sync::atomic::AtomicUsize,
    dropped: &AtomicBool,
    notify: RawFd,
) {
    if tx.try_send(result).is_err() {
        queued_chunks.fetch_sub(1, Ordering::AcqRel);
        dropped.store(true, Ordering::Release);
    }
    notify_hint_reader(notify);
}

/// Resolve coalesced parent handles away from the coordinator thread. Both
/// directions are bounded and nonblocking; saturation is converted to an
/// explicit full-observation request.
struct FanotifyHintResolver {
    tx: SyncSender<HintResolutionJob>,
    rx: Receiver<HintResolutionResult>,
    dropped: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl FanotifyHintResolver {
    fn start(
        fsfds: HashMap<(i32, i32), RawFd>,
        mut handle_capable_fsids: HashSet<(i32, i32)>,
        ambiguous_fsids: HashSet<(i32, i32)>,
        root: PathBuf,
        notify: RawFd,
        queued_chunks: Arc<std::sync::atomic::AtomicUsize>,
    ) -> Result<Self> {
        let (tx, rx_jobs): (SyncSender<HintResolutionJob>, Receiver<HintResolutionJob>) =
            mpsc::sync_channel(HINT_RESOLVER_CHANNEL_CAPACITY);
        let (tx_results, rx): (
            SyncSender<HintResolutionResult>,
            Receiver<HintResolutionResult>,
        ) = mpsc::sync_channel(HINT_RESOLVER_CHANNEL_CAPACITY);
        let dropped = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_dropped = Arc::clone(&dropped);
        let thread_stop = Arc::clone(&stop);
        let join = std::thread::Builder::new()
            .name("dux-hint-resolver".into())
            .spawn(move || {
                while !thread_stop.load(Ordering::Acquire) {
                    let job = match rx_jobs.recv_timeout(Duration::from_millis(100)) {
                        Ok(job) => job,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    let (seen, scope_lost, priority_paths) = resolve_fanotify_hint_scope(
                        job.handles,
                        &fsfds,
                        &mut handle_capable_fsids,
                        &ambiguous_fsids,
                        &root,
                    );
                    let result = HintResolutionResult {
                        seen,
                        lost: scope_lost,
                        priority_paths,
                    };
                    queue_hint_resolution_result(
                        &tx_results,
                        result,
                        &queued_chunks,
                        &thread_dropped,
                        notify,
                    );
                }
            })
            .context("starting fanotify hint resolver")?;
        Ok(Self {
            tx,
            rx,
            dropped,
            stop,
            join: Some(join),
        })
    }

    fn stop_and_join(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for FanotifyHintResolver {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

impl FanotifyHintReader {
    fn start(fan: RawFd, discard_chunks: bool) -> Result<Self> {
        let reader_fan = unsafe { libc::fcntl(fan, libc::F_DUPFD_CLOEXEC, 0) };
        if reader_fan < 0 {
            return Err(std::io::Error::last_os_error()).context("duplicating fanotify fd");
        }
        let notify_fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if notify_fd < 0 {
            unsafe { libc::close(reader_fan) };
            return Err(std::io::Error::last_os_error()).context("creating hint-reader eventfd");
        }
        let notify_guard = RawFdGuard(notify_fd);
        let (tx, rx) = mpsc::sync_channel(HINT_READER_CHANNEL_CAPACITY);
        let kernel_empty = Arc::new(AtomicBool::new(true));
        let queued_chunks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let dropped = Arc::new(AtomicBool::new(false));
        let observed_events = Arc::new(AtomicU64::new(0));
        let kernel_overflows = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let reader_fan_guard = RawFdGuard(reader_fan);
        let thread_empty = Arc::clone(&kernel_empty);
        let thread_queued = Arc::clone(&queued_chunks);
        let thread_dropped = Arc::clone(&dropped);
        let thread_observed_events = Arc::clone(&observed_events);
        let thread_kernel_overflows = Arc::clone(&kernel_overflows);
        let thread_stop = Arc::clone(&stop);
        let thread_discard_chunks = discard_chunks;
        let join = std::thread::Builder::new()
            .name("dux-fanotify-reader".into())
            .spawn(move || {
                let reader_fd = reader_fan_guard.0;
                fanotify_reader_loop(
                    reader_fd,
                    notify_fd,
                    tx,
                    FanotifyReaderState {
                        kernel_empty: thread_empty,
                        queued_chunks: thread_queued,
                        dropped: thread_dropped,
                        stop: thread_stop,
                        observed_events: thread_observed_events,
                        kernel_overflows: thread_kernel_overflows,
                    },
                    thread_discard_chunks,
                );
                drop(reader_fan_guard);
            })
            .context("starting fanotify hint reader")?;
        Ok(Self {
            rx,
            notify: notify_guard,
            kernel_empty,
            queued_chunks,
            dropped,
            observed_events,
            kernel_overflows,
            discard_chunks,
            stop,
            join: Some(join),
        })
    }

    fn acknowledge(&self) {
        let mut count = 0u64;
        unsafe {
            libc::read(
                self.notify.0,
                (&mut count as *mut u64).cast::<libc::c_void>(),
                std::mem::size_of::<u64>(),
            );
        }
    }

    fn stop_and_join(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for FanotifyHintReader {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

fn notify_hint_reader(fd: RawFd) {
    let count = 1u64;
    unsafe {
        libc::write(
            fd,
            (&count as *const u64).cast::<libc::c_void>(),
            std::mem::size_of::<u64>(),
        );
    }
}

fn queue_hint_chunk(
    tx: &SyncSender<Vec<u8>>,
    bytes: Vec<u8>,
    queued_chunks: &std::sync::atomic::AtomicUsize,
    dropped: &AtomicBool,
) {
    queued_chunks.fetch_add(1, Ordering::AcqRel);
    if tx.try_send(bytes).is_err() {
        queued_chunks.fetch_sub(1, Ordering::AcqRel);
        dropped.store(true, Ordering::Release);
    }
}

fn note_hint_pipeline_drop(
    reader: Option<&FanotifyHintReader>,
    resolver: Option<&FanotifyHintResolver>,
) {
    if let Some(reader) = reader {
        reader.queued_chunks.fetch_sub(1, Ordering::AcqRel);
        if let Some(resolver) = resolver {
            resolver.dropped.store(true, Ordering::Release);
        } else {
            reader.dropped.store(true, Ordering::Release);
        }
        notify_hint_reader(reader.notify.0);
    }
}

fn fanotify_reader_loop(
    fan: RawFd,
    notify: RawFd,
    tx: SyncSender<Vec<u8>>,
    state: FanotifyReaderState,
    discard_chunks: bool,
) {
    let mut buf = [0u8; 1 << 15];
    let mut partial_record = Vec::with_capacity(FAN_EVENT_METADATA_LEN);
    let mut pfd = libc::pollfd {
        fd: fan,
        events: libc::POLLIN,
        revents: 0,
    };
    while !state.stop.load(Ordering::Acquire) {
        pfd.revents = 0;
        let polled = unsafe { libc::poll(&mut pfd, 1, HINT_READER_POLL_MS) };
        if polled == 0 {
            // A previous bounded drain may have stopped at its time/read cap
            // with `kernel_empty=false`. Once poll observes the fd idle for a
            // full interval, publish that the kernel queue is now drained;
            // otherwise reconciliation and accounting checks can remain
            // blocked forever after a burst that ended between reader ticks.
            if !state.kernel_empty.swap(true, Ordering::AcqRel) {
                notify_hint_reader(notify);
            }
            continue;
        }
        if polled < 0 {
            let error = std::io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EINTR) | Some(libc::EAGAIN) => continue,
                _ => {
                    tracing::error!(%error, "fanotify hint reader poll failed");
                    state.dropped.store(true, Ordering::Release);
                    notify_hint_reader(notify);
                    break;
                }
            }
        }
        let drain_deadline = Instant::now() + HINT_READER_DRAIN_SLICE;
        let mut reads = 0usize;
        loop {
            if state.stop.load(Ordering::Acquire)
                || reads >= HINT_READER_MAX_READS_PER_TICK
                || Instant::now() >= drain_deadline
            {
                state.kernel_empty.store(false, Ordering::Release);
                notify_hint_reader(notify);
                break;
            }
            let n = unsafe { libc::read(fan, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len()) };
            if n > 0 {
                state.kernel_empty.store(false, Ordering::Release);
                reads += 1;
                if discard_chunks {
                    partial_record.extend_from_slice(&buf[..n as usize]);
                    let (seen, overflows, consumed, malformed) =
                        count_fanotify_records_available(&partial_record);
                    state.observed_events.fetch_add(seen, Ordering::Relaxed);
                    state
                        .kernel_overflows
                        .fetch_add(overflows, Ordering::Relaxed);
                    if consumed > 0 {
                        partial_record.drain(..consumed);
                    }
                    if malformed {
                        // Hints are advisory in scan-first mode. A malformed
                        // record is counted as a reader fault for diagnostics;
                        // the fixed-cadence full walk remains the recovery path.
                        state.dropped.store(true, Ordering::Release);
                        partial_record.clear();
                    }
                    if partial_record.len() > 4096 {
                        state.dropped.store(true, Ordering::Release);
                        partial_record.clear();
                    }
                    continue;
                }
                // Dux can generate a large share of filesystem-wide events
                // itself (SQLite/WAL and reconciliation metadata). Drop valid
                // self-originated records before the bounded userspace ring;
                // filtering later in the coordinator is too late to prevent
                // ring saturation. Preserve malformed input and overflow
                // records so the canonical parser still fails closed.
                let bytes = match filter_self_fanotify_records(&buf[..n as usize]) {
                    Some(bytes) => bytes,
                    None => buf[..n as usize].to_vec(),
                };
                // Full and disconnected are both unrecoverable hint loss;
                // keep draining the kernel and force a full observation.
                if !bytes.is_empty() {
                    queue_hint_chunk(&tx, bytes, &state.queued_chunks, &state.dropped);
                }
                if !discard_chunks {
                    notify_hint_reader(notify);
                }
                continue;
            }
            if n == 0 {
                state.kernel_empty.store(true, Ordering::Release);
                notify_hint_reader(notify);
                break;
            }
            match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::EINTR) => continue,
                Some(libc::EAGAIN) => {
                    state.kernel_empty.store(true, Ordering::Release);
                    notify_hint_reader(notify);
                    break;
                }
                _ => {
                    let error = std::io::Error::last_os_error();
                    tracing::error!(%error, "fanotify hint reader read failed");
                    state.dropped.store(true, Ordering::Release);
                    state.kernel_empty.store(false, Ordering::Release);
                    notify_hint_reader(notify);
                    return;
                }
            }
        }
    }
}

fn wal_warning_allowed() -> bool {
    let now = now_secs().max(0) as u64;
    let previous = LAST_WAL_WARNING_AT.load(Ordering::Relaxed);
    now.saturating_sub(previous) >= 60
        && LAST_WAL_WARNING_AT
            .compare_exchange(previous, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
}

fn dirty_marker_failure_log_allowed() -> bool {
    let now = now_secs().max(0) as u64;
    let previous = LAST_DIRTY_MARKER_FAILURE_LOG_AT.load(Ordering::Relaxed);
    now.saturating_sub(previous) >= 60
        && LAST_DIRTY_MARKER_FAILURE_LOG_AT
            .compare_exchange(previous, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
}

fn wal_checkpoint_progress(conn: &rusqlite::Connection) -> rusqlite::Result<(i64, i64, i64)> {
    conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })
}

extern "C" fn on_term(_sig: libc::c_int) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

/// Set by the SIGHUP handler: a `dux scan` against a live daemon asks the daemon
/// to rebuild its own index in place (atomic, single-writer, no downtime) rather
/// than making the user stop and restart the service by hand.
static RESCAN: AtomicBool = AtomicBool::new(false);
/// Distinguishes an operator-requested `dux scan` from background recovery so
/// the accounting-failure circuit breaker can stop automatic full-root loops
/// while preserving an explicit recovery path.
static MANUAL_RESCAN: AtomicBool = AtomicBool::new(false);

extern "C" fn on_hup(_sig: libc::c_int) {
    MANUAL_RESCAN.store(true, Ordering::SeqCst);
    RESCAN.store(true, Ordering::SeqCst);
}

/// Pause index writes when the index's filesystem drops below this much free
/// space. This reduces further index writes after the threshold is observed;
/// already-running transactions and other writers can still exhaust free space.
const MIN_FREE_BYTES: i64 = 256 * 1024 * 1024;

/// Hard cap on un-flushed pending events. If SQLite is wedged or an event storm
/// outruns flushing, the map would otherwise grow without bound (logical OOM).
/// Past this we drop the backlog and mark the index dirty (reconcile via rescan)
/// — bounded memory beats an OOM kill that loses everything anyway.
const MAX_PENDING: usize = 500_000;

/// Do not let a persistent event stream starve loss recovery forever. Once a
/// requested reconciliation has been unable to reach a drained queue for this
/// long, take a fresh atomic filesystem baseline and replay the retained events
/// afterward. Dirty state remains set until replay and accounting verification
/// complete.
const RECONCILE_BACKLOG_GRACE: Duration = Duration::from_secs(30);

/// Bound full-root retries when another overflow occurs during reconciliation.
const RECONCILE_RETRY_BACKOFF: Duration = Duration::from_secs(60);
const SCAN_FIRST_INTERVAL: Duration = Duration::from_secs(15 * 60);
const RECONCILIATION_RETRY_AFTER_META: &str = "reconciliation_retry_after_unix";
/// Open the automatic-reconciliation circuit after one retry fails. Automatic
/// requests pass the open circuit after its bounded attempt-count cooldown.
pub(crate) const MAX_AUTOMATIC_RECONCILIATION_RETRIES: u32 = 1;

fn reconciliation_retry_delay(failed_attempts: u32) -> Duration {
    let exponent = failed_attempts.saturating_sub(1).min(9);
    Duration::from_secs(
        RECONCILE_RETRY_BACKOFF
            .as_secs()
            .saturating_mul(1u64 << exponent),
    )
}

fn bounded_retry_remaining_seconds(
    retry_after: Option<&str>,
    now: i64,
    maximum: Duration,
) -> (u64, bool) {
    let maximum = maximum.as_secs();
    match retry_after {
        // Older releases persisted an attempt count but no deadline. Give that
        // legacy state the base cooldown; applying the exponential maximum on
        // upgrade could leave an already-stale index waiting for hours.
        None => (RECONCILE_RETRY_BACKOFF.as_secs().min(maximum), false),
        Some(value) => match value.parse::<i64>() {
            Ok(deadline) => {
                let remaining = deadline.saturating_sub(now).max(0) as u64;
                (remaining.min(maximum), false)
            }
            Err(_) => (maximum, true),
        },
    }
}

fn persist_reconciliation_retry_deadline(store: &Store, failed_attempts: u32) -> Result<Instant> {
    let delay = reconciliation_retry_delay(failed_attempts);
    let delay_seconds = i64::try_from(delay.as_secs()).unwrap_or(i64::MAX);
    let deadline = crate::util::now_secs().saturating_add(delay_seconds);
    store.set_meta(RECONCILIATION_RETRY_AFTER_META, &deadline.to_string())?;
    Ok(Instant::now() + delay)
}

fn restore_reconciliation_retry_deadline(
    store: &Store,
    failed_attempts: u32,
    persist: bool,
) -> Result<Instant> {
    let now = crate::util::now_secs();
    let maximum = reconciliation_retry_delay(failed_attempts);
    let stored = store.get_meta(RECONCILIATION_RETRY_AFTER_META)?;
    let (remaining, corrupt) = bounded_retry_remaining_seconds(stored.as_deref(), now, maximum);
    let remaining = Duration::from_secs(remaining);
    let remaining_seconds = i64::try_from(remaining.as_secs()).unwrap_or(i64::MAX);
    let deadline = now.saturating_add(remaining_seconds);
    // Normalize missing, malformed, or implausibly distant deadlines. This
    // preserves the remaining cooldown across restarts while keeping corrupt
    // metadata from creating an unbounded recovery delay.
    if persist {
        store.set_meta(RECONCILIATION_RETRY_AFTER_META, &deadline.to_string())?;
    }
    if corrupt {
        tracing::error!(
            "persisted reconciliation retry deadline is corrupt; applying maximum bounded backoff"
        );
    }
    Ok(Instant::now() + remaining)
}

fn automatic_reconciliation_circuit_open(failed_attempts: u32) -> bool {
    failed_attempts > MAX_AUTOMATIC_RECONCILIATION_RETRIES
}

/// Reconciliation-first mode must keep retrying observations after bounded
/// backoff: scans are its source of truth, so a permanent retry circuit would
/// disable the only repair mechanism. Keep the legacy event-apply circuit until
/// that mode is retired.
fn reconciliation_retry_should_pause(reconcile_first: bool, failed_attempts: u32) -> bool {
    !reconcile_first && automatic_reconciliation_circuit_open(failed_attempts)
}

fn parse_reconciliation_attempts(value: Option<&str>) -> (u32, bool) {
    match value {
        None => (0, false),
        Some(value) => match value.parse::<u32>() {
            Ok(attempts) => (attempts, false),
            // A corrupt retry counter must not silently reset the circuit and
            // admit another immediate full-root scan. Keep it on maximum
            // backoff while still allowing eventual automatic recovery.
            Err(_) => (u32::MAX, true),
        },
    }
}

/// Expose the persisted automatic-reconciliation circuit state to status
/// callers. Missing metadata means this index predates the circuit; malformed
/// metadata reports the circuit open while automatic recovery uses max backoff.
pub(crate) fn reconciliation_retry_status(value: Option<&str>) -> (Option<u32>, Option<bool>) {
    match value {
        None => (None, None),
        Some(value) => {
            let (attempts, corrupt) = parse_reconciliation_attempts(Some(value));
            if corrupt {
                (None, Some(true))
            } else {
                (
                    Some(attempts),
                    Some(automatic_reconciliation_circuit_open(attempts)),
                )
            }
        }
    }
}

pub(crate) fn reconciliation_retry_backoff_seconds(value: Option<&str>) -> Option<u64> {
    value.map(|value| {
        let (attempts, _) = parse_reconciliation_attempts(Some(value));
        reconciliation_retry_delay(attempts).as_secs()
    })
}

pub(crate) fn reconciliation_retry_remaining_seconds(
    retry_after: Option<&str>,
    now: i64,
) -> Option<u64> {
    retry_after.and_then(|value| {
        value
            .parse::<i64>()
            .ok()
            .map(|deadline| deadline.saturating_sub(now).max(0) as u64)
    })
}

fn backlog_exceeded(pending: usize, deferred: usize, target_refreshes: usize) -> bool {
    pending
        .saturating_add(deferred)
        .saturating_add(target_refreshes)
        > MAX_PENDING
}

fn shutdown_drain_incomplete(queue_drained: bool, buffered_bytes: usize) -> bool {
    !queue_drained || buffered_bytes > 0
}

fn shutdown_event_flush_allowed(
    baseline_ready: bool,
    scan_active: bool,
    wal_guarded: bool,
    work_pending: bool,
) -> bool {
    baseline_ready && !scan_active && !wal_guarded && work_pending
}

fn reconciliation_scan_ready(
    requested: bool,
    queues_drained: bool,
    behind: Duration,
    retry_backoff_elapsed: bool,
) -> bool {
    requested && retry_backoff_elapsed && (queues_drained || behind >= RECONCILE_BACKLOG_GRACE)
}

fn reconciliation_retry_gate_elapsed(
    reconcile_first: bool,
    periodic_observation_due: bool,
    backoff_elapsed: bool,
) -> bool {
    backoff_elapsed || (reconcile_first && periodic_observation_due)
}

fn scan_first_observation_due(last_started: Instant, now: Instant) -> bool {
    now.saturating_duration_since(last_started) >= SCAN_FIRST_INTERVAL
}

/// Scan-first mode treats fanotify loss as freshness information only. A full
/// observation is already scheduled on the fixed cadence, so a lossy hint
/// queue must not pull that scan forward or create repeated whole-root work.
fn hint_loss_requests_immediate_scan(reconcile_first: bool) -> bool {
    !reconcile_first
}

fn dirty_retry_due_for_event_mode(
    reconcile_first: bool,
    dirty: bool,
    last_started: Instant,
    now: Instant,
) -> bool {
    !reconcile_first
        && dirty
        && now.saturating_duration_since(last_started) >= Duration::from_secs(300)
}

fn scan_first_can_use_in_place(
    reconcile_first: bool,
    baseline_ready: bool,
    one_file_system: bool,
) -> bool {
    reconcile_first && baseline_ready && !one_file_system
}

fn scan_first_event_race_only(
    reconcile_first: bool,
    lost_events: bool,
    forced_reconciliation: bool,
    coverage_incomplete: bool,
    resolve_warned: bool,
    scan_errors: u64,
) -> bool {
    reconcile_first
        && (lost_events || forced_reconciliation)
        && !coverage_incomplete
        && !resolve_warned
        && scan_errors == 0
}

fn scan_first_preserve_manual_followup(event_race_only: bool, manual_rescan_pending: bool) -> bool {
    event_race_only && manual_rescan_pending
}

fn scan_first_hint_invalidates_observation(
    scan_active: bool,
    in_scope: bool,
    scope_unresolved: bool,
) -> bool {
    in_scope || (scan_active && scope_unresolved)
}

fn automatic_reconciliation_request_allowed(
    retry_paused: bool,
    scan_active: bool,
    accounting_check_active: bool,
    request_pending: bool,
) -> bool {
    request_pending && !retry_paused && !scan_active && !accounting_check_active
}

fn reconciliation_retry_circuit_blocks_request(paused: bool, cooldown_elapsed: bool) -> bool {
    paused && !cooldown_elapsed
}

fn watcher_poll_timeout(
    active_flush_interval: Duration,
    elapsed_since_flush: Duration,
    scan_active: bool,
) -> i32 {
    let until_flush = active_flush_interval.saturating_sub(elapsed_since_flush);
    let wait_ms = until_flush.as_millis().min(i32::MAX as u128);
    if scan_active {
        wait_ms.min(100) as i32
    } else {
        wait_ms as i32
    }
}

/// Max fanotify read()s drained per loop tick before the CPU governor gets to run.
/// Bounds the size of a single CPU burst under a storm so throttling stays smooth
/// (the rest of the backlog waits in the kernel's fanotify queue until next tick).
const MAX_READS_PER_TICK: u32 = 48;
/// Bound synchronous work per event-parser invocation. The fanotify read size
/// and outer drain deadline alone do not bound one call to `parse_events`.
const MAX_EVENTS_PER_PARSE: usize = 128;
/// Keep indexed-link expansion out of the parser and bound the work admitted to
/// any one apply cycle.
const MAX_TARGET_REFRESHES_PER_CYCLE: usize = 128;
const EVENT_PARSE_SLICE: Duration = Duration::from_millis(10);
/// Keep reconciliation parallel enough to overlap metadata I/O under the
/// packaged 100% CPUQuota (one CPU total for the service). A 1m ext4 profile
/// was 35% faster with two walker jobs than the quota-aware auto value of one,
/// with lower measured peak RSS. Scan workers share the process-wide ceiling.
const BACKGROUND_RECONCILIATION_JOBS: usize = 2;
/// Stop committing event batches when a pinned reader lets the WAL exceed this
/// size. SQLite cannot enforce journal_size_limit while readers pin frames.
const MAX_WAL_BYTES: u64 = 256 * 1024 * 1024;

fn wal_checkpoint_complete(busy: i64, log_frames: i64, checkpointed_frames: i64) -> bool {
    busy == 0 && checkpointed_frames >= log_frames
}

fn wal_write_guard_transition(
    guarded: bool,
    wal_bytes: Option<u64>,
    checkpoint: Option<(i64, i64, i64)>,
) -> bool {
    if wal_bytes == Some(0) {
        return false;
    }
    let oversized = wal_bytes.is_some_and(|bytes| bytes > MAX_WAL_BYTES);
    match checkpoint {
        Some((busy, log, checkpointed)) if wal_checkpoint_complete(busy, log, checkpointed) => {
            false
        }
        Some(_) => guarded || oversized,
        None => guarded || oversized,
    }
}

fn background_reconciliation_scan_options(
    one_file_system: bool,
    cancel: Arc<AtomicBool>,
) -> crate::scan::ScanOptions {
    crate::scan::ScanOptions {
        one_file_system,
        progress: false,
        low_priority: true,
        jobs: Some(BACKGROUND_RECONCILIATION_JOBS),
        cancel: Some(cancel),
        ..Default::default()
    }
}
// Reuse resolved directory paths across target-only updates until a flush changes
// the committed graph. Bound the cache during storms over very wide trees.
const MAX_INDEXED_PARENT_PATH_CACHE: usize = 16_384;

#[derive(Default)]
struct IndexedParentPathCache {
    paths: HashMap<(i64, i64), Option<PathBuf>>,
    #[cfg(test)]
    lookups: usize,
}

impl IndexedParentPathCache {
    fn resolve(
        &mut self,
        conn: &rusqlite::Connection,
        dev: i64,
        ino: i64,
    ) -> Result<Option<PathBuf>> {
        let key = (dev, ino);
        if let Some(path) = self.paths.get(&key) {
            return Ok(path.clone());
        }
        let path = crate::store::indexed_inode_path(conn, dev, ino)?;
        #[cfg(test)]
        {
            self.lookups += 1;
        }
        if self.paths.len() < MAX_INDEXED_PARENT_PATH_CACHE {
            self.paths.insert(key, path.clone());
        }
        Ok(path)
    }

    fn clear(&mut self) {
        self.paths.clear();
    }
}

/// If the daemon hasn't emptied the kernel fanotify queue in this long, it's
/// persistently behind (the CPU/IO governor is holding it back under sustained
/// load) — surface a "throttled, data is stale" state to status/TUI.
const STALE_BEHIND_SECS: i64 = 15;

/// Max `pending` entries indexed in one flush. Bounds the per-flush CPU burst so
/// the governor can hold a low `--max-cpu` target under a create storm (a giant
/// single flush would be a multi-second spike it can't offset). The remainder
/// drains over the next, throttled cycles.
const FLUSH_BATCH: usize = 128;

/// Last committed pipeline counters to republish while a SQLite flush is doing
/// expensive work. A status record must stay fresh during a long transaction,
/// but it must conservatively keep the in-flight batch pending until commit.
#[derive(Clone, Copy)]
struct FlushProgress {
    events_seen: u64,
    events_resolved: u64,
    updates_committed: u64,
    last_commit_at: i64,
    updates_dropped: u64,
    kernel_overflow_notifications: u64,
    behind_ms: u64,
    max_pending: usize,
    uncertain: bool,
}

/// Give cross-flush rename pairs five flush intervals to arrive. A longer grace
/// needlessly leaves moved-out files in the live index and delays exactness.
const MOVE_PAIR_GRACE: Duration = Duration::from_millis(500);

/// Max a single throttle can sleep. Must be large enough that even a big work
/// burst (e.g. one flush indexing thousands of new files) can be paid down to a
/// low `--max-cpu` target in one go — otherwise the achieved duty cycle floors at
/// burst/(burst+cap) and the cap is silently exceeded. Bounded so a pathological
/// burst can't stall forever; the heartbeat is refreshed mid-sleep so a long
/// throttle never makes the daemon look dead.
const MAX_THROTTLE_SLEEP_SECS: f64 = 30.0;
/// Use FIONREAD as a queue-pressure signal so CPU duty-cycle repayment cannot
/// starve fanotify draining. Linux kernels may report metadata bytes only while
/// queued records can carry additional variable-size information; treat this
/// as an approximate watermark, never as an exact queue-depth metric.
const DEFAULT_FANOTIFY_MAX_QUEUED_EVENTS: u64 = 16_384;
const FANOTIFY_QUEUE_HIGH_WATERMARK_PERCENT: u64 = 75;
const FANOTIFY_QUEUE_LOW_WATERMARK_PERCENT: u64 = 50;
const FANOTIFY_FIONREAD: libc::Ioctl = 0x541B;

/// Request lower scheduling priority: IDLE I/O class and a low CPU nice. These
/// are best-effort hints; they reduce priority but do not eliminate contention.
/// Lowering one's own priority never requires privilege.
fn set_background_priority() {
    unsafe {
        // nice +10 — production threads always win the CPU.
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
    #[cfg(target_os = "linux")]
    unsafe {
        // ioprio_set(IOPRIO_WHO_PROCESS, 0, IOPRIO_PRIO_VALUE(IDLE, 0)).
        const IOPRIO_WHO_PROCESS: libc::c_int = 1;
        const IOPRIO_CLASS_IDLE: libc::c_int = 3;
        let ioprio = IOPRIO_CLASS_IDLE << 13; // class in the high bits, level 0
        libc::syscall(libc::SYS_ioprio_set, IOPRIO_WHO_PROCESS, 0, ioprio);
    }
}

/// CPU time of the event thread. Charging a concurrent scanner here would put
/// ingestion to sleep precisely when it must drain scan-time filesystem events.
/// Scans retain low priority and the service cgroup CPU/memory backstops.
fn thread_cpu_secs() -> f64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
}

fn pipeline_is_caught_up(
    scan_active: bool,
    kernel_empty: bool,
    pending_empty: bool,
    deferred_empty: bool,
    target_refreshes_empty: bool,
    index_uncertain: bool,
    lost_events: bool,
) -> bool {
    !scan_active
        && kernel_empty
        && pending_empty
        && deferred_empty
        && target_refreshes_empty
        && !index_uncertain
        && !lost_events
}

fn persist_pause_state(store: &Store, reason: Option<&str>) -> Result<()> {
    let tx = store.conn.unchecked_transaction()?;
    if let Some(reason) = reason {
        let now = now_secs().to_string();
        tx.execute(
            "INSERT INTO meta(key,value) VALUES('paused_since',?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [&now],
        )?;
        tx.execute(
            "INSERT INTO meta(key,value) VALUES('pause_reason',?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [reason],
        )?;
    } else {
        tx.execute(
            "DELETE FROM meta WHERE key IN ('paused_since','pause_reason')",
            [],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn reconciliation_deferral_transition(
    already_deferred: bool,
    pressure: crate::guard::Pressure,
    health: &crate::guard::Health,
    disk_floor: i64,
) -> (bool, bool) {
    let deferred =
        !crate::guard::may_start_reconciliation_with_health(pressure, health, disk_floor);
    (deferred, deferred && !already_deferred)
}

#[cfg(test)]
fn reconciliation_is_uncertain(
    coverage_incomplete: bool,
    resolve_warned: bool,
    lost_events: bool,
    scan_errors: u64,
    accounting_consistent: bool,
) -> bool {
    coverage_incomplete
        || resolve_warned
        || lost_events
        || scan_errors > 0
        || !accounting_consistent
}

fn accounting_check_should_cancel(
    pressure: crate::guard::Pressure,
    kernel_empty: bool,
    pending: usize,
    deferred: usize,
    reconcile_first: bool,
) -> bool {
    pressure == crate::guard::Pressure::Critical
        || (!reconcile_first && !kernel_empty)
        || pending > 0
        || deferred > 0
}

/// Periodic whole-index accounting checks must not monopolize the watcher
/// thread. They read every inode and directory entry and can take long enough
/// on million-row indexes to let fanotify overflow. Run the check on its own
/// read-only connection, with a small thread-local CPU budget.
struct AccountingCheck {
    join: std::thread::JoinHandle<Result<Option<bool>>>,
    cancel: Arc<AtomicBool>,
    started: Instant,
    kind: AccountingCheckKind,
}

#[derive(Clone, Copy)]
enum AccountingCheckKind {
    Periodic,
    Reconciliation { safe_to_clear: bool },
}

const RECONCILIATION_ACCOUNTING_PENDING: &str = "reconciliation accounting verification pending";

fn start_accounting_check(
    db: &Path,
    kind: AccountingCheckKind,
) -> std::io::Result<AccountingCheck> {
    let db = db.to_path_buf();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let callback_cancel = Arc::clone(&worker_cancel);
    let started = Instant::now();
    let join = std::thread::Builder::new()
        .name("dux-accounting-check".into())
        .spawn(move || {
            let mut store = Store::open_ro(&db).context("opening read-only accounting snapshot")?;
            let mut cpu_ref = thread_cpu_secs();
            let mut wall_ref = Instant::now();
            store.conn.progress_handler(
                10_000,
                Some(move || {
                    if callback_cancel.load(Ordering::Relaxed) {
                        return true;
                    }
                    // Keep this maintenance worker to 10% of one CPU. The
                    // daemon's event-thread governor has its own target; the
                    // scan and accounting workers share the unit's process-wide
                    // 100% one-CPU hard ceiling.
                    let cpu_now = thread_cpu_secs();
                    let cpu_delta = (cpu_now - cpu_ref).max(0.0);
                    let wall_delta = wall_ref.elapsed().as_secs_f64();
                    let mut remaining = throttle_sleep_secs(cpu_delta, wall_delta, 0.10);
                    while remaining > 0.0 && !callback_cancel.load(Ordering::Relaxed) {
                        let chunk = remaining.min(0.05);
                        std::thread::sleep(Duration::from_secs_f64(chunk));
                        remaining -= chunk;
                    }
                    cpu_ref = thread_cpu_secs();
                    wall_ref = Instant::now();
                    false
                }),
            )?;
            let result = store.totals_consistent();
            if worker_cancel.load(Ordering::Relaxed) {
                Ok(None)
            } else {
                result.map(Some)
            }
        })?;
    Ok(AccountingCheck {
        join,
        cancel,
        started,
        kind,
    })
}

/// CPU governor: targets an average duty cycle of `target` (a fraction of ONE
/// core) for the measured event-loop thread while the fanotify queue is below its
/// high watermark. `throttle()` yields only while queue pressure is low; when the
/// queue is high it bypasses the sleep until backlog falls below a hysteretic low
/// watermark. This can exceed the event-thread target during backlog recovery,
/// but the service's process-wide CPUQuota remains the hard ceiling. The queue is
/// still finite and may overflow if sustained event production exceeds drain
/// capacity; overflow keeps the index uncertain and triggers reconciliation.
struct CpuGovernor {
    target: f64,
    cpu_ref: f64,
    wall_ref: Instant,
    queue_high_watermark_bytes: u64,
    queue_low_watermark_bytes: u64,
    queue_pressure_override: bool,
    queue_pressure_started: Option<Instant>,
    queue_pressure_total: Duration,
    queue_pressure_episodes: u64,
    last_queue_bytes: Option<u64>,
    throttle_sleep_total: Duration,
    telemetry_logged_at: Instant,
    telemetry_events_seen: u64,
    telemetry_events_resolved: u64,
    telemetry_initialized: bool,
}

#[derive(Clone, Copy)]
struct ThrottleWatchSnapshot {
    pending: usize,
    events_seen: u64,
    events_resolved: u64,
    updates_committed: u64,
    last_commit_at: i64,
    updates_dropped: u64,
    behind_ms: u64,
    kernel_overflow_notifications: u64,
    uncertain: bool,
    scan_first: bool,
}

fn publish_throttled_watch_status(db: &Path, snapshot: ThrottleWatchSnapshot, elapsed: Duration) {
    // The kernel queue is not observed while the governor sleeps, so keep the
    // metrics available without claiming the queue was empty at publication.
    crate::util::write_watch_status_with_mode(
        db,
        snapshot.pending,
        false,
        snapshot.events_seen,
        snapshot.events_resolved,
        snapshot.updates_committed,
        snapshot.last_commit_at,
        snapshot.updates_dropped,
        snapshot
            .behind_ms
            .saturating_add(elapsed.as_millis().min(u64::MAX as u128) as u64),
        MAX_PENDING,
        snapshot.kernel_overflow_notifications,
        snapshot.uncertain,
        snapshot.scan_first,
    );
}

impl CpuGovernor {
    fn new(target_frac: f64, max_queued_events: u64) -> Self {
        let (queue_high_watermark_bytes, queue_low_watermark_bytes) =
            fanotify_queue_watermarks(max_queued_events);
        Self {
            target: target_frac.clamp(0.01, 1.0),
            cpu_ref: thread_cpu_secs(),
            wall_ref: Instant::now(),
            queue_high_watermark_bytes,
            queue_low_watermark_bytes,
            queue_pressure_override: false,
            queue_pressure_started: None,
            queue_pressure_total: Duration::ZERO,
            queue_pressure_episodes: 0,
            last_queue_bytes: None,
            throttle_sleep_total: Duration::ZERO,
            telemetry_logged_at: Instant::now(),
            telemetry_events_seen: 0,
            telemetry_events_resolved: 0,
            telemetry_initialized: false,
        }
    }

    fn throttle(&mut self, fanotify_fd: RawFd, db: &Path, watch: Option<ThrottleWatchSnapshot>) {
        let dcpu = (thread_cpu_secs() - self.cpu_ref).max(0.0);
        let dwall = self.wall_ref.elapsed().as_secs_f64();
        // Sleep the shortfall in small chunks so a SIGTERM/SIGHUP is handled
        // within ~50ms, and refresh
        // the heartbeat every ~2s so a long throttle never makes the daemon look
        // dead to `status`/`dux scan`.
        let mut remaining = throttle_sleep_secs(dcpu, dwall, self.target);
        let mut hb = Instant::now();
        let throttle_started = Instant::now();
        // A pending reconciliation request must not bypass the CPU governor.
        // RESCAN remains latched while queued events drain; treating it as an
        // interrupt here turns the hottest recovery path into an unthrottled
        // busy loop precisely when the host is already under churn. A scan
        // becomes eligible in the main loop before this governor is reached,
        // so only shutdown needs to interrupt the duty-cycle sleep.
        while remaining > 0.0 && !SHUTDOWN.load(Ordering::SeqCst) {
            self.update_queue_pressure(fanotify_fd);
            if self.queue_pressure_override {
                self.log_pressure_metrics_if_due(watch);
                break;
            }
            let chunk = remaining.min(0.05);
            let sleep_started = Instant::now();
            std::thread::sleep(Duration::from_secs_f64(chunk));
            self.throttle_sleep_total += sleep_started.elapsed();
            remaining -= chunk;
            if hb.elapsed() >= Duration::from_secs(2) {
                crate::util::write_heartbeat(db);
                if let Some(snapshot) = watch {
                    publish_throttled_watch_status(db, snapshot, throttle_started.elapsed());
                }
                hb = Instant::now();
            }
            self.log_pressure_metrics_if_due(watch);
        }
        self.cpu_ref = thread_cpu_secs();
        self.wall_ref = Instant::now();
    }

    fn update_queue_pressure(&mut self, fanotify_fd: RawFd) {
        let queued_bytes = read_fanotify_queue_bytes(fanotify_fd);
        self.last_queue_bytes = queued_bytes;
        let readable = if queued_bytes.is_none() {
            fanotify_queue_readable(fanotify_fd)
        } else {
            None
        };
        let pressure = fanotify_queue_pressure_override(
            self.queue_pressure_override,
            queued_bytes,
            readable,
            self.queue_high_watermark_bytes,
            self.queue_low_watermark_bytes,
        );
        self.set_queue_pressure(pressure, Instant::now());
    }

    fn set_queue_pressure(&mut self, pressure: bool, now: Instant) {
        if pressure == self.queue_pressure_override {
            return;
        }
        self.queue_pressure_override = pressure;
        if pressure {
            self.queue_pressure_started = Some(now);
            self.queue_pressure_episodes = self.queue_pressure_episodes.saturating_add(1);
            tracing::info!(
                queue_bytes = self.last_queue_bytes,
                queue_pressure_episodes = self.queue_pressure_episodes,
                high_watermark_bytes = self.queue_high_watermark_bytes,
                low_watermark_bytes = self.queue_low_watermark_bytes,
                "temporarily bypassing event-thread CPU sleep to drain fanotify backlog"
            );
        } else if let Some(started) = self.queue_pressure_started.take() {
            let elapsed = now.saturating_duration_since(started);
            self.queue_pressure_total += elapsed;
            tracing::info!(
                queue_bytes = self.last_queue_bytes,
                queue_pressure_episode_ms = elapsed.as_millis(),
                queue_pressure_total_ms = self.queue_pressure_total.as_millis(),
                queue_pressure_episodes = self.queue_pressure_episodes,
                "fanotify backlog fell below the low watermark"
            );
        }
    }

    fn log_pressure_metrics_if_due(&mut self, watch: Option<ThrottleWatchSnapshot>) {
        if !self.telemetry_initialized {
            if let Some(snapshot) = watch {
                self.telemetry_events_seen = snapshot.events_seen;
                self.telemetry_events_resolved = snapshot.events_resolved;
            }
            self.telemetry_logged_at = Instant::now();
            self.telemetry_initialized = true;
            return;
        }
        if self.telemetry_logged_at.elapsed() < Duration::from_secs(10) {
            return;
        }
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.telemetry_logged_at);
        let queue_pressure_total = self.queue_pressure_total
            + self
                .queue_pressure_started
                .map(|started| now.saturating_duration_since(started))
                .unwrap_or_default();
        let (events_seen_per_sec, events_resolved_per_sec) = watch
            .map(|snapshot| {
                let seconds = elapsed.as_secs_f64().max(f64::EPSILON);
                (
                    snapshot
                        .events_seen
                        .saturating_sub(self.telemetry_events_seen) as f64
                        / seconds,
                    snapshot
                        .events_resolved
                        .saturating_sub(self.telemetry_events_resolved) as f64
                        / seconds,
                )
            })
            .unwrap_or((0.0, 0.0));
        if let Some(snapshot) = watch {
            self.telemetry_events_seen = snapshot.events_seen;
            self.telemetry_events_resolved = snapshot.events_resolved;
        }
        tracing::info!(
            queue_bytes = self.last_queue_bytes,
            queue_pressure_active = self.queue_pressure_override,
            queue_pressure_episodes = self.queue_pressure_episodes,
            queue_pressure_total_ms = queue_pressure_total.as_millis(),
            throttle_sleep_total_ms = self.throttle_sleep_total.as_millis(),
            events_seen_per_sec,
            events_resolved_per_sec,
            "fanotify watcher pressure metrics"
        );
        self.telemetry_logged_at = now;
    }
}

fn fanotify_queue_watermarks(max_queued_events: u64) -> (u64, u64) {
    let event_slots = max_queued_events.max(1);
    let minimum_record_bytes = FAN_EVENT_METADATA_LEN as u64;
    let capacity_lower_bound = event_slots.saturating_mul(minimum_record_bytes);
    let high = capacity_lower_bound.saturating_mul(FANOTIFY_QUEUE_HIGH_WATERMARK_PERCENT) / 100;
    let low = capacity_lower_bound.saturating_mul(FANOTIFY_QUEUE_LOW_WATERMARK_PERCENT) / 100;
    (
        high.max(minimum_record_bytes),
        low.max(minimum_record_bytes).min(high),
    )
}

fn fanotify_queue_pressure_override(
    currently_overridden: bool,
    queued_bytes: Option<u64>,
    queue_readable: Option<bool>,
    high_watermark_bytes: u64,
    low_watermark_bytes: u64,
) -> bool {
    match queued_bytes {
        Some(bytes) if currently_overridden => bytes > low_watermark_bytes,
        Some(bytes) => bytes >= high_watermark_bytes,
        None => queue_readable.unwrap_or(true),
    }
}

fn read_fanotify_queue_bytes(fanotify_fd: RawFd) -> Option<u64> {
    let mut bytes: libc::c_int = 0;
    let result = unsafe { libc::ioctl(fanotify_fd, FANOTIFY_FIONREAD, &mut bytes) };
    (result == 0 && bytes >= 0).then_some(bytes as u64)
}

fn fanotify_queue_readable(fanotify_fd: RawFd) -> Option<bool> {
    let mut descriptor = libc::pollfd {
        fd: fanotify_fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
    if result < 0 {
        return None;
    }
    Some(result > 0 && descriptor.revents != 0)
}

fn fanotify_max_queued_events() -> u64 {
    std::fs::read_to_string("/proc/sys/fs/fanotify/max_queued_events")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_FANOTIFY_MAX_QUEUED_EVENTS)
}

/// How long to sleep to hold a `target` duty cycle, given the CPU and wall time
/// spent since the last checkpoint. Pure (testable): to keep cpu/wall ≤ target,
/// the work must span at least `dcpu/target` wall seconds; sleep the shortfall,
/// bounded by MAX_THROTTLE_SLEEP_SECS so a pathological burst can't stall forever.
fn throttle_sleep_secs(dcpu: f64, dwall: f64, target: f64) -> f64 {
    let need_wall = dcpu / target.clamp(0.01, 1.0);
    (need_wall - dwall).clamp(0.0, MAX_THROTTLE_SLEEP_SECS)
}

// ---- fanotify constants (FID / dirent-event mode) ----
const FAN_CLASS_NOTIF: libc::c_uint = 0x0000_0000;
const FAN_CLOEXEC: libc::c_uint = 0x0000_0001;
const FAN_NONBLOCK: libc::c_uint = 0x0000_0002;
const FAN_REPORT_FID: libc::c_uint = 0x0000_0200;
const FAN_REPORT_TARGET_FID: libc::c_uint = 0x0000_1000;
const FAN_REPORT_DFID_NAME: libc::c_uint = 0x0000_0c00;
const FAN_REPORT_DFID_NAME_TARGET: libc::c_uint =
    FAN_REPORT_DFID_NAME | FAN_REPORT_FID | FAN_REPORT_TARGET_FID;
const FAN_MARK_ADD: libc::c_uint = 0x0000_0001;
const FAN_MARK_FILESYSTEM: libc::c_uint = 0x0000_0100;

// event mask bits
const FAN_MODIFY: u64 = 0x0000_0002;
const FAN_ATTRIB: u64 = 0x0000_0004; // metadata change (owner, mode, mtime, nlink)
const FAN_CLOSE_WRITE: u64 = 0x0000_0008;
const FAN_MOVED_FROM: u64 = 0x0000_0040;
const FAN_MOVED_TO: u64 = 0x0000_0080;
const FAN_CREATE: u64 = 0x0000_0100;
const FAN_DELETE: u64 = 0x0000_0200;
const FAN_Q_OVERFLOW: u64 = 0x0000_4000;
const FAN_ONDIR: u64 = 0x4000_0000;

const FAN_EVENT_METADATA_LEN: usize = 24;
const FAN_EVENT_INFO_TYPE_DFID_NAME: u8 = 2;
const FAN_EVENT_INFO_TYPE_FID: u8 = 1;
const MAX_HANDLE_SZ: usize = 128;
const FANOTIFY_METADATA_VERSION: u8 = 3;
/// Bound open_by_handle_at work for one event-parser slice. A burst touching
/// more directories than this widens to a full scan instead of synchronously
/// resolving an unbounded number of distinct scopes on the watcher thread.
const MAX_FANOTIFY_HINT_HANDLES: usize = 32;

#[repr(C)]
struct FanotifyEventMetadata {
    event_len: u32,
    vers: u8,
    reserved: u8,
    metadata_len: u16,
    mask: u64,
    fd: i32,
    pid: i32,
}
// The read_unaligned of this struct is bounds-checked against FAN_EVENT_METADATA_LEN,
// so that constant MUST equal the real struct size — else the guard under-protects
// the read. Assert it at compile time so editing the struct can't silently drift.
const _: () = assert!(std::mem::size_of::<FanotifyEventMetadata>() == FAN_EVENT_METADATA_LEN);

/// Remove well-formed records generated by this process before they consume
/// bounded hint-ring capacity. `None` means the chunk is malformed or
/// ambiguous, so the caller must forward it unchanged to the canonical parser.
fn filter_self_fanotify_records(buf: &[u8]) -> Option<Vec<u8>> {
    let mut offset = 0usize;
    let mut kept = Vec::with_capacity(buf.len());
    let self_pid = std::process::id() as i32;
    while offset < buf.len() {
        if buf.len().saturating_sub(offset) < FAN_EVENT_METADATA_LEN {
            return None;
        }
        let meta: FanotifyEventMetadata = unsafe {
            std::ptr::read_unaligned(buf[offset..].as_ptr().cast::<FanotifyEventMetadata>())
        };
        let event_len = meta.event_len as usize;
        let metadata_len = meta.metadata_len as usize;
        if meta.vers != FANOTIFY_METADATA_VERSION
            || metadata_len < FAN_EVENT_METADATA_LEN
            || event_len < metadata_len
            || event_len > buf.len().saturating_sub(offset)
        {
            return None;
        }
        let end = offset + event_len;
        if meta.pid != self_pid || meta.mask & FAN_Q_OVERFLOW != 0 {
            kept.extend_from_slice(&buf[offset..end]);
        }
        offset = end;
    }
    Some(kept)
}

/// Count event metadata in scan-first mode without allocating a user-space
/// event queue or decoding file handles. The periodic full walk is the source
/// of truth; fanotify is retained only to expose event rate and kernel loss.
#[cfg(test)]
fn count_fanotify_records(buf: &[u8]) -> (u64, u64, bool) {
    let (seen, overflows, consumed, malformed) = count_fanotify_records_available(buf);
    (seen, overflows, malformed || consumed != buf.len())
}

fn count_fanotify_records_available(buf: &[u8]) -> (u64, u64, usize, bool) {
    let mut offset = 0usize;
    let mut seen = 0u64;
    let mut overflows = 0u64;
    while offset < buf.len() {
        if buf.len().saturating_sub(offset) < FAN_EVENT_METADATA_LEN {
            return (seen, overflows, offset, false);
        }
        let meta: FanotifyEventMetadata = unsafe {
            std::ptr::read_unaligned(buf[offset..].as_ptr().cast::<FanotifyEventMetadata>())
        };
        let event_len = meta.event_len as usize;
        let metadata_len = meta.metadata_len as usize;
        if meta.vers != FANOTIFY_METADATA_VERSION
            || metadata_len < FAN_EVENT_METADATA_LEN
            || event_len < metadata_len
        {
            return (seen, overflows, buf.len(), true);
        }
        if event_len > buf.len().saturating_sub(offset) {
            return (seen, overflows, offset, false);
        }
        if meta.mask & FAN_Q_OVERFLOW != 0 {
            overflows = overflows.saturating_add(1);
        } else if meta.pid != std::process::id() as i32 {
            seen = seen.saturating_add(1);
        }
        offset += event_len;
    }
    (seen, overflows, offset, false)
}

/// Validate fanotify records and coalesce parent handles without resolving
/// paths or touching SQLite. One unresolved record or saturated handle set
/// widens recovery to a full observation.
fn parse_fanotify_hints(
    mut buf: &[u8],
    max_events: usize,
) -> (u64, usize, bool, Vec<(Vec<u8>, u64)>) {
    let input_len = buf.len();
    let (mut overflows, mut parsed, mut consumed) = (0u64, 0usize, 0usize);
    let mut lost = false;
    let mut handles = HashMap::<Vec<u8>, u64>::new();
    while buf.len() >= FAN_EVENT_METADATA_LEN && parsed < max_events {
        let meta: FanotifyEventMetadata =
            unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const FanotifyEventMetadata) };
        let event_len = meta.event_len as usize;
        let metadata_len = meta.metadata_len as usize;
        if meta.vers != FANOTIFY_METADATA_VERSION
            || metadata_len < FAN_EVENT_METADATA_LEN
            || event_len < metadata_len
            || event_len > buf.len()
        {
            lost = true;
            consumed = input_len;
            break;
        }
        if meta.mask & FAN_Q_OVERFLOW != 0 {
            overflows = overflows.saturating_add(1);
            lost = true;
        } else {
            // The scan/database writer runs in this daemon process. Its own
            // SQLite, WAL, progress, and staging-file notifications are not
            // user-tree changes (the scanner excludes those artifacts), so
            // they must not make every observation appear raced.
            if meta.pid != std::process::id() as i32 {
                let mut off = metadata_len;
                let mut parent_handle = None;
                let mut info_valid = true;
                while off < event_len {
                    if off.saturating_add(std::mem::size_of::<InfoHeader>()) > event_len {
                        info_valid = false;
                        break;
                    }
                    let header: InfoHeader = unsafe {
                        std::ptr::read_unaligned(buf[off..].as_ptr().cast::<InfoHeader>())
                    };
                    let record_len = header.len as usize;
                    if record_len < std::mem::size_of::<InfoHeader>()
                        || off.saturating_add(record_len) > event_len
                    {
                        info_valid = false;
                        break;
                    }
                    if header.info_type == FAN_EVENT_INFO_TYPE_DFID_NAME {
                        if parent_handle.is_some() {
                            info_valid = false;
                            break;
                        }
                        let payload_start = off + std::mem::size_of::<InfoHeader>();
                        let payload_end = off + record_len;
                        let payload = &buf[payload_start..payload_end];
                        if payload.len() < 16 {
                            info_valid = false;
                            break;
                        }
                        let handle_bytes = u32::from_ne_bytes(
                            payload[8..12].try_into().expect("validated FID prefix"),
                        ) as usize;
                        let handle_end = 16usize.saturating_add(handle_bytes);
                        if handle_bytes == 0
                            || handle_bytes > MAX_HANDLE_SZ
                            || handle_end >= payload.len()
                            || !payload[handle_end..].contains(&0)
                        {
                            info_valid = false;
                            break;
                        }
                        // The filename is deliberately discarded. Parent handle
                        // alone is the coalescing key and sufficient to scope a
                        // dirty-directory hint after the reader has drained.
                        parent_handle = Some(payload[..handle_end].to_vec());
                    }
                    off += record_len;
                }
                if !info_valid || off != event_len || parent_handle.is_none() {
                    lost = true;
                } else if let Some(handle) = parent_handle {
                    if let Some(count) = handles.get_mut(&handle) {
                        *count = count.saturating_add(1);
                    } else if handles.len() < MAX_FANOTIFY_HINT_HANDLES {
                        handles.insert(handle, 1);
                    } else {
                        // Preserve bounded memory and explicitly widen to a full
                        // scan rather than silently discarding an unseen scope.
                        lost = true;
                    }
                }
            }
            // FID-mode notifications normally use FAN_NOFD. Close defensively if
            // a kernel or future mark mode supplies an event descriptor.
            if meta.fd >= 0 {
                unsafe { libc::close(meta.fd) };
            }
        }
        parsed += 1;
        consumed += event_len;
        buf = &buf[event_len..];
    }
    if consumed < input_len && buf.len() < FAN_EVENT_METADATA_LEN {
        consumed = input_len;
    }
    (overflows, consumed, lost, handles.into_iter().collect())
}

/// Resolve each coalesced parent at most once, then discard events outside the
/// indexed root. Failure to prove scope widens to a full-root observation.
fn resolve_fanotify_hint_scope(
    handles: Vec<(Vec<u8>, u64)>,
    fsfds: &HashMap<(i32, i32), RawFd>,
    handle_capable_fsids: &mut HashSet<(i32, i32)>,
    ambiguous_fsids: &HashSet<(i32, i32)>,
    root: &Path,
) -> (u64, bool, Vec<PathBuf>) {
    let mut in_scope = 0u64;
    let mut lost = false;
    let mut priority_paths = Vec::new();
    for (handle, count) in handles {
        if payload_fsid(&handle).is_some_and(|fsid| ambiguous_fsids.contains(&fsid)) {
            // One physical filesystem can have multiple visible mount paths.
            // open_by_handle_at returns a path through the selected mount fd,
            // not necessarily the alias that generated this event. Treat that
            // filesystem as in scope to avoid a false negative.
            in_scope = in_scope.saturating_add(count);
            continue;
        }
        match resolve_handle(&handle, fsfds, handle_capable_fsids) {
            EntryResolution::Resolved(parent, _) if parent.starts_with(root) => {
                in_scope = in_scope.saturating_add(count);
                if priority_paths.len() < 256 && !priority_paths.contains(&parent) {
                    priority_paths.push(parent);
                }
            }
            EntryResolution::Resolved(_, _) => {}
            EntryResolution::Stale(_) | EntryResolution::Unresolved => lost = true,
        }
    }
    (in_scope, lost, priority_paths)
}

#[repr(C)]
struct InfoHeader {
    info_type: u8,
    pad: u8,
    len: u16,
}
// Event walkers validate at least four remaining bytes before this unaligned
// read. Keep the C-layout size tied to that guard at compile time.
const _: () = assert!(std::mem::size_of::<InfoHeader>() == 4);

#[repr(C)]
#[derive(Clone, Copy)]
struct FileHandle {
    handle_bytes: u32,
    handle_type: i32,
    f_handle: [u8; MAX_HANDLE_SZ],
}

#[derive(Clone)]
struct ResolvedEventRecord {
    entry: Option<(PathBuf, std::ffi::OsString)>,
    target_inode: Option<(i64, i64)>,
    stale_target_fsid: Option<(i32, i32)>,
    stale_parent: Option<StaleParentEntry>,
    info_type_mask: u64,
    info_record_count: usize,
    info_records_complete: bool,
}

/// A DFID_NAME record whose directory handle returned `ESTALE`. The name is
/// retained so an indexed parent can still be refreshed by path.
#[derive(Clone, Debug, PartialEq, Eq)]
struct StaleParentEntry {
    fsid: (i32, i32),
    handle_type: i32,
    handle: Vec<u8>,
    name: std::ffi::OsString,
}

/// Outcome for an otherwise unresolved event whose parent directory handle is
/// stale on a filesystem whose handles are known to round-trip.
#[derive(Debug, PartialEq, Eq)]
enum StaleParentOutcome {
    /// The parent inode is still indexed: refresh `parent/name` from final disk
    /// state. This is correct whether the directory was deleted, replaced, or
    /// the stale handle was spurious, because the batched writer stats the path.
    RefreshIndexedPath(PathBuf),
    /// The stale parent inode has no indexed path. A freed directory that was
    /// never committed cannot own indexed descendants; any indexed ancestor's
    /// removal is reported by its own resolvable parent event.
    ParentNotIndexed,
}

// `f_type` is signed on glibc and unsigned on musl; compare normalized u64
// bit patterns, as `scan::fs_magic` does.
const EXT4_SUPER_MAGIC: u64 = 0xEF53;
// generic_encode_ino32_fh() layouts used by ext4 for fanotify FIDs.
const FILEID_INO32_GEN: i32 = 1;
const FILEID_INO32_GEN_PARENT: i32 = 2;

/// Inode number encoded in an ext4 `FILEID_INO32_GEN[_PARENT]` handle. Other
/// handle types (and other filesystems, which may reuse type numbers with a
/// different layout) are rejected by the caller's filesystem check.
#[cfg(test)]
fn ino32_handle_inode(handle_type: i32, handle: &[u8]) -> Option<u32> {
    ino32_handle_identity(handle_type, handle).map(|(ino, _)| ino)
}

fn ino32_handle_identity(handle_type: i32, handle: &[u8]) -> Option<(u32, u32)> {
    if handle_type != FILEID_INO32_GEN && handle_type != FILEID_INO32_GEN_PARENT {
        return None;
    }
    let ino = u32::from_ne_bytes(handle.get(0..4)?.try_into().ok()?);
    let generation = u32::from_ne_bytes(handle.get(4..8)?.try_into().ok()?);
    (ino != 0).then_some((ino, generation))
}

fn ext4_path_handle_identity(path: &Path) -> Option<(u32, u32)> {
    use std::os::unix::ffi::OsStrExt;

    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut handle = FileHandle {
        handle_bytes: MAX_HANDLE_SZ as u32,
        handle_type: 0,
        f_handle: [0; MAX_HANDLE_SZ],
    };
    let mut mount_id: libc::c_int = 0;
    let result = unsafe {
        libc::name_to_handle_at(
            libc::AT_FDCWD,
            path.as_ptr(),
            (&mut handle as *mut FileHandle).cast::<libc::file_handle>(),
            &mut mount_id,
            0,
        )
    };
    if result != 0 || handle.handle_bytes as usize > MAX_HANDLE_SZ {
        return None;
    }
    ino32_handle_identity(
        handle.handle_type,
        &handle.f_handle[..handle.handle_bytes as usize],
    )
}

fn ext4_path_matches_handle(path: &Path, dev: i64, ino: i64, generation: u32) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    metadata.is_dir()
        && crate::util::indexed_identity(&metadata).ok() == Some((dev, ino))
        && ext4_path_handle_identity(path) == Some((ino as u32, generation))
}

/// Only complete records carrying exactly one DFID_NAME and at most one FID
/// qualify; anything else keeps the conservative lost-coverage behavior.
fn stale_parent_event_shape_ok(
    record: &ResolvedEventRecord,
    handle_capable_fsids: &HashSet<(i32, i32)>,
) -> bool {
    let dfid = 1u64 << FAN_EVENT_INFO_TYPE_DFID_NAME;
    let fid = 1u64 << FAN_EVENT_INFO_TYPE_FID;
    let shape_ok = (record.info_type_mask == dfid && record.info_record_count == 1)
        || (record.info_type_mask == dfid | fid && record.info_record_count == 2);
    let name_safe = record.stale_parent.as_ref().is_some_and(|parent| {
        parent.name != ".." && !parent.name.as_os_str().as_bytes().contains(&b'/')
    });
    record.entry.is_none()
        && record.target_inode.is_none()
        && record.info_records_complete
        && shape_ok
        && name_safe
        && record
            .stale_parent
            .as_ref()
            .is_some_and(|parent| handle_capable_fsids.contains(&parent.fsid))
}

/// Indexed (dev, inode) of a stale ext4 parent handle, or `None` when the
/// mount is not ext4 or the handle layout is not the ext4 ino32 encoding.
fn stale_ext4_parent_identity(
    parent: &StaleParentEntry,
    fsfds: &HashMap<(i32, i32), RawFd>,
) -> Option<(i64, i64, u32)> {
    let (ino, generation) = ino32_handle_identity(parent.handle_type, &parent.handle)?;
    let mount_fd = *fsfds.get(&parent.fsid)?;
    let mut fs = std::mem::MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::fstatfs(mount_fd, fs.as_mut_ptr()) } != 0 {
        return None;
    }
    if unsafe { fs.assume_init() }.f_type as u64 != EXT4_SUPER_MAGIC {
        return None;
    }
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(mount_fd, st.as_mut_ptr()) } != 0 {
        return None;
    }
    let dev = i64::try_from(unsafe { st.assume_init() }.st_dev).ok()?;
    Some((dev, i64::from(ino), generation))
}

fn classify_stale_parent_event(
    record: &ResolvedEventRecord,
    handle_capable_fsids: &HashSet<(i32, i32)>,
    fsfds: &HashMap<(i32, i32), RawFd>,
    conn: &rusqlite::Connection,
    parent_paths: &mut IndexedParentPathCache,
) -> Result<Option<StaleParentOutcome>> {
    if !stale_parent_event_shape_ok(record, handle_capable_fsids) {
        return Ok(None);
    }
    let Some(parent) = record.stale_parent.as_ref() else {
        return Ok(None);
    };
    let Some((dev, ino, generation)) = stale_ext4_parent_identity(parent, fsfds) else {
        return Ok(None);
    };
    Ok(Some(match parent_paths.resolve(conn, dev, ino)? {
        Some(dir) => {
            match std::fs::symlink_metadata(&dir) {
                Ok(_) if !ext4_path_matches_handle(&dir, dev, ino, generation) => {
                    // An inode number can be reused. If the path now names
                    // another generation, do not apply an old event to it.
                    return Ok(None);
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // The indexed parent path is gone, so reconciling its old
                    // child path is safe and removes stale rows if still present.
                }
                Err(_) => return Ok(None),
            }
            let full = if parent.name.is_empty() || parent.name == "." {
                dir
            } else {
                dir.join(&parent.name)
            };
            StaleParentOutcome::RefreshIndexedPath(full)
        }
        None if stale_parent_has_index_footprint(conn, dev, ino)? => {
            // A missing parent path is not enough to prove this event is
            // irrelevant: a prior partial update can leave the inode or its
            // child edges in the index without a resolvable parent path. Keep
            // coverage uncertain so a reconciliation can remove/repair those
            // rows instead of silently blessing an orphaned subtree.
            return Ok(None);
        }
        None => StaleParentOutcome::ParentNotIndexed,
    }))
}

fn stale_parent_has_index_footprint(
    conn: &rusqlite::Connection,
    dev: i64,
    ino: i64,
) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM inodes WHERE dev_id=?1 AND inode=?2)
             OR EXISTS(SELECT 1 FROM dirents WHERE dev_id=?1 AND inode=?2)
             OR EXISTS(SELECT 1 FROM dirents WHERE parent_dev=?1 AND parent_inode=?2)",
        params![dev, ino],
        |row| row.get(0),
    )?)
}

fn event_identity_unresolved(record: &ResolvedEventRecord) -> bool {
    record.entry.is_none() && record.target_inode.is_none()
}

fn self_generated_attrib_event(mask: u64, event_pid: i32) -> bool {
    event_pid == std::process::id() as i32 && mask & !FAN_ONDIR == FAN_ATTRIB
}

fn stale_target_attrib_is_benign(
    mask: u64,
    record: &ResolvedEventRecord,
    handle_capable_fsids: &HashSet<(i32, i32)>,
) -> bool {
    mask & !FAN_ONDIR == FAN_ATTRIB
        && record.entry.is_none()
        && record.info_type_mask == (1u64 << FAN_EVENT_INFO_TYPE_FID)
        && record.info_record_count == 1
        && record.info_records_complete
        && record
            .stale_target_fsid
            .is_some_and(|fsid| handle_capable_fsids.contains(&fsid))
}

fn unresolved_event_log_permit_at(
    now: u64,
    last_logged_at: &AtomicU64,
    suppressed: &AtomicU64,
) -> Option<u64> {
    let mut previous = last_logged_at.load(Ordering::Relaxed);
    loop {
        if now.saturating_sub(previous) < UNRESOLVED_EVENT_LOG_INTERVAL_SECS {
            suppressed.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        match last_logged_at.compare_exchange(previous, now, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Some(suppressed.swap(0, Ordering::Relaxed)),
            Err(observed) => previous = observed,
        }
    }
}

fn unresolved_event_log_permit() -> Option<u64> {
    let now = now_secs().max(0) as u64;
    unresolved_event_log_permit_at(
        now,
        &LAST_UNRESOLVED_EVENT_LOG_AT,
        &UNRESOLVED_EVENTS_SUPPRESSED,
    )
}

fn event_info_type_mask(event: &[u8], meta_len: usize) -> u64 {
    let mut off = meta_len.max(FAN_EVENT_METADATA_LEN);
    let mut types = 0u64;
    while off + 4 <= event.len() {
        let hdr: InfoHeader =
            unsafe { std::ptr::read_unaligned(event[off..].as_ptr() as *const InfoHeader) };
        let len = hdr.len as usize;
        if len < 4 || off + len > event.len() {
            break;
        }
        if hdr.info_type < u64::BITS as u8 {
            types |= 1u64 << hdr.info_type;
        }
        off += len;
    }
    types
}

fn unresolved_target_diagnostic(
    event: &[u8],
    meta_len: usize,
    fsfds: &HashMap<(i32, i32), RawFd>,
) -> String {
    let mut off = meta_len.max(FAN_EVENT_METADATA_LEN);
    while off + 4 <= event.len() {
        let hdr: InfoHeader =
            unsafe { std::ptr::read_unaligned(event[off..].as_ptr() as *const InfoHeader) };
        let len = hdr.len as usize;
        if len < 4 || off + len > event.len() {
            return "malformed info record".to_owned();
        }
        if hdr.info_type == FAN_EVENT_INFO_TYPE_FID {
            let payload = &event[off + 4..off + len];
            if payload.len() < 16 {
                return format!("short FID payload: {} bytes", payload.len());
            }
            let fsid = (
                i32::from_ne_bytes(payload[0..4].try_into().unwrap()),
                i32::from_ne_bytes(payload[4..8].try_into().unwrap()),
            );
            let Some(&mount_fd) = fsfds.get(&fsid) else {
                return format!("FID filesystem {:?} has no watched mount descriptor", fsid);
            };
            let handle_bytes = u32::from_ne_bytes(payload[8..12].try_into().unwrap()) as usize;
            let handle_type = i32::from_ne_bytes(payload[12..16].try_into().unwrap());
            if handle_bytes == 0
                || handle_bytes > MAX_HANDLE_SZ
                || payload.len() < 16 + handle_bytes
            {
                return format!(
                    "invalid FID handle length: bytes={handle_bytes} payload_bytes={}",
                    payload.len()
                );
            }
            let mut fh = FileHandle {
                handle_bytes: handle_bytes as u32,
                handle_type,
                f_handle: [0u8; MAX_HANDLE_SZ],
            };
            fh.f_handle[..handle_bytes].copy_from_slice(&payload[16..16 + handle_bytes]);
            let fd = unsafe {
                libc::syscall(
                    libc::SYS_open_by_handle_at,
                    mount_fd,
                    &mut fh as *mut FileHandle,
                    libc::O_PATH | libc::O_CLOEXEC,
                ) as RawFd
            };
            if fd < 0 {
                let error = std::io::Error::last_os_error();
                return format!(
                    "open_by_handle_at failed: fsid={:?} handle_type={handle_type} handle_bytes={handle_bytes} errno={:?} error={error}",
                    fsid,
                    error.raw_os_error()
                );
            }
            unsafe { libc::close(fd) };
            return format!(
                "open_by_handle_at succeeded on diagnostic retry: fsid={:?} handle_type={handle_type} handle_bytes={handle_bytes}",
                fsid
            );
        }
        off += len;
    }
    "no target FID record".to_owned()
}

/// What happened to a path within the flush window (latest event wins).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Op {
    Upsert,    // create / modify / close-write -> (re)stat and insert/update
    Reconcile, // combined CREATE|DELETE mask -> inspect the path's final state
    Delete,    // unlink / rmdir                -> remove node + subtree
    MovedFrom, // rename source                 -> relocated away (or moved out)
    MovedTo,   // rename dest                   -> relocate the inode here, keep subtree
}

/// A FAN_MOVED_FROM whose matching FAN_MOVED_TO hasn't been seen yet.
///
/// NOTE (verified against /usr/include/linux/fanotify.h): `fanotify_event_metadata`
/// has fields {event_len, vers, reserved, metadata_len, mask, fd, pid} and NO
/// `cookie` — that is an *inotify* field (`struct inotify_event.cookie`). fanotify
/// could not pair a rename's two halves at all until `FAN_RENAME` (Linux 5.13),
/// which we don't require. So we pair by INODE IDENTITY instead: when the halves
/// land in different flushes, an unmatched FROM is held for a short grace period.
/// A TO arriving during the grace relocates the inode (zero-delta rename); once
/// the kernel queue is drained and the grace expires, a true move-out is unlinked.
/// The queue-drained check prevents expiring a pair still waiting in the kernel.
#[derive(Clone)]
struct DeferredFrom {
    pdev: i64,
    pino: i64,
    name: Vec<u8>,
    birth_ns: i64,
    expires: Instant,
}

/// Growth-alert configuration.
pub struct AlertConfig {
    pub threshold: i64,
    pub window: i64,
    pub exec: Option<String>,
    pub keep_capabilities: bool,
    pub debounce: i64,
}

/// True if the index at `db` is already rooted at `root_canon` (its stored
/// root_dev/root_inode match). Missing root identity requests a rebuild; I/O or
/// malformed identity errors are surfaced so startup cannot assume a match.
fn root_matches(db: &Path, root_canon: &Path) -> Result<bool> {
    let metadata = std::fs::symlink_metadata(root_canon)
        .with_context(|| format!("cannot stat configured index root {}", root_canon.display()))?;
    let want = crate::util::indexed_identity(&metadata)
        .context("configured root identity cannot be represented by the index")?;
    let store = Store::open_ro(db).context("opening index to verify its configured root")?;
    Ok(crate::query::index_root(&store)? == Some(want))
}

/// A fresh index or a schema/root replacement has no committed tree that can
/// accept live path events yet. Keep those events in the bounded pending map
/// until the first candidate generation is installed; applying them to an empty
/// database repeatedly fails ancestor lookup while recovery is deferred/retried.
fn committed_root_indexed(store: &Store) -> Result<bool> {
    Ok(store.conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM inodes
            WHERE dev_id=CAST((SELECT value FROM meta WHERE key='root_dev') AS INTEGER)
              AND inode=CAST((SELECT value FROM meta WHERE key='root_inode') AS INTEGER)
              AND kind='d'
        )",
        [],
        |row| row.get(0),
    )?)
}

/// Persist uncertainty before starting a watcher that cannot prove continuity.
/// Failing to record the marker must fail startup closed; otherwise status can
/// present a stale index as clean after a restart or partial watch setup.
fn mark_index_dirty(store: &Store, reason: &str) -> Result<()> {
    store
        .set_meta("dirty_since", &now_secs().to_string())
        .context("persisting dirty index marker")?;
    store
        .set_meta("dirty_reason", reason)
        .context("persisting dirty index reason")?;
    Ok(())
}

/// Persist the dirty marker on a best-effort path where recovery is already
/// latched in memory. Never hide failure to persist it: disk-full/SQLite errors
/// are operationally important, but must not abort event parsing or shutdown.
fn persist_dirty_marker_best_effort(store: &Store, cause: &'static str) -> bool {
    let timestamp = now_secs().to_string();
    let result = (|| -> Result<()> {
        let tx = store.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO meta(key,value) VALUES('dirty_since',?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [&timestamp],
        )?;
        tx.execute(
            "INSERT INTO meta(key,value) VALUES('dirty_reason',?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [cause],
        )?;
        tx.commit()?;
        Ok(())
    })();
    if let Err(error) = result {
        if dirty_marker_failure_log_allowed() {
            tracing::error!(%error, cause, "could not persist dirty index marker and reason; recovery remains required");
        }
        false
    } else {
        true
    }
}

/// Keep an upgrade/root-rebuild source immutable until its atomic replacement is
/// installed. The watcher separately latches `index_uncertain` in memory and
/// schedules reconciliation, so the old snapshot cannot be reported as caught
/// up while its metadata remains untouched.
fn mark_index_dirty_before_reconciliation(store: &Store, read_only_source: bool) -> Result<()> {
    if read_only_source {
        Ok(())
    } else {
        mark_index_dirty(store, "daemon startup requires reconciliation")
    }
}

/// Run the watch daemon. Uses fanotify FID mode so creates, deletes, renames,
/// and size-growth are all tracked live — no rescan needed for normal activity.
#[allow(clippy::too_many_arguments)]
pub fn run_daemon(
    db: &Path,
    root: &Path,
    flush_ms: u64,
    one_file_system: bool,
    alert: Option<AlertConfig>,
    growth_days: i64,
    max_cpu_frac: f64,
    reconcile_first: bool,
) -> Result<()> {
    // How long to keep per-inode growth history. Shorter = smaller index on a
    // high-churn host. Clamp to >= 1 day so the growth/heat features still work.
    let growth_keep_secs = growth_days.clamp(1, 3650) * 86400;
    // Exclusive per-db lock for the daemon's whole lifetime — no second daemon or
    // concurrent scan can write this index (the heartbeat is only advisory). Held
    // until `_lock` drops when run_daemon returns.
    let _lock = crate::util::lock_db(db).context("acquiring daemon db lock")?;
    // Keep the unit's neutral OOM score. The daemon is memory-capped and
    // pressure-gates reconciliation; do not bias it as a global-OOM victim.
    // Background reader: request IDLE I/O class + low CPU nice to reduce
    // contention with other workloads. These are best-effort priorities; the CPU
    // governor below limits only the measured event-loop thread's average duty
    // cycle, not total process CPU or instantaneous bursts.
    set_background_priority();
    let root_canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

    // Register marks before rebuilding, including first installation and schema
    // upgrades. The old baseline is read-only until the background install;
    // events are collected continuously instead of leaving a startup scan gap.
    let rebuilt = Store::needs_rebuild(db)? || !root_matches(db, &root_canon)?;
    let mut read_only_source = rebuilt && Store::has_recoverable_database_state(db)?;
    let mut store = if read_only_source {
        Store::open_rebuild_source(db)
            .context("validating existing index before background upgrade")?
    } else {
        Store::open_rw(db)?
    };
    // Do not apply path events against a fresh/upgrade staging source that has
    // no installed root generation. The first successful atomic reconciliation
    // makes the committed root available; queued events are replayed afterward.
    let mut baseline_ready = !rebuilt && committed_root_indexed(&store)?;
    tracing::info!(
        rebuilt,
        read_only_source,
        schema_version = store.schema_version,
        baseline_ready,
        "selected startup index source"
    );
    if !rebuilt {
        store.conn.execute("DELETE FROM meta WHERE key IN ('paused_since','pause_reason','throttled_since','flush_error')", [])?;
    }

    // H1 — drift on resume: if we're RESUMING an existing index (not a fresh
    // rebuild), the daemon was not watching for some interval (crash, reboot, or
    // a clean stop/start), and fanotify only queues events while the mark is live
    // — so any change during that gap was never observed. Flag the index dirty so
    // status/TUI recommend a reconciling `dux scan` instead of silently trusting
    // possibly-drifted totals. A rescan (or the SIGHUP in-place rescan) clears it.
    let reconcile_on_start = true;
    if reconcile_on_start {
        let downtime = crate::util::read_heartbeat_for(db)
            .filter(|(_, _, hbdb)| {
                let want = db.canonicalize().unwrap_or_else(|_| db.to_path_buf());
                std::path::Path::new(hbdb) == want.as_path()
            })
            .map(|(secs, _, _)| now_secs().saturating_sub(secs).max(0));
        match downtime {
            _ if rebuilt => tracing::info!(
                "initial baseline or schema upgrade will run after filesystem watches are active"
            ),
            Some(gap) => tracing::warn!(
                "resuming after ~{gap}s not watching — changes during the gap were not tracked; \
                 marking index dirty; automatic background reconciliation will start"
            ),
            None => tracing::warn!(
                "resuming a pre-existing index this daemon didn't build — marking dirty; \
                 automatic background reconciliation will start"
            ),
        }
        mark_index_dirty_before_reconciliation(&store, read_only_source)?;
    }

    let mut initial_mounts = mount_topology(&root_canon, one_file_system)?;
    let mut last_mount_check = Instant::now();
    let max_queued_events = fanotify_max_queued_events();
    let (queue_high_watermark_bytes, queue_low_watermark_bytes) =
        fanotify_queue_watermarks(max_queued_events);
    tracing::info!(
        max_queued_events,
        queue_high_watermark_bytes,
        queue_low_watermark_bytes,
        reconcile_first,
        "fanotify queue-pressure CPU throttle thresholds"
    );
    let fan_guard = RawFdGuard(init_fanotify().context(
        "fanotify_init (need CAP_SYS_ADMIN and Linux 5.17+ or a vendor backport of FAN_REPORT_TARGET_FID)",
    )?);
    let fan = fan_guard.0;

    // A scan of `/` crosses mount points by default, so the daemon must watch
    // EVERY real filesystem under root (not just one) or live updates miss
    // /home, /var, /boot, etc. We mark each, keyed by fsid, and keep a dir fd
    // per filesystem for open_by_handle_at (the event carries its fsid).
    let mut fsfds: HashMap<(i32, i32), RawFd> = HashMap::new();
    // A stale target-only FAN_ATTRIB can be a normal delete race, but ESTALE
    // also occurs when a filesystem cannot reopen fanotify FIDs. Only treat it
    // as benign after another handle on that same filesystem has opened.
    let mut handle_capable_fsids: HashSet<(i32, i32)> = HashSet::new();
    let mut fsfd_guards = Vec::new();
    let mut mark_failures = 0u32;
    let mut mounted_fsids = HashSet::new();
    let mut ambiguous_hint_fsids = HashSet::new();
    for mp in real_mounts(&root_canon, one_file_system)? {
        let fsid = match statfs_fsid(&mp) {
            Ok(f) => f,
            Err(error) => {
                tracing::warn!(
                    path = %mp.display(),
                    error = %error,
                    "could not identify filesystem for fanotify coverage; its changes may be missed"
                );
                mark_failures = mark_failures.saturating_add(1);
                continue;
            }
        };
        if !mounted_fsids.insert(fsid) {
            ambiguous_hint_fsids.insert(fsid);
        }
        if let std::collections::hash_map::Entry::Vacant(e) = fsfds.entry(fsid) {
            match open_dir(&mp) {
                Ok(fd) => {
                    let fd_guard = RawFdGuard(fd);
                    match mark_fs(fan, &mp) {
                        Ok(()) => {
                            if probe_handle_capability(&mp, fd) {
                                // Establish this before draining the initial event
                                // queue. A deleted target may produce ESTALE before
                                // the first ordinary path event proves the FSID can
                                // reopen handles; that order must not poison every
                                // startup reconciliation on a capable filesystem.
                                handle_capable_fsids.insert(fsid);
                                tracing::debug!(
                                    path = %mp.display(),
                                    fsid = ?fsid,
                                    "filesystem FID handle round-trip probe succeeded"
                                );
                            } else {
                                tracing::debug!(
                                    path = %mp.display(),
                                    fsid = ?fsid,
                                    "filesystem FID handle round-trip probe unavailable; stale target events remain conservative"
                                );
                            }
                            e.insert(fd);
                            fsfd_guards.push(fd_guard);
                        }
                        Err(error) => {
                            // mark failed: this filesystem won't be watched. Don't
                            // pretend full coverage — count it and mark degraded.
                            tracing::warn!(
                                path = %mp.display(),
                                error = %error,
                                "could not fanotify-mark filesystem; its changes may be missed"
                            );
                            mark_failures = mark_failures.saturating_add(1);
                        }
                    }
                }
                Err(_) => {
                    tracing::warn!(
                        "could not open {} — its changes will be missed",
                        mp.display()
                    );
                    mark_failures += 1;
                }
            }
        }
    }
    if fsfds.is_empty() {
        anyhow::bail!(
            "no watchable filesystem found under {}",
            root_canon.display()
        );
    }
    let coverage_incomplete = mark_failures > 0;
    if coverage_incomplete && !read_only_source {
        // partial coverage is a known-incomplete watch — surface it via dirty
        // state so status/TUI stop claiming the index is trustworthy.
        mark_index_dirty(&store, "watch coverage is incomplete")?;
    }
    tracing::info!(
        "dux daemon watching {} ({} filesystem(s), FID mode: create/delete/rename/modify, flush {}ms)",
        root.display(),
        fsfds.len(),
        flush_ms
    );
    if reconcile_first {
        tracing::warn!(
            scan_interval_seconds = SCAN_FIRST_INTERVAL.as_secs(),
            in_place_full_root_reconciliation = !one_file_system,
            "experimental reconciliation-first mode: fanotify events are hints only; complete full-root observations merge in place after the initial baseline"
        );
    }
    let mut hint_reader = if reconcile_first {
        // Full-root walks do not need resolved event paths. Keep a dedicated
        // reader to drain fanotify promptly and count events/overflows, but
        // do not send raw chunks through bounded queues that cannot affect
        // the scheduled walk's coverage or completeness.
        Some(FanotifyHintReader::start(fan, true)?)
    } else {
        None
    };
    let mut hint_resolver = if let Some(reader) = hint_reader.as_ref() {
        (!reader.discard_chunks)
            .then(|| {
                FanotifyHintResolver::start(
                    fsfds.clone(),
                    handle_capable_fsids.clone(),
                    ambiguous_hint_fsids.clone(),
                    root_canon.clone(),
                    reader.notify.0,
                    Arc::clone(&reader.queued_chunks),
                )
            })
            .transpose()?
    } else {
        None
    };
    if let Some(a) = &alert {
        tracing::info!(
            "alerts: >{} growth in {}s{}",
            crate::util::human(a.threshold),
            a.window,
            a.exec
                .as_deref()
                .map(|e| format!(", exec: {e}"))
                .unwrap_or_default()
        );
    }

    // Flush pending events on SIGTERM/SIGINT instead of dropping up to a full
    // flush window on shutdown (systemd stops the daemon with SIGTERM).
    unsafe {
        let h = on_term as *const () as libc::sighandler_t;
        libc::signal(libc::SIGTERM, h);
        libc::signal(libc::SIGINT, h);
        // SIGHUP = "rescan now" (sent by `dux scan` when the daemon is live).
        libc::signal(libc::SIGHUP, on_hup as *const () as libc::sighandler_t);
    }

    crate::util::write_heartbeat(db);
    crate::util::systemd_notify("READY=1\nSTATUS=Watching filesystem; initializing index state");
    let flush_every = Duration::from_millis(flush_ms);
    let mut last_flush = Instant::now();
    let mut draining_backlog = false;
    let mut last_ckpt = Instant::now();
    let mut wal_write_guarded = false;
    let mut last_integrity = Instant::now();
    let mut last_reconcile = Instant::now() - Duration::from_secs(300);
    let mut rescan_retry_after: Option<Instant> = None;
    let (mut failed_reconciliation_attempts, retry_state_corrupt) =
        parse_reconciliation_attempts(store.get_meta("failed_reconciliation_attempts")?.as_deref());
    let mut reconciliation_retry_paused =
        reconciliation_retry_should_pause(reconcile_first, failed_reconciliation_attempts)
            || (retry_state_corrupt && !reconcile_first);
    let persisted_retry_deadline = store.get_meta(RECONCILIATION_RETRY_AFTER_META)?;
    if failed_reconciliation_attempts > 0
        || retry_state_corrupt
        || persisted_retry_deadline.is_some()
    {
        // Restore the remaining wall-clock deadline, bounded to this attempt's
        // maximum delay, then use a monotonic timer for the live wait.
        rescan_retry_after = Some(restore_reconciliation_retry_deadline(
            &store,
            failed_reconciliation_attempts,
            !read_only_source,
        )?);
    }
    if retry_state_corrupt {
        if !read_only_source {
            mark_index_dirty(
                &store,
                "reconciliation retry state is corrupt; automatic recovery uses maximum cooldown",
            )?;
        }
        tracing::error!(
            "persisted reconciliation retry count is malformed; automatic recovery remains enabled with maximum cooldown"
        );
        crate::util::systemd_notify(
            "READY=1\nSTATUS=Retry state corrupt; automatic recovery uses maximum cooldown",
        );
    }
    let mut last_alert: HashMap<(i64, i64), i64> = HashMap::new();
    let mut alert_children: Vec<std::process::Child> = Vec::new();
    let mut pending: HashMap<PathBuf, Op> = HashMap::new();
    // Target-only FID events can repeat many times for a hot inode. Schedule its
    // indexed links once per pending flush window rather than querying SQLite
    // for the same aliases on every write notification.
    let mut target_refreshes: std::collections::HashSet<(i64, i64)> =
        std::collections::HashSet::new();
    let mut indexed_parent_paths = IndexedParentPathCache::default();
    // CPU governor — limits the measured event-loop thread's average CPU duty
    // cycle while queue pressure is low. Backlog suspends duty-cycle repayment;
    // the service cgroup remains the process-wide resource ceiling.
    let mut gov = CpuGovernor::new(max_cpu_frac, max_queued_events);
    // Paths CREATEd since the last flush — lets a same-window delete cancel a
    // transient file before it does any index work (see parse_events). Cleared
    // whenever `pending` is cleared, so it only ever spans one flush window.
    let mut born: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    // Unmatched FAN_MOVED_FROMs awaiting their MOVED_TO (rename across a flush).
    let mut deferred_from: HashMap<(i64, i64), DeferredFrom> = HashMap::new();
    let mut buf = [0u8; 1 << 15];
    // A bounded parse may leave complete records in this buffer. Retain and
    // process that suffix before reading newer kernel events so ordering holds.
    let mut buf_len = 0usize;
    // filesystem holding the index; checked each flush for low-disk protection.
    let watch_dir = db
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/"));
    let mut consecutive_flush_errors = 0u32;
    let mut pressure_gate = crate::guard::PressureGate::default();
    let mut reconciliation_deferred = false;
    let mut writes_paused = false; // transient low-disk pause (self-clearing)
    let (mut ev_seen, mut ev_resolved) = (0u64, 0u64); // capability self-check (C3)
    let (mut updates_committed, mut updates_dropped) = (0u64, 0u64);
    let mut kernel_overflow_notifications = 0u64;
    let mut sampled_hint_events = 0u64;
    let mut sampled_hint_overflows = 0u64;
    let mut last_commit_at = 0i64;
    let mut resolve_warned = false;
    // "Throttled/behind" tracking for the UI. We're "caught up" only when BOTH the
    // kernel fanotify queue is drained AND the indexing backlog (`pending`) is
    // empty. When the governor can't keep up under a storm, one of those stays
    // backed up, so `last_caught_up.elapsed()` = how far behind (hence how stale)
    // the index is. Surfaced to status/TUI via a meta key so the user understands
    // WHY data is stale: dux is deliberately yielding to the host instead of
    // spiking CPU/IO. Cleared once we catch up.
    let mut last_caught_up = Instant::now();
    let mut was_caught_up = false;
    let mut kernel_empty = true; // did the last drain reach EAGAIN (queue empty)?
    let mut throttled_since: Option<i64> = None;
    let mut scan_job: Option<std::thread::JoinHandle<Result<crate::scan::ScanStats>>> = None;
    let mut accounting_check: Option<AccountingCheck> = None;
    let mut pending_reconciliation_check: Option<bool> = None;
    let mut scan_cancel: Option<Arc<AtomicBool>> = None;
    // Keep only a bounded set of resolved directory hints for the next full
    // reconciliation. New hints arriving during a scan are retained for the
    // following pass rather than changing the active traversal.
    let mut pending_scan_hints = Vec::<PathBuf>::new();
    let mut lost_events = false;
    // Keep an operator request latched until a scan actually starts. If SIGHUP
    // arrives during an active scan, that scan may be invalidated by concurrent
    // changes; a normal race must not discard the requested follow-up.
    let mut manual_rescan_pending = false;
    // Distinguish normal changes observed during a walk from known hint loss.
    // A normal race may wait for cadence; an overflow or lost scope needs a
    // follow-up observation immediately after the current walk completes.
    let mut forced_reconciliation_after_scan = false;
    // Latched independently from SQLite: a full/read-only database must not
    // hide event loss merely because its dirty metadata cannot be updated.
    let mut index_uncertain =
        reconcile_on_start || coverage_incomplete || store.get_meta("dirty_since")?.is_some();
    if reconcile_on_start && !reconciliation_retry_paused {
        // The fanotify marks are live before the rebuild begins. Events arriving
        // during the atomic scan remain in the kernel queue and are drained onto
        // the new index afterward, closing the restart gap without downtime.
        RESCAN.store(true, Ordering::SeqCst);
    } else if reconcile_on_start {
        tracing::error!(
            failed_attempts = failed_reconciliation_attempts,
            "automatic reconciliation is cooling down after repeated failures; it will retry automatically"
        );
        // Keep the request pending while the persisted attempt count is in its
        // cooldown. The request gate below admits it when the deadline elapses.
        RESCAN.store(true, Ordering::SeqCst);
        crate::util::systemd_notify(
            "READY=1\nSTATUS=Accounting recovery cooling down; automatic retry scheduled",
        );
    }
    crate::util::write_watch_status(db, 0, true, 0, 0, 0, 0, 0, 0, MAX_PENDING, index_uncertain);

    loop {
        if let Some(reader) = hint_reader.as_ref() {
            let observed = reader.observed_events.load(Ordering::Relaxed);
            if observed > sampled_hint_events {
                ev_seen = ev_seen.saturating_add(observed - sampled_hint_events);
                sampled_hint_events = observed;
                if !index_uncertain {
                    persist_dirty_marker_best_effort(
                        &store,
                        "filesystem changes await the next full observation",
                    );
                }
                index_uncertain = true;
                if scan_job.is_some() {
                    lost_events = true;
                }
            }
            let overflows = reader.kernel_overflows.load(Ordering::Relaxed);
            if overflows > sampled_hint_overflows {
                kernel_overflow_notifications = kernel_overflow_notifications
                    .saturating_add(overflows - sampled_hint_overflows);
                sampled_hint_overflows = overflows;
                if !index_uncertain {
                    persist_dirty_marker_best_effort(
                        &store,
                        "fanotify overflow; next full observation will reconcile state",
                    );
                }
                index_uncertain = true;
                if scan_job.is_some() {
                    lost_events = true;
                }
            }
        }
        if let (Some(reader), Some(resolver)) = (hint_reader.as_ref(), hint_resolver.as_ref()) {
            while let Ok(result) = resolver.rx.try_recv() {
                reader.queued_chunks.fetch_sub(1, Ordering::AcqRel);
                ev_seen = ev_seen.saturating_add(result.seen);
                for path in result.priority_paths {
                    if pending_scan_hints.len() < 256 && !pending_scan_hints.contains(&path) {
                        pending_scan_hints.push(path);
                    }
                }
                let observation_invalidated = scan_first_hint_invalidates_observation(
                    scan_job.is_some(),
                    result.seen > 0,
                    result.lost,
                );
                if observation_invalidated {
                    if !index_uncertain {
                        persist_dirty_marker_best_effort(
                            &store,
                            "filesystem changes await a reconciliation scan",
                        );
                    }
                    index_uncertain = true;
                    if scan_job.is_some() {
                        lost_events = true;
                    }
                    if let Some(check) = &accounting_check {
                        check.cancel.store(true, Ordering::Relaxed);
                        pending_reconciliation_check = None;
                    }
                }
                if result.lost {
                    // An unresolvable parent widens the next repair to a full
                    // observation, but it is not proof of kernel/user-ring loss.
                    // Keep the index stale and let the fixed cadence repair it;
                    // immediately rescanning an active, noisy filesystem can
                    // otherwise form an unbounded scan loop.
                    if observation_invalidated {
                        tracing::debug!(
                            "fanotify parent scope could not be resolved during observation; full observation remains scheduled"
                        );
                    } else {
                        tracing::debug!(
                            "fanotify parent scope could not be resolved after observation; last-observed view remains available until the next scan"
                        );
                    }
                }
            }
            if resolver.dropped.swap(false, Ordering::AcqRel) {
                lost_events = true;
                forced_reconciliation_after_scan = true;
                index_uncertain = true;
                persist_dirty_marker_best_effort(
                    &store,
                    "fanotify hint resolver capacity was exceeded",
                );
                if hint_loss_requests_immediate_scan(reconcile_first) {
                    RESCAN.store(true, Ordering::SeqCst);
                }
                tracing::warn!(
                    "fanotify hint resolver backlog overflowed; discarded scopes leave the index stale until its next observation"
                );
            }
        }
        if accounting_check
            .as_ref()
            .is_some_and(|check| check.join.is_finished())
        {
            let check = accounting_check.take().unwrap();
            let elapsed = check.started.elapsed();
            let kind = check.kind;
            let result = check
                .join
                .join()
                .unwrap_or_else(|_| Err(anyhow::anyhow!("accounting-check worker panicked")));
            match result {
                Ok(None) => {
                    tracing::debug!(
                        elapsed_ms = elapsed.as_millis(),
                        "accounting verification cancelled to service watcher backlog"
                    );
                    last_integrity = Instant::now() - Duration::from_secs(300);
                }
                Ok(Some(true)) => {
                    failed_reconciliation_attempts = 0;
                    reconciliation_retry_paused = false;
                    rescan_retry_after = None;
                    store.conn.execute(
                        "DELETE FROM meta WHERE key IN ('failed_reconciliation_attempts', 'reconciliation_retry_after_unix')",
                        [],
                    )?;
                    match kind {
                        AccountingCheckKind::Periodic => tracing::debug!(
                            elapsed_ms = elapsed.as_millis(),
                            "periodic accounting verification passed"
                        ),
                        AccountingCheckKind::Reconciliation { safe_to_clear } => {
                            pending_reconciliation_check = None;
                            let clean = safe_to_clear
                                && !coverage_incomplete
                                && !resolve_warned
                                && !lost_events
                                && !RESCAN.load(Ordering::SeqCst);
                            if clean {
                                let reason = store.get_meta("dirty_reason")?;
                                if reason.as_deref() == Some(RECONCILIATION_ACCOUNTING_PENDING) {
                                    store.conn.execute(
                                    "DELETE FROM meta WHERE key IN ('dirty_since','dirty_reason')",
                                    [],
                                )?;
                                    index_uncertain = false;
                                    tracing::info!(
                                        elapsed_ms = elapsed.as_millis(),
                                        "reconciliation accounting verification passed"
                                    );
                                } else {
                                    index_uncertain = true;
                                    tracing::warn!(
                                        elapsed_ms = elapsed.as_millis(),
                                        "reconciliation verified but another dirty reason remains"
                                    );
                                }
                            } else {
                                index_uncertain = true;
                                tracing::warn!(
                                elapsed_ms = elapsed.as_millis(),
                                "reconciliation accounting passed, but coverage or events remain uncertain"
                            );
                            }
                        }
                    }
                }
                Ok(Some(false)) => {
                    pending_reconciliation_check = None;
                    index_uncertain = true;
                    store.set_meta("dirty_since", &now_secs().to_string())?;
                    if matches!(kind, AccountingCheckKind::Reconciliation { .. }) {
                        failed_reconciliation_attempts =
                            failed_reconciliation_attempts.saturating_add(1);
                        store.set_meta(
                            "failed_reconciliation_attempts",
                            &failed_reconciliation_attempts.to_string(),
                        )?;
                    }
                    if reconciliation_retry_should_pause(
                        reconcile_first,
                        failed_reconciliation_attempts,
                    ) {
                        reconciliation_retry_paused = true;
                        rescan_retry_after = Some(persist_reconciliation_retry_deadline(
                            &store,
                            failed_reconciliation_attempts,
                        )?);
                        RESCAN.store(true, Ordering::SeqCst);
                        store.set_meta(
                            "dirty_reason",
                            &format!(
                            "directory accounting invariant failed; automatic reconciliation paused after {failed_reconciliation_attempts} failed full scans"
                            ),
                        )?;
                        tracing::error!(
                            elapsed_ms = elapsed.as_millis(),
                            failed_attempts = failed_reconciliation_attempts,
                            "accounting verification still fails after a full reconciliation; automatic scans paused to protect the host"
                        );
                        crate::util::systemd_notify(
                            "READY=1\nSTATUS=Accounting verification failed repeatedly; automatic scans paused",
                        );
                    } else {
                        store.set_meta(
                            "dirty_reason",
                            "directory accounting invariant failed; one bounded automatic reconciliation retry remains",
                        )?;
                        rescan_retry_after = Some(persist_reconciliation_retry_deadline(
                            &store,
                            failed_reconciliation_attempts,
                        )?);
                        RESCAN.store(true, Ordering::SeqCst);
                        tracing::error!(
                            elapsed_ms = elapsed.as_millis(),
                            failed_attempts = failed_reconciliation_attempts,
                            retry_after_seconds = rescan_retry_after
                                .map(|retry| retry.saturating_duration_since(Instant::now()).as_secs())
                                .unwrap_or(0),
                            "materialized index totals disagree with inode rows; bounded reconciliation retry scheduled"
                        );
                        RESCAN.store(true, Ordering::SeqCst);
                    }
                }
                Err(e) => {
                    pending_reconciliation_check = None;
                    index_uncertain = true;
                    store.set_meta("dirty_since", &now_secs().to_string())?;
                    if matches!(kind, AccountingCheckKind::Reconciliation { .. }) {
                        failed_reconciliation_attempts =
                            failed_reconciliation_attempts.saturating_add(1);
                        store.set_meta(
                            "failed_reconciliation_attempts",
                            &failed_reconciliation_attempts.to_string(),
                        )?;
                    }
                    if reconciliation_retry_should_pause(
                        reconcile_first,
                        failed_reconciliation_attempts,
                    ) {
                        reconciliation_retry_paused = true;
                        rescan_retry_after = Some(persist_reconciliation_retry_deadline(
                            &store,
                            failed_reconciliation_attempts,
                        )?);
                        RESCAN.store(true, Ordering::SeqCst);
                        store.set_meta(
                            "dirty_reason",
                            &format!(
                                "directory accounting verification failed; automatic reconciliation paused after {failed_reconciliation_attempts} failed full scans"
                            ),
                        )?;
                        tracing::error!(
                            elapsed_ms = elapsed.as_millis(),
                            failed_attempts = failed_reconciliation_attempts,
                            error = %e,
                            "accounting verification repeatedly failed; automatic scans paused to protect the host"
                        );
                        crate::util::systemd_notify(
                            "READY=1\nSTATUS=Accounting verification repeatedly failed; automatic scans paused",
                        );
                    } else {
                        store.set_meta(
                            "dirty_reason",
                            "directory accounting verification could not complete",
                        )?;
                        rescan_retry_after = Some(persist_reconciliation_retry_deadline(
                            &store,
                            failed_reconciliation_attempts,
                        )?);
                        tracing::warn!(
                            elapsed_ms = elapsed.as_millis(),
                            error = %e,
                            "accounting verification failed; bounded reconciliation retry scheduled"
                        );
                        RESCAN.store(true, Ordering::SeqCst);
                    }
                }
            }
        }
        if SHUTDOWN.load(Ordering::SeqCst) {
            crate::util::systemd_notify("STOPPING=1\nSTATUS=Draining filesystem events");
            let shutdown_deadline = Instant::now() + Duration::from_secs(10);
            if reconcile_first {
                if let Some(reader) = hint_reader.as_mut() {
                    reader.stop_and_join();
                }
                if let Some(resolver) = hint_resolver.as_mut() {
                    resolver.stop_and_join();
                }
                // Events may remain in the bounded ring or kernel queue when
                // the reader stops. Startup always performs a complete
                // observation, so persist that recovery requirement instead of
                // racing the reader with a second consumer during shutdown.
                if let Err(error) = mark_index_dirty(
                    &store,
                    "shutdown requires a fresh reconciliation observation",
                ) {
                    tracing::error!(%error, "could not persist shutdown observation requirement");
                }
            } else {
                // Final best-effort DRAIN of the kernel queue first, so events that
                // landed in the brief window before SIGTERM (a file created right as
                // `systemctl stop` ran) aren't lost — then flush everything once.
                while buf_len > 0 && Instant::now() < shutdown_deadline {
                    let parse_deadline =
                        (Instant::now() + EVENT_PARSE_SLICE).min(shutdown_deadline);
                    let (_, _, overflows, consumed) = parse_events_bounded(
                        &buf[..buf_len],
                        &fsfds,
                        &mut handle_capable_fsids,
                        &root_canon,
                        db,
                        &mut pending,
                        &mut born,
                        &mut target_refreshes,
                        &mut indexed_parent_paths,
                        &mut store,
                        scan_job.is_none(),
                        &mut lost_events,
                        MAX_EVENTS_PER_PARSE,
                        parse_deadline,
                    );
                    if consumed == 0 {
                        break;
                    }
                    buf.copy_within(consumed..buf_len, 0);
                    buf_len -= consumed;
                    kernel_overflow_notifications =
                        kernel_overflow_notifications.saturating_add(overflows);
                }
                if buf_len > 0 {
                    // Do not silently discard a buffered suffix when the shutdown
                    // drain deadline expires; the final index must remain uncertain.
                    lost_events = true;
                    persist_dirty_marker_best_effort(
                        &store,
                        "shutdown left buffered fanotify records unapplied",
                    );
                    tracing::warn!(
                        buffered_bytes = buf_len,
                        "shutdown deadline left buffered fanotify records unapplied"
                    );
                    buf_len = 0;
                }
                let mut shutdown_reads = 0u32;
                let mut shutdown_queue_drained = false;
                loop {
                    if buf_len == 0 {
                        if shutdown_reads >= MAX_READS_PER_TICK
                            || Instant::now() >= shutdown_deadline
                        {
                            break;
                        }
                        let m = unsafe {
                            libc::read(fan, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
                        };
                        if m > 0 {
                            buf_len = m as usize;
                            shutdown_reads += 1;
                        } else if m < 0 {
                            match std::io::Error::last_os_error().raw_os_error() {
                                Some(libc::EINTR) => continue,
                                Some(libc::EAGAIN) => {
                                    shutdown_queue_drained = true;
                                    break;
                                }
                                _ => {
                                    lost_events = true;
                                    tracing::warn!(
                                        error = %std::io::Error::last_os_error(),
                                        "fanotify read failed during shutdown drain"
                                    );
                                    break;
                                }
                            }
                        } else {
                            shutdown_queue_drained = true;
                            break;
                        }
                    }

                    let parse_deadline =
                        (Instant::now() + EVENT_PARSE_SLICE).min(shutdown_deadline);
                    let (_, _, overflows, consumed) = parse_events_bounded(
                        &buf[..buf_len],
                        &fsfds,
                        &mut handle_capable_fsids,
                        &root_canon,
                        db,
                        &mut pending,
                        &mut born,
                        &mut target_refreshes,
                        &mut indexed_parent_paths,
                        &mut store,
                        scan_job.is_none(),
                        &mut lost_events,
                        MAX_EVENTS_PER_PARSE,
                        parse_deadline,
                    );
                    if consumed == 0 {
                        break;
                    }
                    buf.copy_within(consumed..buf_len, 0);
                    buf_len -= consumed;
                    kernel_overflow_notifications =
                        kernel_overflow_notifications.saturating_add(overflows);
                    // A buffered suffix is always processed before another read so
                    // event ordering remains intact across shutdown slices.
                    if buf_len > 0 && Instant::now() >= shutdown_deadline {
                        break;
                    }
                }
                if shutdown_drain_incomplete(shutdown_queue_drained, buf_len) {
                    // Hitting the read/time cap is not evidence that the kernel
                    // queue is empty. Restart reconciliation must close this gap.
                    lost_events = true;
                    if buf_len > 0 {
                        tracing::warn!(
                            buffered_bytes = buf_len,
                            "shutdown deadline left buffered fanotify records unapplied"
                        );
                    }
                    if !shutdown_queue_drained {
                        tracing::warn!(
                            reads = shutdown_reads,
                            "shutdown drain ended without observing an empty fanotify queue"
                        );
                    }
                }
                if lost_events {
                    // The daemon exits immediately after this path, so persist any
                    // shutdown-time loss before its in-memory uncertainty latch goes
                    // away. Startup reconciliation can then repair the gap.
                    if let Err(error) =
                        mark_index_dirty(&store, "shutdown could not prove event continuity")
                    {
                        tracing::error!(%error, "could not persist shutdown event-loss marker");
                    }
                }
            }
            let shutdown_work_pending =
                !pending.is_empty() || !deferred_from.is_empty() || !target_refreshes.is_empty();
            if wal_write_guarded && shutdown_work_pending {
                // The shutdown drain below must not bypass the pinned-reader
                // WAL guard. Persist recovery state before exiting because this
                // in-memory backlog will otherwise disappear with the process.
                if let Err(error) = mark_index_dirty(
                    &store,
                    "shutdown left event work unapplied because the WAL checkpoint is pinned",
                ) {
                    tracing::error!(%error, "could not persist shutdown recovery marker while WAL guarded");
                }
                tracing::warn!(
                    pending_updates = pending.len(),
                    deferred_renames = deferred_from.len(),
                    target_refreshes = target_refreshes.len(),
                    "WAL guard prevented shutdown event flush; startup reconciliation must recover queued work"
                );
            }
            while shutdown_event_flush_allowed(
                baseline_ready,
                scan_job.is_some(),
                wal_write_guarded,
                !pending.is_empty() || !deferred_from.is_empty() || !target_refreshes.is_empty(),
            ) && Instant::now() < shutdown_deadline
            {
                if let Err(error) = materialize_target_refreshes(
                    &mut target_refreshes,
                    &mut pending,
                    deferred_from.len(),
                    &store,
                    &root_canon,
                    &mut indexed_parent_paths,
                ) {
                    let marker = mark_index_dirty(&store, "target refresh failed during shutdown");
                    tracing::warn!(error = %error, marker_error = ?marker.err(), "target refresh failed during shutdown; startup reconciliation will repair");
                }
                for from in deferred_from.values_mut() {
                    from.expires = Instant::now();
                }
                if let Err(e) = flush_batch(
                    &mut store,
                    &mut pending,
                    &mut born,
                    &mut deferred_from,
                    db,
                    growth_keep_secs,
                    FLUSH_BATCH,
                    true,
                ) {
                    tracing::warn!("final flush failed (startup reconciliation will repair): {e}");
                    break;
                }
            }
            if !baseline_ready
                && (!pending.is_empty()
                    || !deferred_from.is_empty()
                    || !target_refreshes.is_empty())
            {
                tracing::warn!(
                    pending_updates = pending.len(),
                    deferred_renames = deferred_from.len(),
                    target_refreshes = target_refreshes.len(),
                    "stopping before a committed baseline was installed; startup reconciliation must recover queued events"
                );
            }
            if baseline_ready
                && (!pending.is_empty()
                    || !deferred_from.is_empty()
                    || !target_refreshes.is_empty())
                && !wal_write_guarded
            {
                if let Err(error) =
                    mark_index_dirty(&store, "shutdown deadline left event work unapplied")
                {
                    tracing::error!(%error, "could not persist shutdown recovery marker for remaining event work");
                }
                tracing::warn!(
                    pending_updates = pending.len(),
                    deferred_renames = deferred_from.len(),
                    target_refreshes = target_refreshes.len(),
                    "shutdown deadline left queued event work unapplied; startup reconciliation must recover it"
                );
            }
            tracing::info!("dux daemon: received SIGTERM/SIGINT — drained + flushed, exiting");
            let _ = std::fs::remove_file(crate::util::runtime_path(db, "watch.status"));
            return Ok(());
        }
        if MANUAL_RESCAN.swap(false, Ordering::SeqCst) {
            manual_rescan_pending = true;
            failed_reconciliation_attempts = 0;
            reconciliation_retry_paused = false;
            rescan_retry_after = None;
            store.conn.execute(
                "DELETE FROM meta WHERE key IN ('failed_reconciliation_attempts', 'reconciliation_retry_after_unix')",
                [],
            )?;
            RESCAN.store(true, Ordering::SeqCst);
            tracing::warn!(
                "operator requested a full reconciliation; resetting the accounting retry circuit"
            );
        }
        if scan_job.as_ref().is_some_and(|job| job.is_finished()) {
            let outcome = scan_job
                .take()
                .unwrap()
                .join()
                .unwrap_or_else(|_| Err(anyhow::anyhow!("scan worker panicked")));
            let cancelled_for_pressure = scan_cancel
                .take()
                .is_some_and(|flag| flag.load(Ordering::Relaxed));
            let legacy_source_retained = outcome.is_err() && Store::needs_rebuild(db)?;
            // If an upgrade scan fails before its atomic install, the old schema
            // is still the only committed index. Keep its read-only connection
            // open and retry; opening it as the current writable schema here
            // would turn an ordinary scan failure into a daemon exit/restart loop.
            if legacy_source_retained {
                let error = outcome
                    .as_ref()
                    .err()
                    .expect("legacy source retention requires a failed scan");
                index_uncertain = true;
                if cancelled_for_pressure {
                    rescan_retry_after = Some(Instant::now() + RECONCILE_RETRY_BACKOFF);
                    RESCAN.store(true, Ordering::SeqCst);
                    tracing::warn!(
                        error = %error,
                        "upgrade reconciliation was cancelled; legacy index retained and retry pending"
                    );
                    crate::util::systemd_notify(
                        "READY=1\nSTATUS=Legacy index retained; waiting to retry reconciliation",
                    );
                } else {
                    failed_reconciliation_attempts =
                        failed_reconciliation_attempts.saturating_add(1);
                    if reconciliation_retry_should_pause(
                        reconcile_first,
                        failed_reconciliation_attempts,
                    ) {
                        reconciliation_retry_paused = true;
                        rescan_retry_after = Some(
                            Instant::now()
                                + reconciliation_retry_delay(failed_reconciliation_attempts),
                        );
                        tracing::error!(
                            failed_attempts = failed_reconciliation_attempts,
                            error = %error,
                            "upgrade reconciliation failed; legacy index retained and automatic retries paused"
                        );
                        crate::util::systemd_notify(
                            "READY=1\nSTATUS=Legacy index retained; automatic reconciliation paused after repeated failures",
                        );
                    } else {
                        rescan_retry_after = Some(
                            Instant::now()
                                + reconciliation_retry_delay(failed_reconciliation_attempts),
                        );
                        RESCAN.store(true, Ordering::SeqCst);
                        tracing::error!(
                            failed_attempts = failed_reconciliation_attempts,
                            error = %error,
                            retry_after_seconds = rescan_retry_after
                                .map(|retry| retry.saturating_duration_since(Instant::now()).as_secs())
                                .unwrap_or(0),
                            "upgrade reconciliation failed; legacy index retained and retry scheduled"
                        );
                        crate::util::systemd_notify(
                            "READY=1\nSTATUS=Legacy index retained; bounded reconciliation retry pending",
                        );
                    }
                }
            } else {
                // The scan atomically replaced the indexed graph; discard paths
                // resolved against the previous generation and reopen it writable.
                store = Store::open_rw(db).context("reopening index after reconciliation")?;
                read_only_source = false;
                indexed_parent_paths.clear();
                match outcome {
                    Ok(stats) => {
                        baseline_ready = committed_root_indexed(&store)?;
                        // Retain events collected while the walk was in flight. They
                        // reconcile CURRENT path state against the new baseline.
                        deferred_from.clear();
                        born.clear();
                        writes_paused = false;
                        pressure_gate = crate::guard::PressureGate::default();
                        throttled_since = None;
                        // Verify installed rollups asynchronously. Keep the status
                        // dirty until both the exact SQL predicate and scan coverage
                        // are proven; the watcher loop remains free to ingest and
                        // commit events while the read-only snapshot runs.
                        // In scan-first mode, event loss does not invalidate a
                        // completed last-observed walk. It means the view may
                        // already be older than the walk, which freshness
                        // reports honestly; replaying hints is not a correctness
                        // gate. A coverage/accounting failure still is.
                        let safe_to_clear = !coverage_incomplete
                            && stats.errors == 0
                            && (reconcile_first || (!resolve_warned && !lost_events));
                        let event_race_only = scan_first_event_race_only(
                            reconcile_first,
                            lost_events,
                            forced_reconciliation_after_scan,
                            coverage_incomplete,
                            resolve_warned,
                            stats.errors,
                        );
                        index_uncertain = true;
                        pending_reconciliation_check = safe_to_clear.then_some(true);
                        store.set_meta("dirty_since", &now_secs().to_string())?;
                        store.set_meta(
                            "dirty_reason",
                            if safe_to_clear {
                                RECONCILIATION_ACCOUNTING_PENDING
                            } else if event_race_only {
                                "filesystem or fanotify hints changed during observation; next periodic scan scheduled"
                            } else {
                                "incomplete coverage or changes lost during reconciliation"
                            },
                        )?;
                        if event_race_only {
                            // A complete walk remains the source of truth even
                            // when its lossy hint stream overflows during that
                            // walk. Do not immediately repeat a full scan: a
                            // sustained event stream can overflow every pass
                            // and otherwise create an unbounded scan loop. Keep
                            // the next observation on the fixed cadence, while
                            // the freshness timestamp and scan-first status
                            // make the last-observed view explicit.
                            // Keep the cadence anchored to observation start.
                            // Resetting here would add the full scan duration to
                            // the configured maximum interval on every raced scan.
                            if scan_first_preserve_manual_followup(
                                event_race_only,
                                manual_rescan_pending,
                            ) {
                                RESCAN.store(true, Ordering::SeqCst);
                                tracing::info!(
                                    "explicit reconciliation request raced with the active observation; preserving its follow-up"
                                );
                            } else {
                                RESCAN.store(false, Ordering::SeqCst);
                            }
                            lost_events = false;
                            forced_reconciliation_after_scan = false;
                            if !manual_rescan_pending {
                                tracing::info!(
                                    "filesystem or fanotify hints changed during scan-first observation; next full observation is scheduled"
                                );
                            }
                        } else if !safe_to_clear {
                            failed_reconciliation_attempts =
                                failed_reconciliation_attempts.saturating_add(1);
                            store.set_meta(
                                "failed_reconciliation_attempts",
                                &failed_reconciliation_attempts.to_string(),
                            )?;
                            if reconciliation_retry_should_pause(
                                reconcile_first,
                                failed_reconciliation_attempts,
                            ) {
                                reconciliation_retry_paused = true;
                                rescan_retry_after = Some(persist_reconciliation_retry_deadline(
                                    &store,
                                    failed_reconciliation_attempts,
                                )?);
                                RESCAN.store(true, Ordering::SeqCst);
                                store.set_meta(
                                "dirty_reason",
                                &format!(
                                    "reconciliation did not produce complete coverage; automatic retries paused after {failed_reconciliation_attempts} failed full scans"
                                ),
                            )?;
                                tracing::error!(
                                failed_attempts = failed_reconciliation_attempts,
                                scan_errors = stats.errors,
                                coverage_incomplete,
                                resolve_warned,
                                lost_events,
                                "reconciliation remained incomplete; automatic scans paused to protect the host"
                            );
                                crate::util::systemd_notify(
                                "READY=1\nSTATUS=Reconciliation incomplete repeatedly; automatic scans paused",
                            );
                            } else {
                                rescan_retry_after = Some(persist_reconciliation_retry_deadline(
                                    &store,
                                    failed_reconciliation_attempts,
                                )?);
                                RESCAN.store(true, Ordering::SeqCst);
                                tracing::warn!(
                                failed_attempts = failed_reconciliation_attempts,
                                scan_errors = stats.errors,
                                coverage_incomplete,
                                resolve_warned,
                                lost_events,
                                retry_after_seconds = rescan_retry_after
                                    .map(|retry| retry
                                        .saturating_duration_since(Instant::now())
                                        .as_secs())
                                    .unwrap_or(0),
                                "reconciliation incomplete; one bounded automatic retry scheduled"
                            );
                            }
                        }
                        tracing::info!(vanished_reuse_paths = stats.vanished_reuse_paths, revalidated_reuse_paths = stats.revalidated_reuse_paths, unresolved_reuse_paths = stats.unresolved_reuse_paths, "rescan complete: {} files, {} dirs, {} staged path instances ({} directory alias paths), visible {} bytes/{} entries, unique {} bytes/{} inodes, {} ({} coverage issues); replaying {} pending paths",
                        stats.files, stats.dirs, stats.staged_path_instances, stats.staged_directory_alias_paths,
                        stats.staged_root_visible_bytes, stats.staged_root_visible_entries,
                        stats.staged_root_unique_bytes, stats.staged_root_unique_inodes,
                        crate::util::human(stats.bytes), stats.errors, pending.len());
                        crate::util::systemd_notify(
                            "READY=1\nSTATUS=Rescan complete; reconciling queued events",
                        );
                    }
                    Err(e) if cancelled_for_pressure => {
                        index_uncertain = true;
                        RESCAN.store(true, Ordering::SeqCst);
                        rescan_retry_after = Some(persist_reconciliation_retry_deadline(
                            &store,
                            failed_reconciliation_attempts,
                        )?);
                        store.set_meta("dirty_since", &now_secs().to_string())?;
                        store.set_meta(
                            "dirty_reason",
                            "reconciliation cancelled while host pressure was elevated",
                        )?;
                        tracing::warn!(
                            error = %e,
                            "reconciliation cancelled under host pressure; old index retained"
                        );
                        crate::util::systemd_notify(
                        "READY=1\nSTATUS=Reconciliation cancelled under host pressure; waiting to retry",
                    );
                    }
                    Err(e) => {
                        index_uncertain = true;
                        store.set_meta("dirty_since", &now_secs().to_string())?;
                        failed_reconciliation_attempts =
                            failed_reconciliation_attempts.saturating_add(1);
                        store.set_meta(
                            "failed_reconciliation_attempts",
                            &failed_reconciliation_attempts.to_string(),
                        )?;
                        if reconciliation_retry_should_pause(
                            reconcile_first,
                            failed_reconciliation_attempts,
                        ) {
                            reconciliation_retry_paused = true;
                            rescan_retry_after = Some(persist_reconciliation_retry_deadline(
                                &store,
                                failed_reconciliation_attempts,
                            )?);
                            RESCAN.store(true, Ordering::SeqCst);
                            store.set_meta(
                            "dirty_reason",
                            &format!(
                                "rescan failed; automatic reconciliation paused after {failed_reconciliation_attempts} failed full scans: {e:#}"
                            ),
                        )?;
                            tracing::error!(
                                failed_attempts = failed_reconciliation_attempts,
                                error = %e,
                                "reconciliation repeatedly failed; automatic scans paused and previous index retained"
                            );
                            crate::util::systemd_notify(
                            "READY=1\nSTATUS=Reconciliation repeatedly failed; automatic scans paused",
                        );
                        } else {
                            store.set_meta(
                                "dirty_reason",
                                &format!("rescan failed; one bounded retry remains: {e:#}"),
                            )?;
                            rescan_retry_after = Some(persist_reconciliation_retry_deadline(
                                &store,
                                failed_reconciliation_attempts,
                            )?);
                            RESCAN.store(true, Ordering::SeqCst);
                            tracing::error!(
                                failed_attempts = failed_reconciliation_attempts,
                                error = %e,
                                "rescan failed; existing index retained and one bounded retry scheduled"
                            );
                            crate::util::systemd_notify(
                                "READY=1\nSTATUS=Rescan failed; bounded retry pending",
                            );
                        }
                    }
                }
            }
            crate::util::write_heartbeat(db);
            last_flush = Instant::now() - flush_every;
            last_ckpt = Instant::now();
        }
        // Sample the host before considering a full-root rebuild. The earlier
        // ordering could start reconciliation first and only notice elevated
        // pressure later, after the scan had already become the dominant load.
        // The watcher remains active while a request is deferred, and events
        // drain after the pressure gate observes sustained recovery.
        let health = crate::guard::sample(&watch_dir);
        let pressure = pressure_gate.observe(health.level(MIN_FREE_BYTES));
        if accounting_check_should_cancel(
            pressure,
            kernel_empty,
            pending.len(),
            deferred_from.len().saturating_add(target_refreshes.len()),
            reconcile_first,
        ) || RESCAN.load(Ordering::SeqCst)
        {
            if let Some(check) = &accounting_check {
                check.cancel.store(true, Ordering::Relaxed);
            }
        }
        let (pressure_defers_reconciliation, report_pressure_deferral) =
            reconciliation_deferral_transition(
                reconciliation_deferred,
                pressure,
                &health,
                MIN_FREE_BYTES,
            );
        if scan_job.is_some() && crate::guard::should_cancel_reconciliation(&health, MIN_FREE_BYTES)
        {
            if let Some(cancel) = scan_cancel.as_ref() {
                if !cancel.swap(true, Ordering::SeqCst) {
                    tracing::warn!(
                        reason = health.reason(MIN_FREE_BYTES),
                        "cancelling background reconciliation under elevated or critical host pressure"
                    );
                }
            }
        }
        let periodic_observation_due =
            reconcile_first && scan_first_observation_due(last_reconcile, Instant::now());
        if periodic_observation_due {
            RESCAN.store(true, Ordering::SeqCst);
        }
        let rescan_requested = RESCAN.load(Ordering::SeqCst);
        let queues_drained = kernel_empty
            && pending.is_empty()
            && deferred_from.is_empty()
            && target_refreshes.is_empty();
        let backoff_elapsed =
            rescan_retry_after.is_none_or(|retry_after| Instant::now() >= retry_after);
        let retry_backoff_elapsed = reconciliation_retry_gate_elapsed(
            reconcile_first,
            periodic_observation_due,
            backoff_elapsed,
        );
        if periodic_observation_due && !backoff_elapsed {
            // A fixed-cadence observation is scan-first mode's recovery path.
            // Do not let exponential retry backoff stretch the freshness
            // interval beyond the next scheduled full observation. Publish the
            // adjusted due time so `dux status` agrees with the scheduler.
            let due = now_secs();
            store.set_meta("reconciliation_retry_after_unix", &due.to_string())?;
            rescan_retry_after = Some(Instant::now());
        }
        // Recompute after reaping completed workers: a failed scan may have
        // opened the circuit and scheduled its cooldown earlier in this loop.
        let retry_circuit_blocks_request = reconciliation_retry_circuit_blocks_request(
            reconciliation_retry_paused,
            retry_backoff_elapsed,
        );
        let reconciliation_ready = reconciliation_scan_ready(
            rescan_requested,
            queues_drained,
            last_caught_up.elapsed(),
            retry_backoff_elapsed,
        );
        if automatic_reconciliation_request_allowed(
            retry_circuit_blocks_request,
            scan_job.is_some(),
            accounting_check.is_some(),
            rescan_requested,
        ) && RESCAN.swap(false, Ordering::SeqCst)
        {
            if pressure_defers_reconciliation {
                RESCAN.store(true, Ordering::SeqCst);
                if report_pressure_deferral {
                    tracing::warn!(
                        pressure = ?pressure,
                        reason = health.reason(MIN_FREE_BYTES),
                        load1 = health.load1,
                        host_cpus = health.ncpu,
                        psi_cpu_avg10 = health.psi_cpu10,
                        psi_memory_avg10 = health.psi_mem10,
                        psi_io_avg10 = health.psi_io10,
                        available_memory_bytes = health.mem_avail,
                        available_disk_bytes = health.disk_avail,
                        "deferring full reconciliation until host pressure meets recovery admission criteria"
                    );
                    crate::util::systemd_notify(
                        "READY=1\nSTATUS=Reconciliation deferred; waiting for host pressure recovery",
                    );
                }
                reconciliation_deferred = true;
            } else if !reconciliation_ready {
                // Prefer draining a finite backlog. A persistent backlog is
                // different: start a fresh baseline after the grace period,
                // retain pending events, and replay them after atomic install.
                // The dirty marker remains set until queue replay and accounting
                // verification prove the resulting index is exact.
                if reconciliation_deferred {
                    let status = format!(
                        "READY=1\nSTATUS=Reconciliation waiting for queued changes ({} pending)",
                        pending
                            .len()
                            .saturating_add(deferred_from.len())
                            .saturating_add(target_refreshes.len())
                    );
                    crate::util::systemd_notify(&status);
                }
                RESCAN.store(true, Ordering::SeqCst);
                reconciliation_deferred = false;
            } else {
                if !queues_drained {
                    tracing::warn!(
                        pending_updates = pending.len(),
                        deferred_renames = deferred_from.len(),
                        kernel_queue_empty = kernel_empty,
                        behind_seconds = last_caught_up.elapsed().as_secs(),
                        "starting reconciliation with queued changes after persistent backlog"
                    );
                }
                reconciliation_deferred = false;
                rescan_retry_after = None;
                last_reconcile = Instant::now();
                manual_rescan_pending = false;
                born.clear();
                lost_events = false;
                forced_reconciliation_after_scan = false;
                let scan_db = db.to_path_buf();
                let scan_root = root_canon.clone();
                let use_in_place_observation =
                    scan_first_can_use_in_place(reconcile_first, baseline_ready, one_file_system);
                tracing::info!(
                    use_in_place_observation,
                    baseline_ready,
                    read_only_source,
                    "selected reconciliation implementation"
                );
                let cancel = Arc::new(AtomicBool::new(false));
                let mut opts =
                    background_reconciliation_scan_options(one_file_system, Arc::clone(&cancel));
                opts.priority_paths = std::mem::take(&mut pending_scan_hints);
                tracing::debug!(
                    priority_directory_count = opts.priority_paths.len(),
                    "starting full reconciliation with resolved directory hints"
                );
                tracing::info!(
                    "starting background reconciliation of {}; event ingestion remains active",
                    root_canon.display()
                );
                crate::util::systemd_notify(if reconcile_first {
                    "RELOADING=1\nSTATUS=Observing filesystem; collecting concurrent change hints"
                } else {
                    "RELOADING=1\nSTATUS=Rebuilding index; collecting concurrent changes"
                });
                scan_cancel = Some(cancel);
                // Scan-first mode treats events as hints and does not mutate the
                // live index while observing. Mark the index uncertain before
                // the writer opens SQLite so event bursts cannot trigger a
                // competing metadata write during the publication transaction.
                if reconcile_first {
                    index_uncertain = true;
                    if !read_only_source {
                        store.set_meta("dirty_since", &now_secs().to_string())?;
                        store.set_meta(
                            "dirty_reason",
                            "periodic filesystem observation in progress",
                        )?;
                    }
                }
                scan_job = Some(std::thread::spawn(move || {
                    if use_in_place_observation {
                        (|| {
                            let mut live = Store::open_rw(&scan_db)?;
                            crate::scan::reconcile_in_place(&mut live, &scan_root, &opts)
                        })()
                    } else {
                        crate::scan::rebuild_atomic(&scan_db, &scan_root, &opts)
                    }
                }));
            }
        }
        // BLOCK (not busy-poll) until an event arrives OR the next flush is due —
        // whichever first. While a full scan is active, cap the sleep at 100ms so
        // host pressure is sampled promptly so hard memory/storage pressure
        // can cancel optional work before the next full flush interval.
        // poll() is interrupted by SIGTERM/SIGHUP (EINTR), so those flags are
        // still checked promptly too.
        let active_flush_interval = if draining_backlog {
            Duration::from_millis(100)
        } else {
            flush_every
        };
        let wait_ms = watcher_poll_timeout(
            active_flush_interval,
            last_flush.elapsed(),
            scan_job.is_some(),
        );
        let mut pfd = libc::pollfd {
            fd: hint_reader.as_ref().map_or(fan, |reader| reader.notify.0),
            events: libc::POLLIN,
            revents: 0,
        };
        // A time-sliced parse can retain records already read from fanotify.
        // Consume that suffix before polling or reading newer records.
        let pr = if buf_len > 0 {
            1
        } else {
            unsafe { libc::poll(&mut pfd, 1, wait_ms.max(0)) }
        };
        if pr > 0 {
            if let Some(reader) = hint_reader.as_ref() {
                reader.acknowledge();
            }
        }
        if pr < 0 {
            // EINTR (a signal) is expected — loop and the flags above handle it.
            // Any OTHER poll error must not turn into a silent busy-spin; log it
            // and back off briefly (defense-in-depth — not reachable in practice).
            let err = std::io::Error::last_os_error();
            if !matches!(err.raw_os_error(), Some(libc::EINTR) | Some(libc::EAGAIN)) {
                tracing::warn!("fanotify poll error: {err}");
                std::thread::sleep(Duration::from_millis(50));
            }
        } else if pr > 0 {
            // Drain queued events — but only up to MAX_READS_PER_TICK per tick, so
            // the CPU governor gets to run between bursts under a storm instead of
            // us draining an unbounded flood in one uninterruptible spike. Anything
            // left stays in the kernel fanotify queue for the next tick.
            let mut reads = 0u32;
            let drain_deadline = Instant::now() + Duration::from_millis(100);
            loop {
                if buf_len == 0 {
                    let n = if let Some(reader) = hint_reader.as_ref() {
                        match reader.rx.try_recv() {
                            Ok(bytes) => {
                                debug_assert!(bytes.len() <= buf.len());
                                buf[..bytes.len()].copy_from_slice(&bytes);
                                reads += 1;
                                bytes.len() as isize
                            }
                            Err(TryRecvError::Empty) => {
                                kernel_empty = reader.kernel_empty.load(Ordering::Acquire)
                                    && reader.queued_chunks.load(Ordering::Acquire) == 0;
                                if reader.dropped.swap(false, Ordering::AcqRel) {
                                    lost_events = true;
                                    forced_reconciliation_after_scan = true;
                                    index_uncertain = true;
                                    persist_dirty_marker_best_effort(
                                        &store,
                                        "fanotify hint reader could not account for an event chunk",
                                    );
                                    if hint_loss_requests_immediate_scan(reconcile_first) {
                                        RESCAN.store(true, Ordering::SeqCst);
                                    }
                                    tracing::warn!(
                                        "fanotify hint reader could not account for input; the next full observation remains authoritative"
                                    );
                                }
                                break;
                            }
                            Err(TryRecvError::Disconnected) => {
                                kernel_empty = false;
                                lost_events = true;
                                forced_reconciliation_after_scan = true;
                                index_uncertain = true;
                                if hint_loss_requests_immediate_scan(reconcile_first) {
                                    RESCAN.store(true, Ordering::SeqCst);
                                }
                                tracing::error!(
                                    "fanotify hint reader stopped unexpectedly; index remains stale until the next observation"
                                );
                                break;
                            }
                        }
                    } else {
                        unsafe {
                            libc::read(fan, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len())
                        }
                    };
                    if n > 0 {
                        if hint_reader.is_none() {
                            buf_len = n as usize;
                            reads += 1;
                        } else {
                            buf_len = n as usize;
                        }
                    } else {
                        if n < 0 {
                            let err = std::io::Error::last_os_error();
                            match err.raw_os_error() {
                                Some(libc::EINTR) => continue,
                                Some(libc::EAGAIN) => {}
                                _ => {
                                    lost_events = true;
                                    forced_reconciliation_after_scan = true;
                                    kernel_empty = false;
                                    tracing::warn!("fanotify read error: {err}");
                                    break;
                                }
                            }
                        }
                        // Reached EAGAIN (or EOF): the kernel queue is drained.
                        kernel_empty = true;
                        break;
                    }
                }

                let parse_deadline = (Instant::now() + EVENT_PARSE_SLICE).min(drain_deadline);
                let (seen, resolved, overflows, consumed) = if reconcile_first {
                    let input_len = buf_len;
                    let (overflows, mut consumed, mut lost, handles) =
                        parse_fanotify_hints(&buf[..input_len], MAX_HINT_EVENTS_PER_CHUNK);
                    if consumed != input_len {
                        // The fanotify read is expected to contain complete
                        // records. Widen instead of carrying an unaccounted
                        // suffix across asynchronous resolver jobs.
                        lost = true;
                        consumed = input_len;
                    }
                    kernel_overflow_notifications =
                        kernel_overflow_notifications.saturating_add(overflows);
                    if lost || overflows > 0 {
                        if !index_uncertain {
                            persist_dirty_marker_best_effort(
                                &store,
                                if overflows > 0 {
                                    "fanotify queue overflow"
                                } else {
                                    "fanotify hint scope could not be validated"
                                },
                            );
                        }
                        index_uncertain = true;
                        if scan_job.is_some() || overflows > 0 {
                            lost_events = true;
                        }
                        if overflows > 0 {
                            forced_reconciliation_after_scan = true;
                            if hint_loss_requests_immediate_scan(reconcile_first) {
                                RESCAN.store(true, Ordering::SeqCst);
                            }
                        }
                    }
                    let job = HintResolutionJob { handles };
                    if let Some(resolver) = hint_resolver.as_ref() {
                        if resolver.tx.try_send(job).is_err() {
                            note_hint_pipeline_drop(hint_reader.as_ref(), Some(resolver));
                        }
                    } else {
                        note_hint_pipeline_drop(hint_reader.as_ref(), None);
                    }
                    (0, 0, 0, consumed)
                } else {
                    parse_events_bounded(
                        &buf[..buf_len],
                        &fsfds,
                        &mut handle_capable_fsids,
                        &root_canon,
                        db,
                        &mut pending,
                        &mut born,
                        &mut target_refreshes,
                        &mut indexed_parent_paths,
                        &mut store,
                        scan_job.is_none(),
                        &mut lost_events,
                        MAX_EVENTS_PER_PARSE,
                        parse_deadline,
                    )
                };
                if consumed > 0 {
                    buf.copy_within(consumed..buf_len, 0);
                    buf_len -= consumed;
                } else {
                    // The parser yielded at its time boundary before consuming a
                    // complete record. Keep the bytes and resume before new reads.
                    kernel_empty = false;
                    break;
                }
                ev_seen += seen;
                ev_resolved += resolved;
                kernel_overflow_notifications =
                    kernel_overflow_notifications.saturating_add(overflows);

                if buf_len > 0 {
                    kernel_empty = false;
                    if Instant::now() >= drain_deadline {
                        break;
                    }
                    continue;
                }
                if reads >= MAX_READS_PER_TICK || Instant::now() >= drain_deadline {
                    kernel_empty = false; // hit the cap; more work may remain in kernel
                    if let Some(reader) = hint_reader.as_ref() {
                        // The bounded userspace ring may still have chunks even
                        // though the kernel queue is empty. Re-wake the
                        // coordinator so it continues draining that ring.
                        notify_hint_reader(reader.notify.0);
                    }
                    break;
                }
            }
            // C3: a daemon with CAP_SYS_ADMIN but NOT CAP_DAC_READ_SEARCH receives
            // events but open_by_handle_at EPERMs on every one, so NONE resolve and
            // the index silently never updates. If we've seen a meaningful number of
            // events and resolved zero, say so loudly (once) and mark dirty.
            if !reconcile_first && !resolve_warned && ev_seen >= 64 && ev_resolved == 0 {
                tracing::error!(
                    "received {ev_seen} fanotify events but resolved 0 paths — the daemon \
                     likely lacks CAP_DAC_READ_SEARCH (open_by_handle_at is failing). Live \
                     updates are NOT being recorded; the index is marked dirty. Grant \
                     CAP_DAC_READ_SEARCH (the packaged dux.service does) and restart."
                );
                persist_dirty_marker_best_effort(&store, "fanotify events could not be resolved");
                index_uncertain = true;
                resolve_warned = true;
            }
            // Overload backstop: if flushing can't keep up (SQLite wedged, or a
            // genuine storm), don't let pending grow until the daemon is OOM-killed.
            // Drop the backlog and mark dirty so a reconcile rescan repairs it.
            if backlog_exceeded(pending.len(), deferred_from.len(), target_refreshes.len()) {
                tracing::warn!(
                    "pending backlog exceeded {MAX_PENDING} — dropping it to bound memory; \
                     index marked dirty (rescan to reconcile)"
                );
                persist_dirty_marker_best_effort(
                    &store,
                    "event backlog exceeded the in-memory bound",
                );
                lost_events = true;
                updates_dropped = updates_dropped.saturating_add(
                    pending
                        .len()
                        .saturating_add(deferred_from.len())
                        .saturating_add(target_refreshes.len()) as u64,
                );
                pending.clear();
                born.clear();
                deferred_from.clear(); // backlog dropped; a rescan will reconcile
                target_refreshes.clear();
            }
        }

        if lost_events || resolve_warned {
            index_uncertain = true;
            if lost_events {
                // Event-apply mode needs a full repair after capture loss.
                // Scan-first mode treats fanotify as a lossy hint source and
                // waits for its fixed observation cadence instead.
                if !reconcile_first {
                    RESCAN.store(true, Ordering::SeqCst);
                }
            }
            // Publish loss before starting a potentially expensive SQL batch.
            crate::util::write_watch_status_with_mode(
                db,
                pending
                    .len()
                    .saturating_add(deferred_from.len())
                    .saturating_add(target_refreshes.len()),
                kernel_empty,
                ev_seen,
                ev_resolved,
                updates_committed,
                last_commit_at,
                updates_dropped,
                last_caught_up.elapsed().as_millis().min(u64::MAX as u128) as u64,
                MAX_PENDING,
                kernel_overflow_notifications,
                true,
                reconcile_first,
            );
        }
        if last_flush.elapsed() >= active_flush_interval {
            if last_mount_check.elapsed() >= Duration::from_secs(5) {
                last_mount_check = Instant::now();
                match mount_topology(&root_canon, one_file_system) {
                    Ok(current) if current != initial_mounts => {
                        match mount_topology_action(&initial_mounts, &current) {
                            MountTopologyAction::RefreshWatchCoverage => {
                                mark_index_dirty(
                                    &store,
                                    "filesystem mounts changed; restarting watch coverage",
                                )
                                .context("recording filesystem mount change")?;
                                crate::util::systemd_notify(
                                    "STOPPING=1\nSTATUS=Filesystem mounts changed; restart required",
                                );
                                anyhow::bail!("filesystem mounts changed; restart daemon to refresh watch coverage and reconcile");
                            }
                            MountTopologyAction::ReconcileVisibleTree => {
                                index_uncertain = true;
                                mark_index_dirty(
                                    &store,
                                    "non-watched filesystem mounts changed; reconciling visible paths",
                                )
                                .context("recording non-watched filesystem mount change")?;
                                crate::util::systemd_notify(
                                    "READY=1\nSTATUS=Mount topology changed; reconciling visible paths",
                                );
                                RESCAN.store(true, Ordering::SeqCst);
                            }
                            MountTopologyAction::DuplicateStorageViewOnly => {
                                tracing::debug!(
                                    "duplicate-storage mount topology changed; scan coverage is unchanged"
                                );
                            }
                        }
                        initial_mounts = current;
                    }
                    Err(error) => {
                        index_uncertain = true;
                        mark_index_dirty(&store, "cannot verify filesystem mount coverage")
                            .context("recording mount coverage uncertainty")?;
                        tracing::warn!("cannot verify mount topology: {error:#}");
                    }
                    Ok(_) => {}
                }
            }
            // RESOURCE GUARDIAN: limit resource use under observed pressure.
            // Sample pressure (free RAM, disk, load, kernel PSI) and self-throttle:
            //   Critical → PAUSE our own writes (keep `pending`, lose nothing) and
            //              hand SQLite's caches back to the OS.
            //   Elevated → still index, but skip the optional extra I/O/CPU
            //              (WAL checkpoint + alert scan) so we don't add load.
            //   Normal   → full operation.
            // Pause/resume state transitions are evaluated EVERY cycle, independent
            // of whether there's pending work, so `status`/TUI never get stuck
            // showing PAUSED after the host recovers while idle (B6). The reason
            // reuses the single sample above (no second /proc read, B5).
            if scan_job.is_none() && pressure == crate::guard::Pressure::Critical {
                if !writes_paused {
                    let reason = health.reason(MIN_FREE_BYTES);
                    tracing::warn!(
                        "host under pressure ({reason}) — pausing index writes to protect it"
                    );
                    if let Err(error) = persist_pause_state(&store, Some(reason)) {
                        index_uncertain = true;
                        tracing::error!(
                            "could not persist host-pressure pause state; health is marked uncertain: {error:#}"
                        );
                    }
                    writes_paused = true;
                }
                // give SQLite's page/heap caches back to the OS under pressure.
                let _ = store.conn.execute_batch("PRAGMA shrink_memory");
            } else if scan_job.is_none() && writes_paused {
                tracing::info!("host recovered — resuming index writes");
                if let Err(error) = persist_pause_state(&store, None) {
                    index_uncertain = true;
                    tracing::error!(
                        "could not clear persisted host-pressure pause state; health is marked uncertain: {error:#}"
                    );
                }
                writes_paused = false;
            }
            // Flush only when healthy AND there's work. Under Critical we keep
            // `pending` (bounded by the MAX_PENDING backstop) and lose nothing.
            if baseline_ready
                && scan_job.is_none()
                && pressure != crate::guard::Pressure::Critical
                && !wal_write_guarded
                && (!pending.is_empty()
                    || !deferred_from.is_empty()
                    || !target_refreshes.is_empty())
            {
                if let Err(error) = materialize_target_refreshes(
                    &mut target_refreshes,
                    &mut pending,
                    deferred_from.len(),
                    &store,
                    &root_canon,
                    &mut indexed_parent_paths,
                ) {
                    lost_events = true;
                    index_uncertain = true;
                    tracing::error!(error = %error, "target refresh failed; index marked uncertain");
                }
                // Bound the flush burst so the CPU governor can actually hold a low
                // cap: a storm can pile tens of thousands of entries into `pending`,
                // and indexing them all in ONE flush is a multi-second CPU spike the
                // governor can't offset. Flush at most FLUSH_BATCH per cycle; the
                // rest drains over subsequent (throttled) cycles. Splitting is safe:
                // rename pairing across batches is handled by `deferred_from`.
                let attempted = pending.len().min(FLUSH_BATCH);
                let progress = FlushProgress {
                    events_seen: ev_seen,
                    events_resolved: ev_resolved,
                    updates_committed,
                    last_commit_at,
                    updates_dropped,
                    kernel_overflow_notifications,
                    behind_ms: last_caught_up.elapsed().as_millis().min(u64::MAX as u128) as u64,
                    max_pending: MAX_PENDING,
                    uncertain: index_uncertain || lost_events,
                };
                let flush_pending_before = pending.len();
                let flush_deferred_before = deferred_from.len();
                let flush_started = Instant::now();
                let result = flush_batch_with_progress(
                    &mut store,
                    &mut pending,
                    &mut born,
                    &mut deferred_from,
                    db,
                    growth_keep_secs,
                    FLUSH_BATCH,
                    kernel_empty,
                    progress,
                );
                let flush_elapsed = flush_started.elapsed();
                if flush_elapsed >= Duration::from_millis(250) {
                    let now = now_secs().max(0) as u64;
                    let previous = LAST_SLOW_FLUSH_LOG_AT.load(Ordering::Relaxed);
                    if now > previous
                        && LAST_SLOW_FLUSH_LOG_AT
                            .compare_exchange(previous, now, Ordering::Relaxed, Ordering::Relaxed)
                            .is_ok()
                    {
                        tracing::warn!(
                            elapsed_ms = flush_elapsed.as_millis(),
                            attempted_updates = attempted,
                            pending_before = flush_pending_before,
                            deferred_before = flush_deferred_before,
                            succeeded = result.is_ok(),
                            "slow event flush"
                        );
                    }
                }
                match result {
                    // Fully drained → the born-set is done; reset it for the next
                    // window. (Partial batches prune born per-key above.)
                    Ok(()) => {
                        draining_backlog = !pending.is_empty() || !target_refreshes.is_empty();
                        indexed_parent_paths.clear();
                        consecutive_flush_errors = 0;
                        let _ = store
                            .conn
                            .execute("DELETE FROM meta WHERE key='flush_error'", []);
                        updates_committed = updates_committed.saturating_add(attempted as u64);
                        if attempted > 0 {
                            last_commit_at = crate::util::now_secs();
                        }
                        if pending.is_empty() {
                            born.clear();
                        }
                    }
                    Err(e) => {
                        draining_backlog = false;
                        tracing::warn!("flush error (batch retained for retry): {e}");
                        store.set_meta("flush_error", &e.to_string()).ok();
                        consecutive_flush_errors = consecutive_flush_errors.saturating_add(1);
                        if consecutive_flush_errors >= 3 {
                            index_uncertain = true;
                            persist_dirty_marker_best_effort(
                                &store,
                                "repeated event flush failures",
                            );
                            store
                                .set_meta(
                                    "dirty_reason",
                                    "repeated write failure; pending changes retained",
                                )
                                .ok();
                        }
                    }
                }
                // alert scanning is an extra query — skip it while Elevated.
                if pressure == crate::guard::Pressure::Normal {
                    if let Some(cfg) = &alert {
                        if let Err(e) =
                            check_alerts(&store, cfg, &mut last_alert, &mut alert_children)
                        {
                            tracing::warn!("alert check error: {e}");
                        }
                    }
                }
            }
            // Heartbeat EVERY cycle, independent of flush success — a failing
            // flush must not make `daemon_live` read false (which would let a
            // concurrent `dux scan` corrupt the index). Written to tmpfs, so
            // an idle daemon makes no database/WAL writes at all.
            crate::util::write_heartbeat(db);
            // Caught up = kernel queue drained AND nothing left to index. Update the
            // timestamp here (post-flush) so a persistent `pending` backlog counts
            // as "behind" too, not just a full kernel queue.
            let caught_up = pipeline_is_caught_up(
                scan_job.is_some(),
                kernel_empty,
                pending.is_empty(),
                deferred_from.is_empty(),
                target_refreshes.is_empty(),
                index_uncertain,
                lost_events,
            );
            if caught_up {
                last_caught_up = Instant::now();
                if !was_caught_up {
                    crate::util::systemd_notify(
                        "READY=1\nSTATUS=Watching filesystem; incremental index is caught up",
                    );
                }
            } else if was_caught_up {
                crate::util::systemd_notify(
                    "READY=1\nSTATUS=Filesystem changes pending; index catching up",
                );
            }
            was_caught_up = caught_up;
            // Throttled/behind detection for the UI: if we haven't been fully caught
            // up in STALE_BEHIND_SECS, the governor is holding us back under
            // sustained load, so the index is intentionally stale. Record WHEN we
            // fell behind (once, on transition) so status/TUI can explain it; clear
            // it the moment we catch up. Writes only on a transition, so this adds
            // no steady-state I/O.
            let behind = last_caught_up.elapsed().as_secs() as i64;
            if scan_job.is_none() && behind >= STALE_BEHIND_SECS {
                if throttled_since.is_none() {
                    let since = now_secs() - behind;
                    store.set_meta("throttled_since", &since.to_string()).ok();
                    throttled_since = Some(since);
                    tracing::info!("index backlog persists — {behind}s since last caught up");
                }
            } else if scan_job.is_none()
                && behind < STALE_BEHIND_SECS
                && throttled_since.take().is_some()
            {
                let _ = store
                    .conn
                    .execute("DELETE FROM meta WHERE key='throttled_since'", []);
                tracing::info!("caught up — live updates resumed");
            }
            index_uncertain |= lost_events;
            crate::util::write_watch_status_with_mode(
                db,
                pending
                    .len()
                    .saturating_add(deferred_from.len())
                    .saturating_add(target_refreshes.len()),
                kernel_empty,
                ev_seen,
                ev_resolved,
                updates_committed,
                last_commit_at,
                updates_dropped,
                last_caught_up.elapsed().as_millis().min(u64::MAX as u128) as u64,
                MAX_PENDING,
                kernel_overflow_notifications,
                index_uncertain,
                reconcile_first,
            );
            // Reap any finished alert children EVERY cycle (cheap try_wait) so a
            // stopped alert script never lingers as a zombie under sustained
            // Elevated pressure or while idle (check_alerts only reaps at Normal).
            alert_children.retain_mut(|c| !matches!(c.try_wait(), Ok(Some(_))));
            // Checkpoint the WAL with PASSIVE (never blocks a live TUI reader).
            // Run it under Normal AND Elevated — skipping it under Elevated let the
            // WAL grow unbounded on a busy high-churn host. Oversized or guarded
            // WALs trigger prompt retries; incomplete progress blocks additional
            // event commits while capture continues into its bounded backlog.
            let wal_bytes = match std::fs::metadata(format!("{}-wal", db.display())) {
                Ok(metadata) => Some(metadata.len()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(0),
                Err(error) => {
                    if wal_warning_allowed() {
                        tracing::warn!(%error, path = %db.display(), "unable to measure SQLite WAL size");
                    }
                    None
                }
            };
            let due = last_ckpt.elapsed() >= Duration::from_secs(60);
            let oversized = wal_bytes.is_some_and(|bytes| bytes > MAX_WAL_BYTES);
            let checkpoint_interval = if oversized || wal_write_guarded {
                1
            } else {
                10
            };
            let mut checkpoint_result = None;
            if scan_job.is_none()
                && ((pressure != crate::guard::Pressure::Critical && due)
                    || oversized
                    || wal_write_guarded)
                && last_ckpt.elapsed() >= Duration::from_secs(checkpoint_interval)
            {
                match wal_checkpoint_progress(&store.conn) {
                    Ok((busy, log_frames, checkpointed_frames)) => {
                        checkpoint_result = Some((busy, log_frames, checkpointed_frames));
                        if busy != 0 || checkpointed_frames < log_frames {
                            if wal_warning_allowed() {
                                tracing::warn!(busy, log_frames, checkpointed_frames,
                                    wal_bytes = ?wal_bytes,
                                    "SQLite WAL checkpoint made partial progress; readers may be pinning frames");
                            }
                        } else {
                            tracing::debug!(
                                log_frames,
                                checkpointed_frames,
                                "SQLite WAL checkpoint completed"
                            );
                        }
                    }
                    Err(error) => {
                        if wal_warning_allowed() {
                            tracing::warn!(%error, "SQLite WAL checkpoint failed");
                        }
                    }
                }
                last_ckpt = Instant::now();
            }
            let was_wal_guarded = wal_write_guarded;
            wal_write_guarded =
                wal_write_guard_transition(wal_write_guarded, wal_bytes, checkpoint_result);
            if wal_write_guarded != was_wal_guarded {
                if wal_write_guarded {
                    tracing::error!(wal_bytes = ?wal_bytes,
                        "SQLite WAL exceeded its budget; event writes are paused until checkpointing resumes");
                } else {
                    tracing::info!(wal_bytes = ?wal_bytes,
                        "SQLite WAL checkpoint recovered; event writes resumed");
                }
            }
            if scan_job.is_none() && pressure != crate::guard::Pressure::Critical {
                // Scan-first accounting verifies the last completed observation.
                // Do not require the global fanotify queue to become empty: it
                // can contain only out-of-root filesystem activity, and any
                // in-scope hint received while checking cancels this worker.
                let caught_up = (reconcile_first || kernel_empty)
                    && pending.is_empty()
                    && deferred_from.is_empty();
                if accounting_check.is_none()
                    && pending_reconciliation_check == Some(true)
                    && caught_up
                {
                    match start_accounting_check(
                        db,
                        AccountingCheckKind::Reconciliation {
                            safe_to_clear: true,
                        },
                    ) {
                        Ok(check) => {
                            tracing::debug!("starting post-reconciliation accounting verification");
                            accounting_check = Some(check);
                        }
                        Err(e) => {
                            index_uncertain = true;
                            tracing::warn!(error = %e, "could not start post-reconciliation accounting verification");
                            store.set_meta("dirty_since", &now_secs().to_string())?;
                            store.set_meta(
                                "dirty_reason",
                                "post-reconciliation accounting verification could not start",
                            )?;
                            RESCAN.store(true, Ordering::SeqCst);
                        }
                    }
                } else if accounting_check.is_none()
                    && pending_reconciliation_check.is_none()
                    && last_integrity.elapsed() >= Duration::from_secs(300)
                    && caught_up
                    && !index_uncertain
                {
                    match start_accounting_check(db, AccountingCheckKind::Periodic) {
                        Ok(check) => {
                            tracing::debug!(
                                "starting background directory-accounting verification"
                            );
                            accounting_check = Some(check);
                        }
                        Err(e) => {
                            index_uncertain = true;
                            tracing::warn!(error = %e, "could not start accounting verification");
                            store.set_meta("dirty_since", &now_secs().to_string())?;
                            store.set_meta(
                                "dirty_reason",
                                "directory accounting verification could not start",
                            )?;
                            RESCAN.store(true, Ordering::SeqCst);
                        }
                    }
                    last_integrity = Instant::now();
                }
                if !coverage_incomplete
                    && !reconciliation_retry_paused
                    && (index_uncertain || store.get_meta("dirty_since")?.is_some())
                    && dirty_retry_due_for_event_mode(
                        reconcile_first,
                        true,
                        last_reconcile,
                        Instant::now(),
                    )
                {
                    RESCAN.store(true, Ordering::SeqCst);
                }
            }
            last_flush = Instant::now();
        }
        // CPU governor: repay event-thread CPU debt while fanotify queue pressure
        // is low. When the queue crosses its high watermark, bypass sleep until
        // it falls below the low watermark. This is a soft event-thread target;
        // the service CPUQuota remains the process-wide cap. Idle iterations do
        // not sleep here.
        gov.throttle(
            fan,
            db,
            Some(ThrottleWatchSnapshot {
                pending: pending
                    .len()
                    .saturating_add(deferred_from.len())
                    .saturating_add(target_refreshes.len()),
                events_seen: ev_seen,
                events_resolved: ev_resolved,
                updates_committed,
                last_commit_at,
                updates_dropped,
                behind_ms: last_caught_up.elapsed().as_millis().min(u64::MAX as u128) as u64,
                kernel_overflow_notifications,
                uncertain: index_uncertain || lost_events,
                scan_first: reconcile_first,
            }),
        );
    }
}

/// Observe mount identities without touching mountpoints: stat/statfs of an
/// unavailable remote mount could block the event loop. A changed identity
/// requires new fanotify marks and a baseline through the service restart path.
fn mount_topology(root: &Path, one_fs: bool) -> Result<Vec<String>> {
    let info = std::fs::read_to_string("/proc/self/mountinfo")
        .context("reading filesystem mount topology")?;
    Ok(mount_topology_from_info(root, one_fs, &info))
}

/// Fingerprint mounts that can cover or sit beneath the indexed root.
/// Pseudo and duplicate-view mounts remain relevant because they can replace
/// an indexed path. Mounts below those excluded subtrees are invisible to the
/// scanner, however, so including them would cause needless full-root rebuilds
/// for changes under locations such as `/run` or a container overlay.
fn mount_topology_from_info(root: &Path, one_fs: bool, info: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    for line in info.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let fields: Vec<_> = left.split_whitespace().collect();
        if fields.len() < 5 {
            continue;
        }
        let fs = right.split_whitespace().next().unwrap_or("");
        let path = unescape_mount(fields[4]);
        let relevant = if one_fs {
            root.starts_with(&path)
        } else {
            root.starts_with(&path) || path.starts_with(root)
        };
        if relevant {
            candidates.push((
                path,
                fs.to_owned(),
                format!("{} {} {} {}", fields[0], fields[2], fields[4], fs),
            ));
        }
    }
    let mut mounts: Vec<_> = candidates
        .iter()
        .filter(|(path, _, _)| {
            !candidates.iter().any(|(ancestor, fs, _)| {
                ancestor != path
                    && ancestor != root
                    && ancestor.starts_with(root)
                    && path.starts_with(ancestor)
                    && is_unwatched_mount_fs(fs)
            })
        })
        .map(|(_, _, fingerprint)| fingerprint.clone())
        .collect();
    mounts.sort_unstable();
    mounts
}

/// Whether a topology change adds, removes, or replaces a filesystem that
/// needs a fanotify mark. Other mount changes still require a visible-tree
/// reconciliation, but do not require restarting the watcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MountTopologyAction {
    RefreshWatchCoverage,
    ReconcileVisibleTree,
    DuplicateStorageViewOnly,
}

fn mount_topology_action(previous: &[String], current: &[String]) -> MountTopologyAction {
    if mount_topology_requires_watch_refresh(previous, current) {
        MountTopologyAction::RefreshWatchCoverage
    } else if mount_topology_requires_visible_reconcile(previous, current) {
        MountTopologyAction::ReconcileVisibleTree
    } else {
        MountTopologyAction::DuplicateStorageViewOnly
    }
}

fn mount_topology_requires_watch_refresh(previous: &[String], current: &[String]) -> bool {
    fn watched(mounts: &[String]) -> Vec<&str> {
        mounts
            .iter()
            .filter_map(|mount| {
                let fs = mount.rsplit_once(' ')?.1;
                (!is_unwatched_mount_fs(fs)).then_some(mount.as_str())
            })
            .collect()
    }

    watched(previous) != watched(current)
}

/// Whether a non-watchable mount change can alter the tree Dux actually
/// indexes. Overlayfs mounts are duplicate storage views and are excluded by
/// the scanner, so their lifecycle does not invalidate the indexed baseline.
/// Pseudo and other excluded filesystems can cover previously visible paths and
/// therefore continue to require reconciliation.
fn mount_topology_requires_visible_reconcile(previous: &[String], current: &[String]) -> bool {
    fn indexed_views(mounts: &[String]) -> Vec<&str> {
        mounts
            .iter()
            .filter_map(|mount| {
                let fs = mount.rsplit_once(' ')?.1;
                (fs != "overlay").then_some(mount.as_str())
            })
            .collect()
    }

    indexed_views(previous) != indexed_views(current)
}

fn is_unwatched_mount_fs(fs: &str) -> bool {
    matches!(
        fs,
        "proc"
            | "sysfs"
            | "cgroup"
            | "cgroup2"
            | "devpts"
            | "devtmpfs"
            | "tmpfs"
            | "debugfs"
            | "tracefs"
            | "securityfs"
            | "bpf"
            | "mqueue"
            | "pstore"
            | "binfmt_misc"
            | "fusectl"
            | "configfs"
            | "nsfs"
            | "hugetlbfs"
            | "ramfs"
            | "autofs"
            | "rpc_pipefs"
            | "selinuxfs"
            | "overlay"
            | "fuse"
    ) || fs.starts_with("fuse.")
}

/// Mountpoints at/under `root` on real filesystems (same set the scanner indexes,
/// i.e. excluding pseudo filesystems). With `one_fs`, only `root` itself.
fn real_mounts(root: &Path, one_fs: bool) -> Result<Vec<PathBuf>> {
    if one_fs {
        return Ok(vec![root.to_path_buf()]);
    }
    let mut out = vec![root.to_path_buf()];
    let mi = std::fs::read_to_string("/proc/self/mountinfo")
        .context("reading mount topology for fanotify coverage")?;
    out.extend(watchable_mounts_from_info(root, &mi));
    Ok(out)
}

/// Filesystem mounts the scanner can actually reach beneath `root`. Keep this
/// set aligned with scan pruning: a real filesystem nested below an excluded
/// pseudo or duplicate-view mount is invisible to the walk and must not receive
/// a fanotify mark. The excluded mountpoint itself can still cover an indexed
/// path, so mount-topology reconciliation tracks it separately.
fn watchable_mounts_from_info(root: &Path, info: &str) -> Vec<PathBuf> {
    let mut mounts = Vec::new();
    let mut candidates = Vec::new();
    for line in info.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let fields: Vec<_> = left.split_whitespace().collect();
        let Some(mountpoint) = fields.get(4) else {
            continue;
        };
        let Some(fstype) = right.split_whitespace().next() else {
            continue;
        };
        let path = unescape_mount(mountpoint);
        if path == root || path.starts_with(root) {
            candidates.push((path, fstype));
        }
    }

    for (path, fstype) in &candidates {
        if path != root && is_unwatched_mount_fs(fstype) {
            continue;
        }
        let hidden_by_excluded_ancestor = candidates.iter().any(|(ancestor, ancestor_fs)| {
            ancestor != path
                && ancestor != root
                && ancestor.starts_with(root)
                && path.starts_with(ancestor)
                && is_unwatched_mount_fs(ancestor_fs)
        });
        if !hidden_by_excluded_ancestor && path != root {
            mounts.push(path.clone());
        }
    }
    mounts
}

/// mountinfo octal-escapes space/tab/newline/backslash as \040 etc.
/// Works on raw bytes so non-ASCII (UTF-8) mountpoints survive intact.
fn unescape_mount(s: &str) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 4 <= b.len() {
            if let Ok(n) =
                u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 4]).unwrap_or(""), 8)
            {
                out.push(n);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    PathBuf::from(std::ffi::OsStr::from_bytes(&out))
}

/// The filesystem id (matches the fanotify event fsid) for the fs containing path.
fn statfs_fsid(path: &Path) -> Result<(i32, i32)> {
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .with_context(|| format!("encoding mount path {}", path.display()))?;
    let mut s = MaybeUninit::<libc::statfs>::uninit();
    anyhow::ensure!(
        unsafe { libc::statfs(c.as_ptr(), s.as_mut_ptr()) } == 0,
        "statfs {}: {}",
        path.display(),
        std::io::Error::last_os_error()
    );
    let v = unsafe { s.assume_init() }.f_fsid;
    // fsid_t is an opaque 8-byte struct ({ __val: [i32;2] }); read its two ints.
    let raw: [i32; 2] = unsafe { std::mem::transmute(v) };
    Ok((raw[0], raw[1]))
}

fn open_dir(path: &Path) -> Result<RawFd> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(fd)
}

fn init_fanotify() -> Result<RawFd> {
    let fd = unsafe {
        libc::fanotify_init(
            FAN_CLASS_NOTIF | FAN_REPORT_DFID_NAME_TARGET | FAN_CLOEXEC | FAN_NONBLOCK,
            (libc::O_RDONLY | libc::O_LARGEFILE) as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(fd)
}

fn mark_fs(fan: RawFd, root: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    // raw bytes, not to_string_lossy: a non-UTF-8 mountpoint must be marked at its
    // real path (matches open_dir/statfs_fsid), or its events are silently lost.
    let cpath = std::ffi::CString::new(root.as_os_str().as_bytes())?;
    let mask = FAN_CREATE
        | FAN_DELETE
        | FAN_MOVED_FROM
        | FAN_MOVED_TO
        | FAN_MODIFY
        | FAN_ATTRIB
        | FAN_CLOSE_WRITE
        | FAN_ONDIR;
    let rc = unsafe {
        libc::fanotify_mark(
            fan,
            FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
            mask,
            libc::AT_FDCWD,
            cpath.as_ptr(),
        )
    };
    if rc < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

/// Parse a buffer of fanotify events into pending ops keyed by full path.
/// Parse a buffer of fanotify events into `pending`. Returns (seen, resolved):
/// non-overflow events seen, and how many had their handle resolved to a path.
/// A run of `seen > 0, resolved == 0` is the signature of a missing
/// CAP_DAC_READ_SEARCH (open_by_handle_at EPERMs on every event) — see the caller.
/// Repeated notifications are coalesced by path. A CREATE followed by DELETE is
/// retained as a delete-state check at flush time: whether the name was present
/// in the committed index is not known without an extra SQLite lookup on the
/// event-capture path. The flush checks final filesystem state and either
/// upserts the surviving occupant or removes a stale indexed dirent.
#[cfg(test)]
fn ingest_event(
    pending: &mut HashMap<PathBuf, Op>,
    born: &mut std::collections::HashSet<PathBuf>,
    full: PathBuf,
    mask: u64,
) {
    ingest_event_with_indexed_path(pending, born, full, mask, false);
}

/// Fold an event while preserving destructive ordering. A true `may_be_indexed`
/// value avoids classifying an uncertain CREATE as new; the live capture path
/// uses this conservative mode because it no longer queries SQLite in the event
/// loop. CREATE and DELETE in one window retain a final-state delete check.
fn ingest_event_with_indexed_path(
    pending: &mut HashMap<PathBuf, Op>,
    born: &mut std::collections::HashSet<PathBuf>,
    full: PathBuf,
    mask: u64,
    may_be_indexed: bool,
) {
    // fanotify may merge several notifications for one object into a single
    // event mask. CREATE|DELETE therefore does not mean the path is absent: a
    // rapid replacement can leave a new occupant at the same name. Defer the
    // decision until flush, when the final filesystem state can be inspected.
    if mask & (FAN_CREATE | FAN_DELETE) == (FAN_CREATE | FAN_DELETE) {
        born.remove(&full);
        pending.insert(full, Op::Reconcile);
        return;
    }
    if mask & FAN_DELETE != 0 {
        // The path may already exist in the committed index despite this
        // window's CREATE (for example, a delayed event or a replacement).
        // The batched delete phase checks final disk state and becomes a
        // harmless no-op when this was a genuinely new transient path.
        born.remove(&full);
        pending.insert(full, Op::Delete);
    } else {
        let op = if mask & FAN_MOVED_FROM != 0 {
            Op::MovedFrom
        } else if mask & FAN_MOVED_TO != 0 {
            Op::MovedTo
        } else {
            Op::Upsert
        };
        tracing::debug!(
            path = %full.display(),
            mask = format_args!("{mask:#x}"),
            operation = ?op,
            previous = ?pending.get(&full).copied(),
            "fanotify event coalesced"
        );
        // Track paths known to be absent before this window. The live path uses
        // conservative mode and avoids retaining duplicate path keys here.
        if mask & FAN_CREATE != 0
            && !may_be_indexed
            && !matches!(pending.get(&full), Some(Op::Delete | Op::MovedFrom))
        {
            born.insert(full.clone());
        }
        // Metadata events after a directory move must not erase the need to
        // reconcile its descendants (they may not have individual events).
        if op == Op::Upsert && pending.get(&full) == Some(&Op::MovedTo) {
            return;
        }
        // Metadata notifications may be delayed relative to a rename/unlink and
        // still name the old pathname. Letting such an Upsert erase a pending
        // removal leaves a ghost dirent when the path is now absent. Preserve the
        // destructive intent unless a CREATE proves that this name was reused;
        // flush-time existence checks safely reconcile a replacement occupant.
        if op == Op::Upsert
            && mask & FAN_CREATE == 0
            && matches!(pending.get(&full), Some(Op::Delete | Op::MovedFrom))
        {
            return;
        }
        pending.insert(full, op);
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn parse_events(
    buf: &[u8],
    fsfds: &HashMap<(i32, i32), RawFd>,
    handle_capable_fsids: &mut HashSet<(i32, i32)>,
    root: &Path,
    db: &Path,
    pending: &mut HashMap<PathBuf, Op>,
    born: &mut std::collections::HashSet<PathBuf>,
    target_refreshes: &mut std::collections::HashSet<(i64, i64)>,
    indexed_parent_paths: &mut IndexedParentPathCache,
    store: &mut Store,
    coalesce: bool,
    lost_events: &mut bool,
) -> (u64, u64, u64) {
    let (seen, resolved, overflows, _) = parse_events_with_budget(
        buf,
        fsfds,
        handle_capable_fsids,
        root,
        db,
        pending,
        born,
        target_refreshes,
        indexed_parent_paths,
        store,
        coalesce,
        lost_events,
        usize::MAX,
        None,
    );
    (seen, resolved, overflows)
}

#[allow(clippy::too_many_arguments)]
fn parse_events_bounded(
    buf: &[u8],
    fsfds: &HashMap<(i32, i32), RawFd>,
    handle_capable_fsids: &mut HashSet<(i32, i32)>,
    root: &Path,
    db: &Path,
    pending: &mut HashMap<PathBuf, Op>,
    born: &mut std::collections::HashSet<PathBuf>,
    target_refreshes: &mut std::collections::HashSet<(i64, i64)>,
    indexed_parent_paths: &mut IndexedParentPathCache,
    store: &mut Store,
    coalesce: bool,
    lost_events: &mut bool,
    max_events: usize,
    deadline: Instant,
) -> (u64, u64, u64, usize) {
    parse_events_with_budget(
        buf,
        fsfds,
        handle_capable_fsids,
        root,
        db,
        pending,
        born,
        target_refreshes,
        indexed_parent_paths,
        store,
        coalesce,
        lost_events,
        max_events,
        Some(deadline),
    )
}

#[allow(clippy::too_many_arguments)]
fn parse_events_with_budget(
    mut buf: &[u8],
    fsfds: &HashMap<(i32, i32), RawFd>,
    handle_capable_fsids: &mut HashSet<(i32, i32)>,
    root: &Path,
    db: &Path,
    pending: &mut HashMap<PathBuf, Op>,
    born: &mut std::collections::HashSet<PathBuf>,
    target_refreshes: &mut std::collections::HashSet<(i64, i64)>,
    indexed_parent_paths: &mut IndexedParentPathCache,
    store: &mut Store,
    coalesce: bool,
    lost_events: &mut bool,
    max_events: usize,
    deadline: Option<Instant>,
) -> (u64, u64, u64, usize) {
    let input_len = buf.len();
    let (mut seen, mut resolved, mut overflow_notifications) = (0u64, 0u64, 0u64);
    let mut pulse = Instant::now();
    let mut consumed = 0usize;
    let mut parsed = 0usize;
    while buf.len() >= FAN_EVENT_METADATA_LEN && parsed < max_events {
        if deadline.is_some_and(|limit| Instant::now() >= limit) {
            break;
        }
        let meta: FanotifyEventMetadata =
            unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const FanotifyEventMetadata) };
        // a version mismatch means the struct layout differs from ours — bail
        // rather than misinterpret every subsequent offset.
        if meta.vers != FANOTIFY_METADATA_VERSION {
            *lost_events = true;
            consumed = input_len;
            break;
        }
        let len = meta.event_len as usize;
        if len < FAN_EVENT_METADATA_LEN || len > buf.len() {
            *lost_events = true;
            consumed = input_len;
            break;
        }
        if pulse.elapsed() >= Duration::from_secs(2) {
            crate::util::write_heartbeat(db);
            pulse = Instant::now();
        }
        let event = &buf[..len];

        if meta.mask & FAN_Q_OVERFLOW != 0 {
            // The bounded kernel queue overflowed — mark index dirty
            *lost_events = true;
            overflow_notifications = overflow_notifications.saturating_add(1);
            tracing::warn!("fanotify queue overflow — index may have missed events");
            if coalesce {
                persist_dirty_marker_best_effort(store, "fanotify queue overflow");
            }
        } else {
            seen += 1;
            let event_started = Instant::now();
            let resolve_started = Instant::now();
            let resolved_record = resolve_record(
                event,
                meta.metadata_len as usize,
                meta.mask,
                fsfds,
                handle_capable_fsids,
            );
            let resolve_elapsed = resolve_started.elapsed();
            let has_parent_name = resolved_record.entry.is_some();
            if event_identity_unresolved(&resolved_record)
                && self_generated_attrib_event(meta.mask, meta.pid)
            {
                // Our reconciliation reads do not change indexed metadata (atime
                // is not part of the index). Filesystem-wide fanotify marks can
                // also report transient metadata changes to our own SQLite and
                // staging files; an unopenable, self-generated ATTRIB FID must
                // not poison an otherwise exact reconciliation.
                tracing::debug!(
                    mask = format_args!("{:#x}", meta.mask),
                    event_pid = meta.pid,
                    "ignored unresolved self-generated FAN_ATTRIB event"
                );
            } else if stale_target_attrib_is_benign(
                meta.mask,
                &resolved_record,
                handle_capable_fsids,
            ) {
                tracing::debug!(
                    mask = format_args!("{:#x}", meta.mask),
                    "ignored stale target-only FAN_ATTRIB for deleted object"
                );
            } else if let Some(outcome) = match classify_stale_parent_event(
                &resolved_record,
                handle_capable_fsids,
                fsfds,
                &store.conn,
                indexed_parent_paths,
            ) {
                Ok(outcome) => outcome,
                Err(error) => {
                    // Fall through to the conservative lost-coverage branch.
                    tracing::warn!(%error, "could not classify stale-parent fanotify event");
                    None
                }
            } {
                match outcome {
                    StaleParentOutcome::RefreshIndexedPath(full) => {
                        if full.starts_with(root) && !crate::util::is_index_artifact(&full, db) {
                            // Decide from final disk state at flush, exactly like
                            // a merged CREATE|DELETE: never trust the event mask
                            // for a directory whose identity could not be opened.
                            tracing::debug!(
                                path = %full.display(),
                                mask = format_args!("{:#x}", meta.mask),
                                "stale parent handle mapped to indexed path; reconciling"
                            );
                            born.remove(&full);
                            pending.insert(full, Op::Reconcile);
                        }
                    }
                    StaleParentOutcome::ParentNotIndexed => {
                        tracing::debug!(
                            mask = format_args!("{:#x}", meta.mask),
                            "ignored event under freed, never-indexed parent directory"
                        );
                    }
                }
            } else if event_identity_unresolved(&resolved_record) {
                // Aggregate all-unresolved detection cannot catch one lost
                // notification in an otherwise healthy stream. A single event
                // without usable identity makes coverage uncertain.
                *lost_events = true;
                if let Some(suppressed_events) = unresolved_event_log_permit() {
                    tracing::warn!(
                        mask = format_args!("{:#x}", meta.mask),
                        info_type_mask = format_args!(
                            "{:#x}",
                            event_info_type_mask(event, meta.metadata_len as usize)
                        ),
                        target_detail = %unresolved_target_diagnostic(
                            event,
                            meta.metadata_len as usize,
                            fsfds
                        ),
                        event_pid = meta.pid,
                        self_generated = meta.pid == std::process::id() as i32,
                        metadata_len = meta.metadata_len,
                        event_len = meta.event_len,
                        suppressed_events,
                        "fanotify event has no resolvable parent/name or target identity"
                    );
                }
            }
            if let Some((dir, name)) = resolved_record.entry {
                resolved += 1;
                let full = if name.is_empty() || name.as_os_str() == std::ffi::OsStr::new(".") {
                    dir.clone()
                } else {
                    dir.join(&name)
                };
                if full.starts_with(root) && !crate::util::is_index_artifact(&full, db) {
                    if !coalesce {
                        born.remove(&full);
                    }
                    let event_mask = if coalesce {
                        meta.mask
                    } else {
                        meta.mask & !FAN_CREATE
                    };
                    ingest_event_with_indexed_path(
                        pending, born, full, event_mask,
                        // Avoid a SQLite B-tree probe for each new name and do
                        // not retain every CREATE path in the auxiliary born set.
                        // The batched writer validates deletes from final state.
                        true,
                    );
                }
            }
            // DFID_NAME is not present on every event type/kernel path even when
            // the fanotify group also reports a target FID. A target-only MODIFY,
            // ATTRIB, or CLOSE_WRITE still identifies the changed inode, so use
            // its indexed links to refresh allocation and rollups. DELETE always
            // needs this path as well: a hardlink alias may disappear before it
            // is ever flushed, while writes through it changed the surviving link.
            let mut target_refresh_queued = false;
            if let Some((dev, ino)) = resolved_record.target_inode {
                if needs_target_identity(meta.mask, has_parent_name)
                    && target_refreshes.insert((dev, ino))
                {
                    target_refresh_queued = true;
                    tracing::debug!(
                        dev,
                        inode = ino,
                        mask = format_args!("{:#x}", meta.mask),
                        "queued indexed-link refresh for target-only event"
                    );
                }
            }
            let event_elapsed = event_started.elapsed();
            if event_elapsed >= Duration::from_millis(100) {
                let now = now_secs().max(0) as u64;
                let previous = LAST_SLOW_EVENT_LOG_AT.load(Ordering::Relaxed);
                if now > previous
                    && LAST_SLOW_EVENT_LOG_AT
                        .compare_exchange(previous, now, Ordering::Relaxed, Ordering::Relaxed)
                        .is_ok()
                {
                    tracing::warn!(
                        mask = format_args!("{:#x}", meta.mask),
                        event_elapsed_ms = event_elapsed.as_millis(),
                        resolve_elapsed_ms = resolve_elapsed.as_millis(),
                        has_parent_name,
                        target_refresh_queued,
                        pending_updates = pending.len(),
                        "slow fanotify event resolution"
                    );
                }
            }
        }

        buf = &buf[len..];
        consumed += len;
        parsed += 1;
    }
    if consumed < input_len && buf.len() < FAN_EVENT_METADATA_LEN {
        // fanotify read() returns complete records. Preserve correctness if an
        // unexpected short tail ever appears instead of spinning forever on it.
        *lost_events = true;
        consumed = input_len;
    }
    (seen, resolved, overflow_notifications, consumed)
}

/// Find the DFID_NAME info record, resolve the directory via open_by_handle_at,
/// and return (dir_path, entry_name).
fn resolve_record(
    event: &[u8],
    meta_len: usize,
    mask: u64,
    fsfds: &HashMap<(i32, i32), RawFd>,
    handle_capable_fsids: &mut HashSet<(i32, i32)>,
) -> ResolvedEventRecord {
    // Info records begin AFTER the fixed metadata header. Floor the start offset
    // at the header size so a bogus small `metadata_len` can't make the scan begin
    // inside the header and misparse (memory-safe either way — every read below is
    // bounds-checked — but this keeps the parse honest).
    let mut off = meta_len.max(FAN_EVENT_METADATA_LEN);
    let mut entry_payload = None;
    let mut target_payload = None;
    let mut info_type_mask = 0u64;
    let mut info_record_count = 0usize;
    let mut info_records_complete = true;
    while off < event.len() {
        if off + 4 > event.len() {
            info_records_complete = false;
            break;
        }
        let hdr: InfoHeader =
            unsafe { std::ptr::read_unaligned(event[off..].as_ptr() as *const InfoHeader) };
        let rec_len = hdr.len as usize;
        if rec_len < 4 || off + rec_len > event.len() {
            info_records_complete = false;
            break;
        }
        info_record_count = info_record_count.saturating_add(1);
        if hdr.info_type < u64::BITS as u8 {
            info_type_mask |= 1u64 << hdr.info_type;
        }
        if hdr.info_type == FAN_EVENT_INFO_TYPE_DFID_NAME {
            entry_payload = Some(&event[off + 4..off + rec_len]); // fsid(8) + file_handle + name
        } else if hdr.info_type == FAN_EVENT_INFO_TYPE_FID {
            target_payload = Some(&event[off + 4..off + rec_len]);
        }
        off += rec_len;
    }
    info_records_complete &= off == event.len();
    // Most notifications carry both a parent/name record and a target FID.
    // The path event is sufficient to stat/upsert ordinary creates, renames and
    // metadata updates. Opening the target FID as well costs another
    // open_by_handle_at plus fstat on every such event. Keep target identity
    // resolution only for target-only metadata events and deletes, where the
    // named path may already be gone and surviving hardlinks still need refresh.
    let (entry, stale_parent) =
        match entry_payload.map(|payload| resolve_handle(payload, fsfds, handle_capable_fsids)) {
            Some(EntryResolution::Resolved(dir, name)) => (Some((dir, name)), None),
            Some(EntryResolution::Stale(parent)) => (None, Some(parent)),
            Some(EntryResolution::Unresolved) | None => (None, None),
        };
    let (target_inode, stale_target_fsid) = if needs_target_identity(mask, entry.is_some()) {
        match target_payload
            .map(|payload| resolve_handle_identity(payload, fsfds, handle_capable_fsids))
        {
            Some(TargetHandleResolution::Resolved(identity)) => (Some(identity), None),
            Some(TargetHandleResolution::Stale(fsid)) => (None, Some(fsid)),
            Some(TargetHandleResolution::Unresolved) | None => (None, None),
        }
    } else {
        (None, None)
    };
    ResolvedEventRecord {
        entry,
        target_inode,
        stale_target_fsid,
        stale_parent,
        info_type_mask,
        info_record_count,
        info_records_complete,
    }
}

fn needs_target_identity(mask: u64, has_parent_name: bool) -> bool {
    let metadata_event = mask & (FAN_MODIFY | FAN_ATTRIB | FAN_CLOSE_WRITE | FAN_DELETE) != 0;
    metadata_event && (!has_parent_name || mask & FAN_DELETE != 0)
}

fn take_target_refresh_batch(
    target_refreshes: &mut std::collections::HashSet<(i64, i64)>,
    limit: usize,
) -> Vec<(i64, i64)> {
    let batch: Vec<_> = target_refreshes.iter().take(limit).copied().collect();
    for id in &batch {
        target_refreshes.remove(id);
    }
    batch
}

fn materialize_target_refreshes(
    target_refreshes: &mut std::collections::HashSet<(i64, i64)>,
    pending: &mut HashMap<PathBuf, Op>,
    deferred_work: usize,
    store: &Store,
    root: &Path,
    parent_paths: &mut IndexedParentPathCache,
) -> Result<usize> {
    let batch = take_target_refresh_batch(target_refreshes, MAX_TARGET_REFRESHES_PER_CYCLE);
    let count = batch.len();
    for (index, (dev, ino)) in batch.iter().copied().enumerate() {
        let reserved = deferred_work
            .saturating_add(target_refreshes.len())
            .saturating_add(batch.len().saturating_sub(index + 1));
        let pending_budget = MAX_PENDING.saturating_sub(reserved);
        if let Err(error) =
            enqueue_inode_refreshes(pending, store, root, dev, ino, parent_paths, pending_budget)
        {
            // Keep later identities queued; the failed identity is covered by
            // the caller's uncertainty/reconciliation path.
            target_refreshes.extend(batch.iter().skip(index + 1).copied());
            return Err(error)
                .with_context(|| format!("refreshing indexed links for inode {dev}:{ino}"));
        }
    }
    Ok(count)
}

fn enqueue_inode_refreshes(
    pending: &mut HashMap<PathBuf, Op>,
    store: &Store,
    root: &Path,
    dev: i64,
    ino: i64,
    parent_paths: &mut IndexedParentPathCache,
    pending_budget: usize,
) -> Result<()> {
    use std::os::unix::ffi::OsStringExt;
    // Reuse parent paths across aliases and distinct inodes until a flush changes
    // the committed graph. Without this window-level cache, a write storm in one
    // directory repeats the same recursive SQLite path query once per inode.
    // Keyset pages bound temporary memory for inodes with very many hardlinks.
    let mut cursor = 0i64;
    loop {
        let links: Vec<(i64, i64, i64, Vec<u8>)> = store
            .conn
            .prepare(
                "SELECT rowid,parent_dev,parent_inode,name FROM dirents
                 WHERE dev_id=?1 AND inode=?2 AND rowid>?3
                   AND NOT(parent_dev=dev_id AND parent_inode=inode)
                 ORDER BY rowid LIMIT 256",
            )?
            .query_map(params![dev, ino, cursor], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        if links.is_empty() {
            break;
        }
        for (rowid, parent_dev, parent_ino, name) in links {
            cursor = rowid;
            let parent = parent_paths.resolve(&store.conn, parent_dev, parent_ino)?;
            let Some(parent) = parent else {
                continue;
            };
            let mut path = parent;
            path.push(std::ffi::OsString::from_vec(name));
            if path.starts_with(root) && std::fs::symlink_metadata(&path).is_ok() {
                if !pending.contains_key(&path) && pending.len() >= pending_budget {
                    anyhow::bail!(
                        "target inode {dev}:{ino} has more live links than the pending path budget"
                    );
                }
                pending
                    .entry(path)
                    .and_modify(|op| {
                        if *op == Op::Delete {
                            *op = Op::Upsert;
                        }
                    })
                    .or_insert(Op::Upsert);
            }
        }
    }
    Ok(())
}

enum EntryResolution {
    Resolved(PathBuf, std::ffi::OsString),
    Stale(StaleParentEntry),
    Unresolved,
}

fn resolve_handle(
    payload: &[u8],
    fsfds: &HashMap<(i32, i32), RawFd>,
    handle_capable_fsids: &mut HashSet<(i32, i32)>,
) -> EntryResolution {
    if payload.len() < 16 {
        return EntryResolution::Unresolved;
    }
    // payload: [fsid:8][handle_bytes:4][handle_type:4][f_handle:hb][name...]
    let Some(hb) = payload[8..12]
        .try_into()
        .ok()
        .map(|bytes| u32::from_ne_bytes(bytes) as usize)
    else {
        return EntryResolution::Unresolved;
    };
    if hb == 0 || hb > MAX_HANDLE_SZ || payload.len() < 16 + hb {
        return EntryResolution::Unresolved;
    }

    // name follows the handle, NUL-terminated
    let name_bytes = &payload[16 + hb..];
    let name_end = name_bytes
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(name_bytes.len());
    // Keep the name as raw bytes (OsString), NOT a lossy UTF-8 String: a filename
    // with non-UTF-8 bytes would otherwise be mangled into replacement chars and
    // never match its real dirent, silently dropping the event (permanent drift).
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::OsStr::from_bytes(&name_bytes[..name_end]).to_owned();

    let dfd = match open_handle(payload, fsfds, handle_capable_fsids) {
        Ok(fd) => fd,
        Err(HandleOpenError::Stale) => {
            let (Some(fsid), Ok(handle_type)) = (
                payload_fsid(payload),
                payload[12..16].try_into().map(i32::from_ne_bytes),
            ) else {
                return EntryResolution::Unresolved;
            };
            return EntryResolution::Stale(StaleParentEntry {
                fsid,
                handle_type,
                handle: payload[16..16 + hb].to_vec(),
                name,
            });
        }
        Err(HandleOpenError::Other) => return EntryResolution::Unresolved,
    };
    let dir = std::fs::read_link(format!("/proc/self/fd/{dfd}")).ok();
    unsafe {
        libc::close(dfd);
    }
    match dir {
        Some(dir) => EntryResolution::Resolved(dir, name),
        None => EntryResolution::Unresolved,
    }
}

enum TargetHandleResolution {
    Resolved((i64, i64)),
    Stale((i32, i32)),
    Unresolved,
}

fn resolve_handle_identity(
    payload: &[u8],
    fsfds: &HashMap<(i32, i32), RawFd>,
    handle_capable_fsids: &mut HashSet<(i32, i32)>,
) -> TargetHandleResolution {
    use std::os::fd::FromRawFd;
    let Some(fsid) = payload_fsid(payload) else {
        return TargetHandleResolution::Unresolved;
    };
    let fd = match open_handle(payload, fsfds, handle_capable_fsids) {
        Ok(fd) => fd,
        Err(HandleOpenError::Stale) => return TargetHandleResolution::Stale(fsid),
        Err(HandleOpenError::Other) => return TargetHandleResolution::Unresolved,
    };
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let Some(metadata) = file.metadata().ok() else {
        return TargetHandleResolution::Unresolved;
    };
    match crate::util::indexed_identity(&metadata) {
        Ok(identity) => TargetHandleResolution::Resolved(identity),
        Err(_) => TargetHandleResolution::Unresolved,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum HandleOpenError {
    Stale,
    Other,
}

fn payload_fsid(payload: &[u8]) -> Option<(i32, i32)> {
    if payload.len() < 16 {
        return None;
    }
    Some((
        i32::from_ne_bytes(payload[0..4].try_into().ok()?),
        i32::from_ne_bytes(payload[4..8].try_into().ok()?),
    ))
}

fn open_handle(
    payload: &[u8],
    fsfds: &HashMap<(i32, i32), RawFd>,
    handle_capable_fsids: &mut HashSet<(i32, i32)>,
) -> Result<RawFd, HandleOpenError> {
    let fsid = payload_fsid(payload).ok_or(HandleOpenError::Other)?;
    let mount_fd = *fsfds.get(&fsid).ok_or(HandleOpenError::Other)?;
    let hb = u32::from_ne_bytes(
        payload[8..12]
            .try_into()
            .map_err(|_| HandleOpenError::Other)?,
    ) as usize;
    let ht = i32::from_ne_bytes(
        payload[12..16]
            .try_into()
            .map_err(|_| HandleOpenError::Other)?,
    );
    if hb == 0 || hb > MAX_HANDLE_SZ || payload.len() < 16 + hb {
        return Err(HandleOpenError::Other);
    }
    let mut fh = FileHandle {
        handle_bytes: hb as u32,
        handle_type: ht,
        f_handle: [0u8; MAX_HANDLE_SZ],
    };
    fh.f_handle[..hb].copy_from_slice(&payload[16..16 + hb]);
    let fd = unsafe {
        libc::syscall(
            libc::SYS_open_by_handle_at,
            mount_fd,
            &mut fh as *mut FileHandle,
            libc::O_PATH | libc::O_CLOEXEC,
        )
    } as RawFd;
    if fd < 0 {
        return Err(classify_handle_open_error(
            std::io::Error::last_os_error().raw_os_error(),
        ));
    }
    handle_capable_fsids.insert(fsid);
    Ok(fd)
}

/// Probe name_to_handle_at/open_by_handle_at on one mounted directory before
/// queued fanotify events are consumed. This distinguishes a normal ESTALE
/// delete race from a filesystem that cannot reopen FIDs even when that stale
/// notification arrives first. Require the opened handle to resolve to the
/// same device/inode as the descriptor used for the probe.
fn probe_handle_capability(path: &Path, mount_fd: RawFd) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let mut handle = FileHandle {
        handle_bytes: MAX_HANDLE_SZ as u32,
        handle_type: 0,
        f_handle: [0; MAX_HANDLE_SZ],
    };
    let mut mount_id = 0;
    let named = unsafe {
        libc::name_to_handle_at(
            libc::AT_FDCWD,
            path.as_ptr(),
            (&mut handle as *mut FileHandle).cast::<libc::file_handle>(),
            &mut mount_id,
            0,
        )
    };
    if named != 0 || handle.handle_bytes == 0 || handle.handle_bytes as usize > MAX_HANDLE_SZ {
        return false;
    }
    let fd = unsafe {
        libc::syscall(
            libc::SYS_open_by_handle_at,
            mount_fd,
            &mut handle as *mut FileHandle,
            libc::O_PATH | libc::O_CLOEXEC,
        ) as RawFd
    };
    if fd < 0 {
        return false;
    }
    let probe_fd = RawFdGuard(fd);
    let mut expected: libc::stat = unsafe { std::mem::zeroed() };
    let mut resolved: libc::stat = unsafe { std::mem::zeroed() };
    unsafe {
        libc::fstat(mount_fd, &mut expected) == 0
            && libc::fstat(probe_fd.0, &mut resolved) == 0
            && expected.st_dev == resolved.st_dev
            && expected.st_ino == resolved.st_ino
    }
}

fn classify_handle_open_error(errno: Option<i32>) -> HandleOpenError {
    if errno == Some(libc::ESTALE) {
        HandleOpenError::Stale
    } else {
        HandleOpenError::Other
    }
}

/// Prime parent of an inode = the parent dir of its block-bearing (prime) dirent.
/// Directory totals roll up this single chain, so the walk is unambiguous.
fn prime_parent(tx: &rusqlite::Transaction, dev: i64, ino: i64) -> Result<Option<(i64, i64)>> {
    Ok(tx
        .query_row(
            "SELECT parent_dev, parent_inode FROM dirents
         WHERE dev_id=?1 AND inode=?2 AND prime=1 LIMIT 1",
            params![dev, ino],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?)
}

/// Transaction-scoped accounting and verified directory identities. A cached
/// directory is reused only while its current identity/allocation still match;
/// graph mutations invalidate the cache before moving or deleting entries.
#[derive(Default)]
struct AncestorChanges {
    deltas: HashMap<(i64, i64), (i64, i64)>,
    verified_dirs: HashMap<PathBuf, (i64, i64, i64, u64)>,
    parent_chains: HashMap<(i64, i64), Vec<(i64, i64)>>,
    force_exact: bool,
}
impl AncestorChanges {
    fn require_exact(&mut self) {
        self.force_exact = true;
    }

    fn invalidate_geometry(&mut self) {
        self.verified_dirs.clear();
        self.parent_chains.clear();
    }
}
impl std::ops::Deref for AncestorChanges {
    type Target = HashMap<(i64, i64), (i64, i64)>;
    fn deref(&self) -> &Self::Target {
        &self.deltas
    }
}
impl std::ops::DerefMut for AncestorChanges {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.deltas
    }
}

/// Accumulate a (bytes,inodes) delta into every ancestor inode, starting at
/// `(sdev,sino)` and walking prime parents. Coalesced in `anc` and written once
/// at the end of the flush. Depth-guarded against corrupt cycles.
fn accrue(
    tx: &rusqlite::Transaction,
    anc: &mut AncestorChanges,
    sdev: i64,
    sino: i64,
    dbytes: i64,
    dinodes: i64,
) -> Result<()> {
    let chain = if let Some(chain) = anc.parent_chains.get(&(sdev, sino)) {
        chain.clone()
    } else {
        let mut chain = Vec::new();
        let (mut cd, mut ci) = (sdev, sino);
        let mut seen = std::collections::HashSet::new();
        loop {
            anyhow::ensure!(seen.insert((cd, ci)), "cycle in accounting ancestor chain");
            anyhow::ensure!(
                seen.len() <= 4096,
                "accounting ancestor chain exceeds depth limit"
            );
            chain.push((cd, ci));
            match prime_parent(tx, cd, ci)? {
                Some((pd, pi)) if (pd, pi) != (cd, ci) => {
                    cd = pd;
                    ci = pi;
                }
                _ => break,
            }
        }
        anc.parent_chains.insert((sdev, sino), chain.clone());
        chain
    };
    for id in chain {
        let delta = anc.deltas.entry(id).or_insert((0, 0));
        delta.0 = delta
            .0
            .checked_add(dbytes)
            .context("ancestor byte delta overflow")?;
        delta.1 = delta
            .1
            .checked_add(dinodes)
            .context("ancestor inode delta overflow")?;
    }
    Ok(())
}

/// Re-materialize recursive totals for directories affected by this transaction.
///
/// The reducer uses coalesced deltas while applying operations because later
/// operations in the same batch need to account for earlier uncommitted changes.
/// At commit, however, the dirent graph is final. Recompute each affected
/// directory from its prime children, deepest first, so a rename/replay sequence
/// cannot leave equal-and-opposite attribution errors in sibling rollups.
const REPAIR_AFFECTED_LEAVES_SQL: &str =
    "UPDATE inodes SET recursive_bytes=blocks,recursive_inodes=1
    WHERE kind!='d' AND (recursive_bytes!=blocks OR recursive_inodes!=1)
      AND (dev_id,inode) IN (
        SELECT de.dev_id,de.inode FROM dirents de
        WHERE de.parent_dev=?1 AND de.parent_inode=?2 AND de.prime=1
      )";

fn reconcile_affected_rollups(tx: &rusqlite::Transaction, anc: &AncestorChanges) -> Result<()> {
    // Ordinary creates, deletes, and metadata changes already carry exact,
    // coalesced deltas through the prime-parent chain. Applying those deltas is
    // O(number of affected ancestors); recomputing each directory by summing
    // every child makes a small event batch O(directory fanout) and repeatedly
    // scans large database, log, and build directories. Keep the full graph
    // reconciliation for rename/reconcile/identity-reuse cases where repairing
    // an already-divergent local subtree is intentional.
    if !anc.force_exact {
        let mut needs_exact = false;
        for (&(dev, ino), &(bytes_delta, inodes_delta)) in &anc.deltas {
            if bytes_delta == 0 && inodes_delta == 0 {
                continue;
            }
            let current: Option<(i64, i64)> = tx
                .query_row(
                    "SELECT recursive_bytes,recursive_inodes FROM inodes
                     WHERE dev_id=?1 AND inode=?2 AND kind='d'",
                    params![dev, ino],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((current_bytes, current_inodes)) = current else {
                continue;
            };
            let Some(bytes) = current_bytes.checked_add(bytes_delta) else {
                needs_exact = true;
                continue;
            };
            let Some(inodes) = current_inodes.checked_add(inodes_delta) else {
                needs_exact = true;
                continue;
            };
            if bytes < 0 || inodes < 0 {
                needs_exact = true;
                continue;
            }
            tx.execute(
                "UPDATE inodes SET recursive_bytes=?3,recursive_inodes=?4
                 WHERE dev_id=?1 AND inode=?2 AND kind='d'",
                params![dev, ino, bytes, inodes],
            )?;
        }
        if !needs_exact {
            return Ok(());
        }
        // Never publish negative or overflowing recursive totals. A corrupt
        // prior generation or invalid delta falls back to the exact final graph
        // for affected directories in this same transaction.
        tracing::warn!(
            "incremental directory rollup is out of range; falling back to exact reconciliation"
        );
    }

    let mut affected = Vec::new();
    for &(dev, ino) in anc.keys() {
        let kind: Option<String> = tx
            .query_row(
                "SELECT kind FROM inodes WHERE dev_id=?1 AND inode=?2",
                params![dev, ino],
                |row| row.get(0),
            )
            .optional()?;
        if kind.as_deref() == Some("d") {
            affected.push((dev, ino));
        }
    }

    // A path-event reducer should keep every non-directory's cached recursive
    // size equal to its own allocation. A lost/coalesced write or a hardlink
    // promotion can leave that leaf value stale even when its directory rows
    // are otherwise structurally sound. Repair leaf values only beneath
    // directories already touched by this transaction; the deepest-first
    // directory recomputation below then propagates the corrected allocation.
    for &(dev, ino) in &affected {
        let repaired = tx.execute(REPAIR_AFFECTED_LEAVES_SQL, params![dev, ino])?;
        if repaired > 0 {
            tracing::warn!(
                parent_dev = dev,
                parent_inode = ino,
                repaired_leaf_inodes = repaired,
                "repaired stale leaf allocation totals during rollup reconciliation"
            );
        }
    }

    // Resolve depths through the current prime-parent graph. Directory moves
    // invalidate the reducer's cached geometry, so this walk reflects the final
    // transactional namespace. A self-parent root terminates each mount tree.
    let mut depths = HashMap::<(i64, i64), usize>::new();
    for start in affected.iter().copied() {
        if depths.contains_key(&start) {
            continue;
        }
        let mut chain = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut current = start;
        let mut base_depth = 0usize;
        for _ in 0..4096 {
            if let Some(depth) = depths.get(&current).copied() {
                base_depth = depth;
                break;
            }
            if !seen.insert(current) {
                anyhow::bail!("directory parent cycle while reconciling rollups");
            }
            chain.push(current);
            match prime_parent(tx, current.0, current.1)? {
                Some(parent) if parent != current => current = parent,
                _ => {
                    // The root contributes its own allocation and is the
                    // shallowest affected directory in this chain.
                    base_depth = 0;
                    chain.pop();
                    depths.insert(current, 0);
                    break;
                }
            }
        }
        anyhow::ensure!(
            chain.len() < 4096,
            "directory depth exceeds rollup repair limit"
        );
        for id in chain.into_iter().rev() {
            base_depth = base_depth.saturating_add(1);
            depths.insert(id, base_depth);
        }
    }
    affected.sort_unstable_by_key(|id| std::cmp::Reverse(depths.get(id).copied().unwrap_or(0)));

    for (dev, ino) in affected {
        let expected: Option<(i64, i64)> = tx
            .query_row(
                "SELECT d.blocks+COALESCE(SUM(ch.recursive_bytes),0),
                        1+COALESCE(SUM(ch.recursive_inodes),0)
                 FROM inodes d LEFT JOIN dirents de
                   ON de.parent_dev=d.dev_id AND de.parent_inode=d.inode AND de.prime=1
                  AND NOT(de.dev_id=d.dev_id AND de.inode=d.inode)
                 LEFT JOIN inodes ch ON ch.dev_id=de.dev_id AND ch.inode=de.inode
                 WHERE d.dev_id=?1 AND d.inode=?2 AND d.kind='d'
                 GROUP BY d.dev_id,d.inode,d.blocks",
                params![dev, ino],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((bytes, inodes)) = expected {
            tx.execute(
                "UPDATE inodes SET recursive_bytes=?3,recursive_inodes=?4
                 WHERE dev_id=?1 AND inode=?2
                   AND (recursive_bytes!=?3 OR recursive_inodes!=?4)",
                params![dev, ino, bytes, inodes],
            )?;
        }
    }
    Ok(())
}

/// Delete a whole subtree: every directory entry within it, and every inode whose
/// LAST link was inside it. An inode that is also hardlinked OUTSIDE the subtree
/// SURVIVES (the file still exists via the external link) — if its block-bearing
/// (prime) link was the internal one, an external link is promoted to prime and
/// the blocks re-attributed to the external parent chain (B8). FTS rows follow via
/// the AFTER DELETE trigger.
///
/// The descendant set is staged in a temp table once, then deleted SET-BASED (a
/// few statements) instead of ~3 executes per descendant — a million-entry dir is
/// a short transaction, not millions of round-trips. The walk must run BEFORE any
/// dirent is deleted (it follows dirent parent links), hence the staging table.
fn del_subtree(
    tx: &rusqlite::Transaction,
    anc: &mut AncestorChanges,
    d: i64,
    i: i64,
) -> Result<()> {
    tx.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS _delset(d INTEGER, i INTEGER, PRIMARY KEY(d,i)) WITHOUT ROWID;
         DELETE FROM _delset;",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO _delset(d,i)
         WITH RECURSIVE sub(d,i) AS (
            SELECT ?1,?2
            UNION
            SELECT de.dev_id,de.inode FROM dirents de
            JOIN sub ON de.parent_dev=sub.d AND de.parent_inode=sub.i
            WHERE NOT (de.dev_id=de.parent_dev AND de.inode=de.parent_inode)
         ) SELECT d,i FROM sub",
        params![d, i],
    )?;

    // RARE: descendant inodes hardlinked OUTSIDE the subtree must survive. Find
    // them (a dirent whose parent is NOT in the set); if their prime link is
    // internal (about to be deleted), promote an external link to prime and
    // re-attribute the blocks to the external chain — the caller subtracts the
    // whole subtree total, which counted these blocks under the deleted dir.
    let mut st = tx.prepare(
        "SELECT DISTINCT de.dev_id, de.inode FROM dirents de
         WHERE (de.dev_id,de.inode) IN (SELECT d,i FROM _delset)
           AND (de.parent_dev,de.parent_inode) NOT IN (SELECT d,i FROM _delset)",
    )?;
    let survivors: Vec<(i64, i64)> = st
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(st);
    for (sd, si) in survivors {
        // is the current prime (block-bearing) dirent internal to the subtree?
        let prime_internal = tx
            .query_row(
                "SELECT 1 FROM dirents WHERE dev_id=?1 AND inode=?2 AND prime=1
                   AND (parent_dev,parent_inode) IN (SELECT d,i FROM _delset) LIMIT 1",
                params![sd, si],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !prime_internal {
            continue; // prime already external — blocks already counted there
        }
        let (opdev, opino, oname) = tx
            .query_row(
                "SELECT parent_dev, parent_inode, name FROM dirents
             WHERE dev_id=?1 AND inode=?2
               AND (parent_dev,parent_inode) NOT IN (SELECT d,i FROM _delset)
             ORDER BY rowid LIMIT 1",
                params![sd, si],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Vec<u8>>(2)?,
                    ))
                },
            )
            .optional()?
            .context("surviving inode is missing its external hardlink")?;
        tx.execute(
            "UPDATE dirents SET prime=1 WHERE parent_dev=?1 AND parent_inode=?2 AND name=?3",
            params![opdev, opino, &oname],
        )?;
        let (rb, ri): (i64, i64) = tx.query_row(
            "SELECT recursive_bytes, recursive_inodes FROM inodes WHERE dev_id=?1 AND inode=?2",
            params![sd, si],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let adj = anc.get(&(sd, si)).copied().unwrap_or((0, 0));
        let bytes = rb
            .checked_add(adj.0)
            .context("surviving inode byte total overflow")?;
        let inodes = ri
            .checked_add(adj.1)
            .context("surviving inode count overflow")?;
        accrue(tx, anc, opdev, opino, bytes, inodes)?;
    }

    // Delete the directory ENTRIES within the subtree, then delete only inodes
    // that NO LONGER have any dirent (survivors keep their external link).
    tx.execute_batch(
        "DELETE FROM dirents WHERE (parent_dev,parent_inode) IN (SELECT d,i FROM _delset);
         DELETE FROM inodes WHERE (dev_id,inode) IN (SELECT d,i FROM _delset)
            AND NOT EXISTS (SELECT 1 FROM dirents de WHERE de.dev_id=inodes.dev_id AND de.inode=inodes.inode);
         DELETE FROM _delset;",
    )?;
    Ok(())
}

/// Remove one directory entry (a single link/path). Handles the three cases:
/// last link → drop the inode + its subtree; prime link with others remaining →
/// promote a sibling dirent and move the block attribution; non-prime link →
/// just unlink. Returns the (bytes,inodes) the subtree carried, for growth.
fn unlink_dirent(
    tx: &rusqlite::Transaction,
    anc: &mut AncestorChanges,
    bucket: i64,
    pdev: i64,
    pino: i64,
    name: &[u8],
) -> Result<()> {
    anc.invalidate_geometry();
    let de: Option<(i64, i64, i64)> = tx
        .query_row(
            "SELECT dev_id, inode, prime FROM dirents
             WHERE parent_dev=?1 AND parent_inode=?2 AND name=?3 LIMIT 1",
            params![pdev, pino, name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (cdev, cino, was_prime) = match de {
        Some(v) => v,
        None => return Ok(()),
    };
    let (kind, crb, cri): (String, i64, i64) = tx.query_row(
        "SELECT kind,recursive_bytes,recursive_inodes FROM inodes WHERE dev_id=?1 AND inode=?2",
        params![cdev, cino],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let adj = anc.get(&(cdev, cino)).copied().unwrap_or((0, 0));
    let (eff_rb, eff_ri) = if kind == "d" {
        // Cached recursive totals can be stale when deleting a directory.
        // Derive the removed allocation from its prime-attributed inode set;
        // the set-based subtree delete below uses the same namespace graph.
        tx.query_row(
            "WITH RECURSIVE subtree(dev_id,inode) AS (
                SELECT ?1,?2
                UNION
                SELECT de.dev_id,de.inode FROM dirents de
                JOIN subtree parent
                  ON de.parent_dev=parent.dev_id AND de.parent_inode=parent.inode
                WHERE de.prime=1
                  AND NOT (de.dev_id=de.parent_dev AND de.inode=de.parent_inode)
             )
             SELECT COALESCE(SUM(i.blocks),0),COUNT(*) FROM subtree s
             JOIN inodes i ON i.dev_id=s.dev_id AND i.inode=s.inode",
            params![cdev, cino],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        )?
    } else {
        (
            crb.checked_add(adj.0)
                .context("unlink byte total overflow")?,
            cri.checked_add(adj.1)
                .context("unlink inode count overflow")?,
        )
    };

    tx.execute(
        "DELETE FROM dirents WHERE parent_dev=?1 AND parent_inode=?2 AND name=?3",
        params![pdev, pino, name],
    )?;
    let remaining: i64 = tx.query_row(
        "SELECT COUNT(*) FROM dirents WHERE dev_id=?1 AND inode=?2",
        params![cdev, cino],
        |r| r.get(0),
    )?;

    if remaining == 0 {
        // last link gone: remove the inode (and any subtree) and subtract totals
        // from this (prime) parent chain.
        let birth_ns: i64 = tx.query_row(
            "SELECT birth_ns FROM inodes WHERE dev_id=?1 AND inode=?2",
            params![cdev, cino],
            |row| row.get(0),
        )?;
        del_subtree(tx, anc, cdev, cino)?;
        let remove_bytes = eff_rb.checked_neg().context("unlink byte delta overflow")?;
        let remove_inodes = eff_ri
            .checked_neg()
            .context("unlink inode delta overflow")?;
        accrue(tx, anc, pdev, pino, remove_bytes, remove_inodes)?;
        if eff_rb != 0 && birth_ns != 0 {
            gro(tx, bucket, cdev, cino, birth_ns, remove_bytes)?;
        }
    } else if was_prime != 0 {
        // the block-bearing link was removed but the inode lives on through another
        // link: promote a sibling dirent and MOVE the attribution to its chain.
        let (opdev, opino, oname) = tx
            .query_row(
                "SELECT parent_dev, parent_inode, name FROM dirents
                 WHERE dev_id=?1 AND inode=?2 ORDER BY rowid LIMIT 1",
                params![cdev, cino],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Vec<u8>>(2)?,
                    ))
                },
            )
            .optional()?
            .context("prime hardlink has a positive link count but no surviving sibling")?;
        tx.execute(
            "UPDATE dirents SET prime=1 WHERE parent_dev=?1 AND parent_inode=?2 AND name=?3",
            params![opdev, opino, oname],
        )?;
        let remove_bytes = eff_rb.checked_neg().context("relink byte delta overflow")?;
        let remove_inodes = eff_ri
            .checked_neg()
            .context("relink inode delta overflow")?;
        accrue(tx, anc, pdev, pino, remove_bytes, remove_inodes)?;
        accrue(tx, anc, opdev, opino, eff_rb, eff_ri)?;
    }
    // non-prime link with others remaining: the dirent delete above is all there is.
    Ok(())
}

/// Growth-history bucket upsert (delta of allocated blocks for an inode).
fn gro(
    tx: &rusqlite::Transaction,
    bucket: i64,
    dev: i64,
    ino: i64,
    birth_ns: i64,
    delta: i64,
) -> Result<()> {
    if delta == 0 {
        return Ok(());
    }
    // Without birth evidence we cannot tell a reused inode from the old object.
    // Suppress history rather than attribute writes to the wrong file.
    if birth_ns == 0 {
        return Ok(());
    }
    let changed = tx.execute(
        "INSERT INTO growth(bucket,dev_id,inode,birth_ns,delta) VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(bucket,dev_id,inode,birth_ns) DO UPDATE SET delta=delta+excluded.delta
         WHERE typeof(delta+excluded.delta)='integer'",
        params![bucket, dev, ino, birth_ns, delta],
    )?;
    anyhow::ensure!(
        changed == 1,
        "growth delta exceeds the SQLite integer range"
    );
    Ok(())
}

/// True if making (npdev,npino) the parent of inode (dev,ino) would create a
/// cycle (the proposed new parent is at or under the node being moved).
fn would_cycle(
    tx: &rusqlite::Transaction,
    dev: i64,
    ino: i64,
    npdev: i64,
    npino: i64,
) -> Result<bool> {
    let (mut cd, mut ci) = (npdev, npino);
    let mut g = 0;
    loop {
        if cd == dev && ci == ino {
            return Ok(true);
        }
        g += 1;
        if g > 4096 {
            anyhow::bail!("directory parent chain exceeds cycle check limit");
        }
        match prime_parent(tx, cd, ci)? {
            Some((pd, pi)) if !(pd == cd && pi == ci) => {
                cd = pd;
                ci = pi;
            }
            _ => return Ok(false),
        }
    }
}

/// Create / modify / hardlink a single path, maintaining both tables with prime-
/// dirent block attribution (an inode's blocks are counted exactly once).
#[allow(clippy::too_many_arguments)]
fn upsert_path(
    tx: &rusqlite::Transaction,
    anc: &mut AncestorChanges,
    bucket: i64,
    m: &std::fs::Metadata,
    dev: i64,
    ino: i64,
    pdev: i64,
    pino: i64,
    name: &[u8],
    birth_ns: i64,
) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    anyhow::ensure!(
        crate::util::indexed_identity(m)? == (dev, ino),
        "event metadata identity does not match its indexed key"
    );
    let blocks = crate::util::allocated_bytes(m).context("reading allocated inode size")?;
    let mtime = m.mtime();
    let uid = m.uid() as i64;
    let is_dir = m.is_dir();
    let kind = kind_of(m);
    let previous_incarnation: Option<(String, i64)> = tx
        .query_row(
            "SELECT kind,birth_ns FROM inodes WHERE dev_id=?1 AND inode=?2",
            params![dev, ino],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if previous_incarnation
        .as_ref()
        .is_some_and(|(old_kind, old_birth)| {
            old_kind != kind || (*old_birth != 0 && birth_ns != 0 && *old_birth != birth_ns)
        })
    {
        anc.invalidate_geometry();
        anc.require_exact();
        // The kernel can reuse an inode before its delayed delete is flushed.
        // All old links and recursive accounting belong to the dead incarnation,
        // not to this file. Unlink transactionally through the ordinary accounting
        // path, including external hardlinks surviving an old directory subtree.
        loop {
            let link: Option<(i64, i64, Vec<u8>)> = tx.query_row(
                "SELECT parent_dev,parent_inode,name FROM dirents WHERE dev_id=?1 AND inode=?2 LIMIT 1",
                params![dev, ino], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .optional()?;
            let Some((parent_dev, parent_inode, old_name)) = link else {
                break;
            };
            anyhow::ensure!(
                (parent_dev, parent_inode) != (dev, ino),
                "indexed root incarnation changed; full reconciliation required"
            );
            unlink_dirent(tx, anc, bucket, parent_dev, parent_inode, &old_name)?;
        }
        anc.remove(&(dev, ino));
    }

    let existing: Option<(i64, i64)> = tx
        .query_row(
            "SELECT dev_id, inode FROM dirents
             WHERE parent_dev=?1 AND parent_inode=?2 AND name=?3 LIMIT 1",
            params![pdev, pino, name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match existing {
        Some((edev, eino)) if edev == dev && eino == ino => {
            // same inode at the same path: a metadata/size change
            let (eb, erb): (i64, i64) = tx.query_row(
                "SELECT blocks, recursive_bytes FROM inodes WHERE dev_id=?1 AND inode=?2",
                params![dev, ino],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if is_dir {
                // a directory's OWN allocation can change (entries added/removed);
                // that delta flows into its recursive_bytes and its ancestors'.
                let delta = blocks
                    .checked_sub(eb)
                    .context("directory size delta overflow")?;
                tx.execute(
                    "UPDATE inodes SET blocks=?3, uid=?4, mtime=?5,
                     birth_ns=CASE WHEN birth_ns=0 THEN ?6 ELSE birth_ns END
                     WHERE dev_id=?1 AND inode=?2",
                    params![dev, ino, blocks, uid, mtime, birth_ns],
                )?;
                if delta != 0 {
                    accrue(tx, anc, dev, ino, delta, 0)?;
                    gro(tx, bucket, dev, ino, birth_ns, delta)?;
                }
            } else {
                let delta = blocks
                    .checked_sub(erb)
                    .context("file size delta overflow")?;
                tx.execute(
                    "UPDATE inodes SET blocks=?3, recursive_bytes=?3, uid=?4, mtime=?5,
                     birth_ns=CASE WHEN birth_ns=0 THEN ?6 ELSE birth_ns END
                     WHERE dev_id=?1 AND inode=?2",
                    params![dev, ino, blocks, uid, mtime, birth_ns],
                )?;
                if delta != 0 {
                    // Route the delta up the PRIME chain (the block-bearing link),
                    // NOT the event's parent: a hardlinked file modified via a
                    // NON-prime link would otherwise credit the wrong ancestors and,
                    // when its last link is later removed, `unlink_dirent` subtracts
                    // the grown recursive_bytes from the prime chain that never
                    // received the delta — driving those totals negative (H2).
                    let (ppd, ppi) = prime_parent(tx, dev, ino)?
                        .context("modified indexed inode has no prime dirent")?;
                    accrue(tx, anc, ppd, ppi, delta, 0)?;
                    gro(tx, bucket, dev, ino, birth_ns, delta)?;
                }
            }
        }
        other => {
            if other.is_some() {
                // a DIFFERENT inode currently occupies this path (a missed delete):
                // unlink the stale occupant before installing the new one.
                unlink_dirent(tx, anc, bucket, pdev, pino, name)?;
            }
            let inode_exists = tx
                .query_row(
                    "SELECT 1 FROM inodes WHERE dev_id=?1 AND inode=?2",
                    params![dev, ino],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if inode_exists && is_dir {
                anc.invalidate_geometry();
                anc.require_exact();
                // Directories have one location. A delayed TO (or a descendant
                // processed before its parent's rename) must relocate that
                // location, never create a non-prime directory alias.
                anyhow::ensure!(
                    !would_cycle(tx, dev, ino, pdev, pino)?,
                    "directory move would create a cycle"
                );
                let (od, oi) =
                    prime_parent(tx, dev, ino)?.context("directory has no prime parent")?;
                let (rb, ri): (i64, i64) = tx.query_row(
                    "SELECT recursive_bytes,recursive_inodes FROM inodes WHERE dev_id=?1 AND inode=?2",
                    params![dev, ino], |r| Ok((r.get(0)?,r.get(1)?)))?;
                let adj = anc.get(&(dev, ino)).copied().unwrap_or_default();
                tx.execute("UPDATE dirents SET parent_dev=?3,parent_inode=?4,name=?5 WHERE dev_id=?1 AND inode=?2 AND prime=1",
                    params![dev, ino, pdev, pino, name])?;
                let bytes = rb
                    .checked_add(adj.0)
                    .context("moved subtree byte total overflow")?;
                let inodes = ri
                    .checked_add(adj.1)
                    .context("moved subtree inode count overflow")?;
                accrue(
                    tx,
                    anc,
                    od,
                    oi,
                    bytes
                        .checked_neg()
                        .context("moved subtree byte delta overflow")?,
                    inodes
                        .checked_neg()
                        .context("moved subtree inode delta overflow")?,
                )?;
                accrue(tx, anc, pdev, pino, bytes, inodes)?;
                sync_directory(tx, anc, bucket, m)?;
            } else if inode_exists {
                // additional HARDLINK: record the path (searchable) but count the
                // inode's blocks only once -> a non-prime dirent, no attribution.
                tx.execute(
                    "INSERT OR REPLACE INTO dirents
                     (parent_dev,parent_inode,name,dev_id,inode,prime) VALUES(?1,?2,?3,?4,?5,0)",
                    params![pdev, pino, name, dev, ino],
                )?;
                upsert_path(tx, anc, bucket, m, dev, ino, pdev, pino, name, birth_ns)?;
            } else {
                // fresh inode + its first (prime) dirent
                // Deferred deltas for a removed directory must not leak into a
                // new inode reusing its identity within the same transaction.
                anc.remove(&(dev, ino));
                tx.execute(
                    "INSERT OR REPLACE INTO inodes
                     (dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime,birth_ns)
                     VALUES(?1,?2,?3,?4,?5,1,?6,?7,?8)",
                    params![dev, ino, kind, blocks, blocks, uid, mtime, birth_ns],
                )?;
                tx.execute(
                    "INSERT OR REPLACE INTO dirents
                     (parent_dev,parent_inode,name,dev_id,inode,prime) VALUES(?1,?2,?3,?4,?5,1)",
                    params![pdev, pino, name, dev, ino],
                )?;
                accrue(tx, anc, pdev, pino, blocks, 1)?;
                gro(tx, bucket, dev, ino, birth_ns, blocks)?;
            }
        }
    }
    Ok(())
}

/// Index the existing contents of a directory that was moved INTO the tree — its
/// descendants produced no per-file events, so a plain move-to would leave the
/// subtree empty. Bounded breadth-first walk; marks the index dirty if the entry
/// budget is exhausted (rather than silently indexing a partial subtree).
fn incomplete_reconcile(tx: &rusqlite::Transaction, reason: &str) -> Result<()> {
    tx.execute("INSERT INTO meta(key,value) VALUES('dirty_since',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![now_secs().to_string()])?;
    tx.execute("INSERT INTO meta(key,value) VALUES('dirty_reason',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![reason])?;
    Ok(())
}

fn reconcile_subtree(
    tx: &rusqlite::Transaction,
    anc: &mut AncestorChanges,
    bucket: i64,
    dir_path: &Path,
    dev: i64,
    ino: i64,
) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let mut queue: Vec<(PathBuf, i64, i64)> = vec![(dir_path.to_path_buf(), dev, ino)];
    let mut budget = 50_000usize;
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut visited = std::collections::HashSet::new();
    while let Some((dir, ddev, dino)) = queue.pop() {
        if !visited.insert((ddev, dino)) {
            continue;
        }
        // The directory may be renamed again while a moved-in subtree is being
        // reconciled. Its inode remains indexed under the new path and the
        // corresponding move event will reconcile that location; treating the
        // vanished old pathname as unreadable coverage incorrectly dirties the
        // whole index and forces a root rebuild.
        match std::fs::symlink_metadata(&dir) {
            Ok(metadata) if metadata.is_dir() => {
                let actual = crate::util::indexed_identity(&metadata)
                    .context("subtree directory identity cannot be represented by the index")?;
                if actual != (ddev, dino) {
                    continue; // the queued pathname now names another object
                }
            }
            Ok(_) => continue, // the queued pathname now names another object
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                return incomplete_reconcile(
                    tx,
                    "directory unreadable during subtree reconciliation",
                )
            }
        }
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotADirectory => continue,
            Err(_) => {
                return incomplete_reconcile(
                    tx,
                    "directory unreadable during subtree reconciliation",
                )
            }
        };
        // Deletes queued under the old directory name cannot be resolved after
        // its rename. Prune missing indexed names against the current location,
        // in bounded chunks, before adding the live directory contents.
        let mut cursor = 0i64;
        loop {
            let chunk: Vec<(i64,Vec<u8>)> = tx.prepare(
                "SELECT rowid,name FROM dirents WHERE parent_dev=?1 AND parent_inode=?2 AND rowid>?3 ORDER BY rowid LIMIT 256")?
                .query_map(params![ddev,dino,cursor],|row|Ok((row.get(0)?,row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            if chunk.is_empty() {
                break;
            }
            for (rowid, name) in chunk {
                if budget == 0 || Instant::now() >= deadline {
                    return incomplete_reconcile(tx, "subtree reconciliation budget exhausted");
                }
                budget -= 1;
                cursor = rowid;
                match std::fs::symlink_metadata(dir.join(std::ffi::OsStr::from_bytes(&name))) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        unlink_dirent(tx, anc, bucket, ddev, dino, &name)?
                    }
                    Err(_) => {
                        return incomplete_reconcile(
                            tx,
                            "indexed entry unreadable during subtree reconciliation",
                        )
                    }
                    Ok(_) => {}
                }
            }
        }
        for ent in rd {
            let ent = match ent {
                Ok(ent) => ent,
                Err(_) => {
                    return incomplete_reconcile(
                        tx,
                        "directory entry unreadable during subtree reconciliation",
                    )
                }
            };
            if budget == 0 || Instant::now() >= deadline {
                tracing::warn!(
                    "reconcile_subtree: entry budget exhausted at {}",
                    dir.display()
                );
                return incomplete_reconcile(tx, "subtree reconciliation budget exhausted");
            }
            budget -= 1;
            let p = ent.path();
            let m = match std::fs::symlink_metadata(&p) {
                Ok(m) => m,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    return incomplete_reconcile(
                        tx,
                        "entry metadata unreadable during subtree reconciliation",
                    )
                }
            };
            let (cdev, cino) = crate::util::indexed_identity(&m)
                .context("subtree entry identity cannot be represented by the index")?;
            let name = ent.file_name().as_bytes().to_vec();
            upsert_path(
                tx,
                anc,
                bucket,
                &m,
                cdev,
                cino,
                ddev,
                dino,
                &name,
                crate::util::inode_birth_ns_at(&m, &p),
            )?;
            if m.is_dir()
                && !crate::scan::is_pseudo_fs(&p)
                && !crate::scan::is_duplicate_storage_view(&p)
            {
                queue.push((p, cdev, cino));
            }
        }
    }
    Ok(())
}

/// Apply all pending ops in one transaction, in correctness-preserving phases:
///   B. deletes (deep-first)            — unlink path; drop inode only on last link
///   C. move-from                       — record renames; else unlink (left tree)
///   D. move-to                         — relocate the renamed dirent (keep subtree),
///                                        else add a hardlink path / fresh inode
///   E. upserts (shallow-first)         — create / modify / hardlink
///
/// The whole transaction is ATOMIC: any operation error aborts it via `?`, so
/// `pending` is left intact and retried next flush — a single failed op never
/// commits partial drift.
/// Retain ownership of every event until its transaction commits. A failed
/// batch is restored verbatim; no transient write failure can discard work.
#[allow(clippy::too_many_arguments)]
fn flush_batch(
    store: &mut Store,
    pending: &mut HashMap<PathBuf, Op>,
    born: &mut std::collections::HashSet<PathBuf>,
    deferred: &mut HashMap<(i64, i64), DeferredFrom>,
    db: &Path,
    growth_keep_secs: i64,
    limit: usize,
    kernel_empty: bool,
) -> Result<()> {
    flush_batch_inner(
        store,
        pending,
        born,
        deferred,
        db,
        growth_keep_secs,
        limit,
        kernel_empty,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn flush_batch_with_progress(
    store: &mut Store,
    pending: &mut HashMap<PathBuf, Op>,
    born: &mut std::collections::HashSet<PathBuf>,
    deferred: &mut HashMap<(i64, i64), DeferredFrom>,
    db: &Path,
    growth_keep_secs: i64,
    limit: usize,
    kernel_empty: bool,
    progress: FlushProgress,
) -> Result<()> {
    flush_batch_inner(
        store,
        pending,
        born,
        deferred,
        db,
        growth_keep_secs,
        limit,
        kernel_empty,
        Some(progress),
    )
}

#[allow(clippy::too_many_arguments)]
fn flush_batch_inner(
    store: &mut Store,
    pending: &mut HashMap<PathBuf, Op>,
    born: &mut std::collections::HashSet<PathBuf>,
    deferred: &mut HashMap<(i64, i64), DeferredFrom>,
    db: &Path,
    growth_keep_secs: i64,
    limit: usize,
    kernel_empty: bool,
    progress: Option<FlushProgress>,
) -> Result<()> {
    let keys: Vec<_> = pending.keys().take(limit).cloned().collect();
    let mut batch = HashMap::with_capacity(keys.len());
    for k in &keys {
        batch.insert(k.clone(), pending.remove(k).unwrap());
    }
    match flush_transaction(
        store,
        &mut batch,
        deferred,
        db,
        FlushContext {
            growth_keep_secs,
            may_expire: kernel_empty && pending.is_empty(),
            progress,
            pending_after_batch: pending.len() + deferred.len(),
        },
    ) {
        Ok(()) => {
            // A subtree reconciliation may have indexed children still waiting
            // in `pending`. After any committed batch none of those paths can
            // safely be assumed absent from the database for delete cancellation.
            born.clear();
            Ok(())
        }
        Err(e) => {
            pending.extend(batch);
            Err(e)
        }
    }
}

/// Resolve missing ancestor directories shallow-first inside this transaction.
/// Batch ordering alone is insufficient: a parent's event can be in another
/// batch, or its create can have coalesced with a later rename.
fn ensure_ancestors(
    tx: &rusqlite::Transaction,
    anc: &mut AncestorChanges,
    bucket: i64,
    dir: &Path,
) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let mut missing = Vec::new();
    // Capture parent identities during the same shallow-first walk. Re-statting
    // these paths while installing ancestors races with a concurrent directory
    // rename and can roll back an otherwise valid fanotify batch.
    let mut identities = HashMap::new();
    let mut current = dir;
    loop {
        let m = match std::fs::symlink_metadata(current) {
            Ok(m) if m.is_dir() => m,
            Ok(_) => return Ok(false), // the event's parent path was replaced
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(false); // a concurrent move/delete superseded this path
            }
            Err(error) => return Err(error.into()),
        };
        let id = crate::util::indexed_identity(&m)
            .context("ancestor identity cannot be represented by the index")?;
        identities.insert(current.to_path_buf(), id);
        let signature = (
            id.0,
            id.1,
            crate::util::inode_birth_ns_at(&m, current),
            m.blocks(),
        );
        if anc.verified_dirs.get(current) == Some(&signature) {
            break;
        }
        let present = tx.query_row(
            "SELECT birth_ns,EXISTS(SELECT 1 FROM dirents d WHERE d.dev_id=i.dev_id AND d.inode=i.inode AND d.parent_dev=i.dev_id AND d.parent_inode=i.inode)
             FROM inodes i WHERE dev_id=?1 AND inode=?2 AND kind='d'",
            params![id.0, id.1],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?)),
        );
        match present {
            Ok((birth, true))
                if birth == 0
                    || crate::util::inode_birth_ns_at(&m, current) == 0
                    || birth == crate::util::inode_birth_ns_at(&m, current) =>
            {
                sync_directory(tx, anc, bucket, &m)?;
                anc.verified_dirs.insert(current.to_path_buf(), signature);
                break;
            }
            Ok(_) => {} // validate the entire chain, including renamed ancestors
            Err(rusqlite::Error::QueryReturnedNoRows) => {}
            Err(e) => return Err(e.into()),
        }
        missing.push((current.to_path_buf(), m));
        let parent = current.parent().context("event has no indexed ancestor")?;
        anyhow::ensure!(parent != current, "event is outside the indexed root");
        current = parent;
        anyhow::ensure!(missing.len() < 4096, "ancestor chain exceeds limit");
    }
    for (p, m) in missing.into_iter().rev() {
        let (parent, name) = split(&p).context("missing directory name")?;
        let (pd, pi) = identities
            .get(parent)
            .copied()
            .context("ancestor parent identity was not captured")?;
        let (dev, ino) = crate::util::indexed_identity(&m)
            .context("ancestor identity cannot be represented by the index")?;
        upsert_path(
            tx,
            anc,
            bucket,
            &m,
            dev,
            ino,
            pd,
            pi,
            &name,
            crate::util::inode_birth_ns_at(&m, &p),
        )?;
        let signature = (dev, ino, crate::util::inode_birth_ns_at(&m, &p), m.blocks());
        anc.verified_dirs.insert(p, signature);
    }
    Ok(true)
}

fn sync_directory(
    tx: &rusqlite::Transaction,
    anc: &mut AncestorChanges,
    bucket: i64,
    m: &std::fs::Metadata,
) -> Result<()> {
    if !m.is_dir() {
        return Ok(());
    }
    let (dev, ino) = crate::util::indexed_identity(m)
        .context("directory identity cannot be represented by the index")?;
    let blocks = crate::util::allocated_bytes(m).context("reading allocated directory size")?;
    let old: Option<(i64, i64)> = tx
        .query_row(
            "SELECT blocks,birth_ns FROM inodes WHERE dev_id=?1 AND inode=?2",
            params![dev, ino],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((old, birth_ns)) = old {
        let delta = blocks
            .checked_sub(old)
            .context("directory size delta overflow")?;
        if delta != 0 {
            tx.execute(
                "UPDATE inodes SET blocks=?3 WHERE dev_id=?1 AND inode=?2",
                params![dev, ino, blocks],
            )?;
            accrue(tx, anc, dev, ino, delta, 0)?;
            gro(tx, bucket, dev, ino, birth_ns, delta)?;
        }
    }
    Ok(())
}

#[cfg(test)]
fn flush(
    store: &mut Store,
    pending: &mut HashMap<PathBuf, Op>,
    deferred: &mut HashMap<(i64, i64), DeferredFrom>,
    db: &Path,
    growth_keep_secs: i64,
) -> Result<()> {
    flush_transaction(
        store,
        pending,
        deferred,
        db,
        FlushContext {
            growth_keep_secs,
            may_expire: true,
            progress: None,
            pending_after_batch: 0,
        },
    )
}

#[derive(Clone, Copy)]
struct FlushContext {
    growth_keep_secs: i64,
    may_expire: bool,
    progress: Option<FlushProgress>,
    pending_after_batch: usize,
}

fn flush_transaction(
    store: &mut Store,
    pending: &mut HashMap<PathBuf, Op>,
    deferred: &mut HashMap<(i64, i64), DeferredFrom>,
    db: &Path,
    context: FlushContext,
) -> Result<()> {
    // Target-inode refreshes can resolve to no live paths (for example, when a
    // stale handle is delivered during reconciliation). Do not open and commit
    // an empty SQLite transaction unless a deferred rename is actually ready to
    // expire. Besides avoiding needless WAL/fsync work, this keeps a no-op event
    // from delaying publication of the caught-up status behind SQLite commit.
    let deferred_expiry_due = context.may_expire
        && deferred
            .values()
            .any(|entry| entry.expires <= Instant::now());
    if pending.is_empty() && !deferred_expiry_due {
        return Ok(());
    }
    let FlushContext {
        progress,
        pending_after_batch,
        ..
    } = context;
    let mut working = deferred.clone();
    if let Some(progress) = progress {
        crate::util::write_watch_status_with_overflows(
            db,
            pending.len().saturating_add(pending_after_batch),
            false,
            progress.events_seen,
            progress.events_resolved,
            progress.updates_committed,
            progress.last_commit_at,
            progress.updates_dropped,
            progress.behind_ms,
            progress.max_pending,
            progress.kernel_overflow_notifications,
            progress.uncertain,
        );
    }
    let outcome = flush_inner(store, pending, &mut working, db, context);
    let cleanup = store.conn.progress_handler(0, None::<fn() -> bool>);
    outcome?;
    *deferred = working;
    cleanup?;
    Ok(())
}

fn flush_inner(
    store: &mut Store,
    pending: &mut HashMap<PathBuf, Op>,
    deferred: &mut HashMap<(i64, i64), DeferredFrom>,
    db: &Path,
    context: FlushContext,
) -> Result<()> {
    let FlushContext {
        growth_keep_secs,
        may_expire,
        progress,
        pending_after_batch,
    } = context;
    let now = now_secs();
    // (Low-disk protection lives in the daemon loop now, as a self-clearing
    // `paused_since` state — NOT the lossy `dirty_since`, since a pause preserves
    // `pending` and loses nothing.)
    // COPY, don't drain: if the transaction below fails (disk full, lock, I/O),
    // we return Err with `pending` intact so the events are retried next flush
    // instead of being lost. `pending` is only cleared after a durable commit.
    let items: Vec<(PathBuf, Op)> = pending.iter().map(|(p, op)| (p.clone(), *op)).collect();
    let mut deletes: Vec<&PathBuf> = Vec::new();
    let mut moved_from: Vec<&PathBuf> = Vec::new();
    let mut moved_to: Vec<&PathBuf> = Vec::new();
    let mut upserts: Vec<&PathBuf> = Vec::new();
    for (p, op) in &items {
        match op {
            Op::Delete => deletes.push(p),
            Op::MovedFrom => moved_from.push(p),
            Op::MovedTo => moved_to.push(p),
            Op::Upsert | Op::Reconcile => upserts.push(p),
        }
    }
    let depth = |p: &PathBuf| p.components().count();
    deletes.sort_by_key(|p| std::cmp::Reverse(depth(p))); // children first
    upserts.sort_by_key(|a| depth(a)); // parents first

    tracing::debug!(
        "flush: {} del, {} moved_from, {} moved_to, {} upsert",
        deletes.len(),
        moved_from.len(),
        moved_to.len(),
        upserts.len()
    );

    let hb_db = db.to_path_buf();
    let status_db = db.to_path_buf();
    let pending_during_flush = pending.len().saturating_add(pending_after_batch);
    let progress_started = Instant::now();
    let progress_snapshot = progress;
    let mut pulse = Instant::now();
    let mut last_slow_batch_log = Duration::ZERO;
    let report_slow_phase = |phase: &'static str, elapsed: Duration, items: usize| {
        if elapsed >= Duration::from_millis(250) {
            tracing::warn!(
                phase,
                elapsed_ms = elapsed.as_millis(),
                item_count = items,
                "slow event flush phase"
            );
        }
    };
    store.conn.progress_handler(
        10000,
        Some(move || {
            if pulse.elapsed() >= Duration::from_secs(2) {
                let elapsed = progress_started.elapsed();
                crate::util::write_heartbeat(&hb_db);
                if let Some(progress) = progress_snapshot {
                    let behind_ms = progress
                        .behind_ms
                        .saturating_add(elapsed.as_millis().min(u64::MAX as u128) as u64);
                    crate::util::write_watch_status_with_overflows(
                        &status_db,
                        pending_during_flush,
                        false,
                        progress.events_seen,
                        progress.events_resolved,
                        progress.updates_committed,
                        progress.last_commit_at,
                        progress.updates_dropped,
                        behind_ms,
                        progress.max_pending,
                        progress.kernel_overflow_notifications,
                        progress.uncertain,
                    );
                }
                if elapsed >= Duration::from_secs(10)
                    && elapsed.saturating_sub(last_slow_batch_log) >= Duration::from_secs(10)
                {
                    tracing::warn!(
                        elapsed_seconds = elapsed.as_secs(),
                        pending_updates = pending_during_flush,
                        events_seen = ?progress_snapshot.map(|p| p.events_seen),
                        events_resolved = ?progress_snapshot.map(|p| p.events_resolved),
                        "SQLite event batch is still executing"
                    );
                    last_slow_batch_log = elapsed;
                }
                pulse = Instant::now();
            }
            false
        }),
    )?;
    let tx = store.conn.transaction()?;
    let mut phase_started = Instant::now();
    let bucket = now / crate::store::GROWTH_BUCKET_SECS;
    // Ancestor totals are COALESCED: each affected dir's delta is summed here and
    // written ONCE at the end, instead of one UPDATE per changed file.
    let mut anc = AncestorChanges::default();
    // Rename pairing and ambiguous create/delete records can repair topology or
    // stale leaf rows. Keep those uncommon cases exact; routine file churn uses
    // the coalesced ancestor deltas.
    if !moved_from.is_empty()
        || !moved_to.is_empty()
        || items.iter().any(|(_, op)| *op == Op::Reconcile)
    {
        anc.require_exact();
    }
    // Deferred FROMs (from a PREVIOUS flush) that got paired with a TO this flush.
    // Like Phase F's `expired`, these are removed from `deferred` only AFTER a
    // durable commit, so a tx rollback leaves the map in sync with the un-applied
    // DB and the pairing retries next flush (idempotent).
    let mut paired_deferred: std::collections::HashSet<(i64, i64)> =
        std::collections::HashSet::new();

    // ---- Phase B: deletes (unlink one path; drop the inode only on last link) ----
    for full in &deletes {
        // Delayed events describe an earlier state. If the name has been reused,
        // reconcile the current occupant instead of deleting the replacement.
        if std::fs::symlink_metadata(full).is_ok() {
            upserts.push(full);
            continue;
        }
        let (dir, name) = match split(full) {
            Some(v) => v,
            None => continue,
        };
        // A parent may already have moved on disk while its rename is still in
        // this batch. Its inode identity remains valid even under the new path.
        let parent = match stat_id(dir)? {
            Some(id) => Some(id),
            None => crate::store::indexed_path(&tx, dir)?,
        };
        let (pdev, pino) = match parent {
            Some(id) => id,
            None => continue,
        };
        unlink_dirent(&tx, &mut anc, bucket, pdev, pino, &name)?;
    }
    report_slow_phase("delete", phase_started.elapsed(), deletes.len());
    phase_started = Instant::now();

    // ---- Phase C: move-from (record renames; otherwise unlink the left path) ----
    for full in &moved_from {
        if std::fs::symlink_metadata(full).is_ok() {
            upserts.push(full);
            continue;
        }
        let (dir, name) = match split(full) {
            Some(v) => v,
            None => continue,
        };
        let (pdev, pino) = match stat_id(dir)? {
            Some(v) => v,
            None => continue,
        };
        let de: Option<(i64, i64, i64)> = tx
            .query_row(
                "SELECT d.dev_id,d.inode,i.birth_ns FROM dirents d
                 JOIN inodes i ON i.dev_id=d.dev_id AND i.inode=d.inode
                 WHERE d.parent_dev=?1 AND d.parent_inode=?2 AND d.name=?3 LIMIT 1",
                params![pdev, pino, &name],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        tracing::debug!(
            path = %full.display(), parent_dev = pdev, parent_ino = pino,
            inode = ?de.map(|(dev, ino, _)| (dev, ino)),
            "move-from reduction"
        );
        if let Some((cdev, cino, birth_ns)) = de {
            // Always defer instead of pairing against any same-inode MOVED_TO
            // observed in this batch. Hardlinks and rapid multi-rename sequences
            // can produce an unrelated/intermediate TO for the same inode; the
            // actual destination may arrive in a later flush. Phase D can consume
            // this deferred source in the same transaction when a valid TO exists.
            // Last-wins: if an inode already has a deferred FROM, the newest
            // vanished path is the one its eventual TO should pair with.
            // One inode can emit multiple vanished names before its destination
            // arrives (for example, several rapid renames of hardlinks). The
            // deferred map keeps one candidate per inode, so retire an older
            // missing candidate before replacing it. Without this, the older
            // source remains as a ghost dirent after the later candidate is
            // relocated. Preserve a candidate that still names this inode on
            // disk: it may be an independent hardlink rather than a rename.
            if let Some(previous) = deferred.get(&(cdev, cino)) {
                use std::os::unix::ffi::OsStrExt;
                let previous_path =
                    crate::store::indexed_inode_path(&tx, previous.pdev, previous.pino)?
                        .map(|path| path.join(std::ffi::OsStr::from_bytes(&previous.name)));
                let previous_still_exists = match previous_path.as_ref() {
                    Some(path) => stat_id(path)? == Some((cdev, cino)),
                    None => false,
                };
                if !previous_still_exists {
                    let previous_is_indexed: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM dirents WHERE parent_dev=?1 AND parent_inode=?2 AND name=?3 AND dev_id=?4 AND inode=?5)",
                        params![previous.pdev, previous.pino, &previous.name, cdev, cino],
                        |row| row.get(0),
                    )?;
                    if previous_is_indexed {
                        unlink_dirent(
                            &tx,
                            &mut anc,
                            bucket,
                            previous.pdev,
                            previous.pino,
                            &previous.name,
                        )?;
                    }
                }
            }
            deferred.insert(
                (cdev, cino),
                DeferredFrom {
                    pdev,
                    pino,
                    name,
                    birth_ns,
                    expires: Instant::now() + MOVE_PAIR_GRACE,
                },
            );
        }
    }
    report_slow_phase("move_from", phase_started.elapsed(), moved_from.len());
    phase_started = Instant::now();

    // ---- Phase D: move-to (relocate a renamed dirent; else add link / fresh) ----
    for full in &moved_to {
        use std::os::unix::fs::MetadataExt;
        let m = match std::fs::symlink_metadata(full) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let (dir, name) = match split(full) {
            Some(v) => v,
            None => continue,
        };
        let (npdev, npino) = match stat_id(dir)? {
            Some(v) => v,
            None => continue,
        };
        if !ensure_ancestors(&tx, &mut anc, bucket, dir)? {
            continue; // the event path was concurrently moved or removed
        }
        let (dev, ino) = crate::util::indexed_identity(&m)
            .context("move destination identity cannot be represented by the index")?;
        tracing::debug!(
            path = %full.display(), inode = ?(dev, ino),
            "move-to reduction"
        );

        // Do not short-circuit merely because this exact destination is already
        // indexed: a concurrent rebuild may have observed it while the rename
        // was in flight. First consume a matching FROM so the vanished source
        // dirent is relocated even when the destination already has this inode.

        // Pair this TO with a vanished FROM retained across flushes. Do NOT remove
        // the deferred entry yet: mutating the persistent map before commit loses
        // the rename if the transaction rolls back. Just peek; forget it after
        // commit (see paired set).
        let mut from_deferred = false;
        let mut old_loc = deferred.get(&(dev, ino)).map(|d| {
            from_deferred = true;
            (d.pdev, d.pino, d.name.clone())
        });
        if let Some((opdev, opino, ref oldname)) = old_loc {
            use std::os::unix::ffi::OsStrExt;
            let source_path = crate::store::indexed_inode_path(&tx, opdev, opino)?
                .map(|parent| parent.join(std::ffi::OsStr::from_bytes(oldname)));
            let source_still_exists = match source_path {
                Some(path) => stat_id(&path)? == Some((dev, ino)),
                None => false,
            };
            let birth = crate::util::inode_birth_ns_at(&m, full);
            let deferred_matches = !from_deferred
                || deferred
                    .get(&(dev, ino))
                    .is_some_and(|old| old.birth_ns == 0 || birth == 0 || old.birth_ns == birth);
            let valid: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM dirents d JOIN inodes i ON i.dev_id=d.dev_id AND i.inode=d.inode
                 WHERE d.parent_dev=?1 AND d.parent_inode=?2 AND d.name=?3 AND d.dev_id=?4 AND d.inode=?5
                 AND i.kind=?6 AND (i.birth_ns=0 OR ?7=0 OR i.birth_ns=?7))",
                params![opdev, opino, oldname, dev, ino, kind_of(&m), birth], |row| row.get(0))?;
            tracing::debug!(
                path = %full.display(), source_parent = ?(opdev, opino),
                source_name = ?oldname, valid, deferred = from_deferred,
                deferred_matches, source_still_exists,
                "move pair validation"
            );
            if !valid || !deferred_matches || source_still_exists {
                // A delayed FROM can outlive its directory entry or inode
                // incarnation. Never relocate a replacement at that old name.
                old_loc = None;
                if from_deferred {
                    paired_deferred.insert((dev, ino));
                }
            }
        }
        if let Some((opdev, opino, ref oldname)) = old_loc {
            let destination_is_same_inode: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM dirents WHERE parent_dev=?1 AND parent_inode=?2
                  AND name=?3 AND dev_id=?4 AND inode=?5)",
                params![npdev, npino, &name, dev, ino],
                |row| row.get(0),
            )?;
            let destination_is_source =
                npdev == opdev && npino == opino && name.as_slice() == oldname.as_slice();
            let indexed_links: i64 = tx.query_row(
                "SELECT COUNT(*) FROM dirents WHERE dev_id=?1 AND inode=?2",
                params![dev, ino],
                |row| row.get(0),
            )?;
            let destination_has_independent_link = m.nlink() as i64 >= indexed_links;
            if destination_is_same_inode
                && !destination_is_source
                && destination_has_independent_link
            {
                // A destination already linked to this inode may be an unrelated
                // hardlink or a no-op rename. It is not evidence that this FROM
                // belongs to this TO. Keep the source deferred; if no real TO
                // arrives, normal expiry removes the vanished source dirent.
                tracing::debug!(
                    path = %full.display(),
                    source_parent = ?(opdev, opino),
                    source_name = ?oldname,
                    links = m.nlink(),
                    indexed_links,
                    "retaining move-from because destination is already an inode link"
                );
                old_loc = None;
            }
        }
        if let Some((opdev, opino, oldname)) = old_loc {
            if m.is_dir() {
                anc.invalidate_geometry();
                anc.require_exact();
            }
            // RENAME of an existing link. Cycle guard: never move a dir under its
            // own subtree (a stale tree could install A→B→A and hang the CTEs).
            if would_cycle(&tx, dev, ino, npdev, npino)? {
                // Leave any deferred FROM in place: the rename didn't apply, so the
                // old path genuinely left — Phase F expiry will unlink it later.
                // We can't safely relocate (the index has a corrupt parent chain),
                // so the rename on disk and the index now disagree. Flag dirty so a
                // rescan is recommended instead of leaving silent inconsistency.
                tracing::warn!("skip move-to (would cycle): dev={dev} ino={ino} — marking dirty");
                tx.execute(
                    "INSERT INTO meta(key,value) VALUES('dirty_since',?1)
                     ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    params![now.to_string()],
                )?;
                continue;
            }
            // The relocation WILL be applied in this tx. If it paired a FROM that
            // was deferred from a previous flush, mark it to forget AFTER commit.
            if from_deferred {
                paired_deferred.insert((dev, ino));
            }
            // OVERWRITE-rename: `rename(src, dst)` atomically REPLACES dst when it
            // exists (mv -f, or the write-temp-then-rename-over-original atomic-save
            // pattern editors/dpkg/rsync use constantly). If a *different* dirent
            // already sits at the destination name, the bare UPDATE below would hit
            // UNIQUE(parent,name) and — since the whole flush is one transaction —
            // roll back EVERY pending change and retry forever, freezing a "live"
            // index. So drop the clobbered destination first (unlink_dirent also
            // subtracts its totals from the ancestor chain, matching the on-disk
            // replace). Skip when the destination IS the source (a no-op rename).
            let dest_occupied = tx
                .query_row(
                    "SELECT 1 FROM dirents
                     WHERE parent_dev=?1 AND parent_inode=?2 AND name=?3 LIMIT 1",
                    params![npdev, npino, &name],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            let dest_is_source =
                npdev == opdev && npino == opino && name.as_slice() == oldname.as_slice();
            if dest_occupied && !dest_is_source {
                unlink_dirent(&tx, &mut anc, bucket, npdev, npino, &name)?;
            }
            // Removing an overwritten destination can promote the source link
            // to prime when a concurrent scan indexed both names for this inode
            // and made the destination prime. Read the source's current
            // attribution after that promotion; using its pre-unlink value skips
            // the subsequent source-parent -> destination-parent rollup move.
            let prime: i64 = tx.query_row(
                "SELECT prime FROM dirents
                     WHERE parent_dev=?1 AND parent_inode=?2 AND name=?3 LIMIT 1",
                params![opdev, opino, &oldname],
                |r| r.get(0),
            )?;
            // relocate THIS specific dirent old->new; children keep pointing at
            // the inode, so a directory's whole subtree follows for free.
            tx.execute(
                "UPDATE dirents SET parent_dev=?1, parent_inode=?2, name=?3
                 WHERE parent_dev=?4 AND parent_inode=?5 AND name=?6",
                params![npdev, npino, &name, opdev, opino, &oldname],
            )?;
            if prime != 0 {
                // move the block attribution from the old parent chain to the new.
                let (rb, ri): (i64, i64) = tx.query_row(
                    "SELECT recursive_bytes, recursive_inodes FROM inodes
                         WHERE dev_id=?1 AND inode=?2",
                    params![dev, ino],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                let adj = anc.get(&(dev, ino)).copied().unwrap_or((0, 0));
                let eff_rb = rb
                    .checked_add(adj.0)
                    .context("renamed inode byte total overflow")?;
                let eff_ri = ri
                    .checked_add(adj.1)
                    .context("renamed inode count overflow")?;
                accrue(
                    &tx,
                    &mut anc,
                    opdev,
                    opino,
                    eff_rb.checked_neg().context("rename byte delta overflow")?,
                    eff_ri
                        .checked_neg()
                        .context("rename inode delta overflow")?,
                )?;
                accrue(&tx, &mut anc, npdev, npino, eff_rb, eff_ri)?;
            }
            if m.is_dir() {
                // Some stale child paths may already have been consumed by an
                // earlier partial batch, so absence from the current backlog
                // cannot establish a complete baseline. Verify the moved subtree
                // at its current location, within the existing work/time budget.
                reconcile_subtree(&tx, &mut anc, bucket, full, dev, ino)?;
            }
            // a rename has zero net byte delta -> no growth row
        } else {
            // not a rename: a path appeared (moved in from outside, or a new link)
            upsert_path(
                &tx,
                &mut anc,
                bucket,
                &m,
                dev,
                ino,
                npdev,
                npino,
                &name,
                crate::util::inode_birth_ns_at(&m, full),
            )?;
            // a populated directory moved in carries existing descendants that
            // produced no per-file events — index its current contents.
            if m.is_dir() {
                reconcile_subtree(&tx, &mut anc, bucket, full, dev, ino)?;
            }
        }
        // Coalesced writes may have followed this rename. Reconcile allocation
        // and ownership after moving the entry rather than treating all TOs as
        // metadata-free, zero-delta operations.
        upsert_path(
            &tx,
            &mut anc,
            bucket,
            &m,
            dev,
            ino,
            npdev,
            npino,
            &name,
            crate::util::inode_birth_ns_at(&m, full),
        )?;
    }
    report_slow_phase("move_to", phase_started.elapsed(), moved_to.len());
    phase_started = Instant::now();

    // ---- Phase E: upserts (create / modify / hardlink) ----
    let mut pulse = Instant::now();
    for full in &upserts {
        if pulse.elapsed() >= Duration::from_secs(2) {
            crate::util::write_heartbeat(db);
            pulse = Instant::now();
        }
        let m = match std::fs::symlink_metadata(full) {
            Ok(m) => m,
            Err(_) if pending.get(*full) == Some(&Op::Reconcile) => {
                // The combined event's final state is absence. Remove any stale
                // dirent through the normal accounting path; if none exists this
                // is an idempotent no-op.
                if let Some((dir, name)) = split(full) {
                    let parent = match stat_id(dir)? {
                        Some(id) => Some(id),
                        None => crate::store::indexed_path(&tx, dir)?,
                    };
                    if let Some((pdev, pino)) = parent {
                        unlink_dirent(&tx, &mut anc, bucket, pdev, pino, &name)?;
                    }
                }
                continue;
            }
            Err(_) => continue,
        };
        let (dir, name) = match split(full) {
            Some(v) => v,
            None => continue,
        };
        let (pdev, pino) = match stat_id(dir)? {
            Some(v) => v,
            None => continue,
        };
        let (dev, ino) = crate::util::indexed_identity(&m)
            .context("event path identity cannot be represented by the index")?;
        if !ensure_ancestors(&tx, &mut anc, bucket, dir)? {
            continue; // the event path was concurrently moved or removed
        }
        upsert_path(
            &tx,
            &mut anc,
            bucket,
            &m,
            dev,
            ino,
            pdev,
            pino,
            &name,
            crate::util::inode_birth_ns_at(&m, full),
        )?;
        if m.is_dir() && matches!(pending.get(*full), Some(Op::Delete | Op::MovedFrom)) {
            reconcile_subtree(&tx, &mut anc, bucket, full, dev, ino)?;
        }
    }
    report_slow_phase("upsert", phase_started.elapsed(), upserts.len());
    phase_started = Instant::now();

    // ---- Phase F: expire deferred FROMs (a rename whose TO never came) ----
    // Anything still here that has waited a full flush was a genuine move-out of
    // the tree (or a delete) — unlink it now (subtree + totals). DO NOT remove the
    // entries from `deferred` yet: if this tx later rolls back (a `?` errors), the
    // map must stay in sync with the un-applied DB so the unlink retries next flush
    // (idempotent). They're dropped only AFTER a durable commit (B1).
    let expired: Vec<(i64, i64)> = deferred
        .iter()
        .filter(|(k, d)| may_expire && d.expires <= Instant::now() && !paired_deferred.contains(k))
        .map(|(k, _)| *k)
        .collect();
    for k in &expired {
        if let Some(d) = deferred.get(k) {
            let still_matches: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM dirents de JOIN inodes i ON i.dev_id=de.dev_id AND i.inode=de.inode
                 WHERE de.parent_dev=?1 AND de.parent_inode=?2 AND de.name=?3 AND de.dev_id=?4 AND de.inode=?5
                 AND (i.birth_ns=0 OR ?6=0 OR i.birth_ns=?6))",
                params![d.pdev, d.pino, &d.name, k.0, k.1, d.birth_ns], |row| row.get(0))?;
            if still_matches {
                use std::os::unix::ffi::OsStrExt;
                let source_path = crate::store::indexed_inode_path(&tx, d.pdev, d.pino)?
                    .map(|parent| parent.join(std::ffi::OsStr::from_bytes(&d.name)));
                let source_still_exists = match source_path {
                    Some(path) => stat_id(&path)? == Some(*k),
                    None => false,
                };
                if !source_still_exists {
                    unlink_dirent(&tx, &mut anc, bucket, d.pdev, d.pino, &d.name)?;
                }
            }
        }
    }
    report_slow_phase("expire_deferred", phase_started.elapsed(), expired.len());
    phase_started = Instant::now();

    let mut touched_dirs = std::collections::HashSet::new();
    for (p, _) in &items {
        if let Some(parent) = p.parent() {
            touched_dirs.insert(parent);
        }
    }
    let touched_dir_count = touched_dirs.len();
    for dir in &touched_dirs {
        if let Ok(m) = std::fs::symlink_metadata(dir) {
            sync_directory(&tx, &mut anc, bucket, &m)?;
        }
    }
    report_slow_phase(
        "sync_directories",
        phase_started.elapsed(),
        touched_dir_count,
    );
    phase_started = Instant::now();

    // The coalesced deltas above remain available to operations within this
    // transaction. At commit, derive exact materialized values for affected
    // directories from the final prime-dirent graph, deepest first.
    reconcile_affected_rollups(&tx, &anc)?;
    report_slow_phase("rollup_reconcile", phase_started.elapsed(), anc.len());
    phase_started = Instant::now();
    crate::fault::checkpoint("watch.flush_before_commit");
    tx.commit()?;
    report_slow_phase("commit", phase_started.elapsed(), items.len());
    crate::fault::checkpoint("watch.flush_committed");
    // Events are now durably applied — safe to drop them. (Clearing BEFORE the
    // best-effort prune below ensures a prune error can't trigger a re-apply.)
    pending.clear();
    for k in &expired {
        deferred.remove(k); // committed — now safe to forget the expired renames
    }
    for k in &paired_deferred {
        deferred.remove(k); // committed — the paired rename is durably applied
    }
    crate::util::write_heartbeat(db);
    // Prune growth history beyond the retention window (configurable via
    // `dux daemon --growth-days`). On a high-churn host this table dominates index
    // size, so a shorter window keeps it small. Best-effort: a prune failure must
    // not fail the (already committed) flush.
    let keep_bucket = (now - growth_keep_secs) / crate::store::GROWTH_BUCKET_SECS;
    let _ = store
        .conn
        .execute("DELETE FROM growth WHERE bucket < ?1", params![keep_bucket]);
    Ok(())
}

/// (parent_dir, file_name_bytes) from a full path — raw bytes, identity-safe.
fn split(full: &Path) -> Option<(&Path, Vec<u8>)> {
    use std::os::unix::ffi::OsStrExt;
    let dir = full.parent()?;
    let name = full.file_name()?.as_bytes().to_vec();
    Some((dir, name))
}

fn kind_of(m: &std::fs::Metadata) -> &'static str {
    if m.is_dir() {
        "d"
    } else if m.file_type().is_symlink() {
        "l"
    } else if m.is_file() {
        "f"
    } else {
        "o"
    }
}

/// Stat a path for its (dev, inode). A non-directory ancestor is equivalent to
/// absence during rename/type-change churn: a queued child event may outlive
/// replacement of one of its parent directories by a regular file.
fn stat_id(path: &Path) -> Result<Option<(i64, i64)>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(crate::util::indexed_identity(&metadata)?)),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error).with_context(|| format!("stat indexed path {}", path.display())),
    }
}

/// Fire the alert command for paths whose growth in the window exceeds threshold.
/// Max alert subprocesses allowed to be running at once. Beyond this, new alerts
/// are logged but not spawned, so an event storm can't fork-bomb the host.
const MAX_ALERT_CHILDREN: usize = 16;
/// Alert hooks execute through the system shell after capability reduction.
/// Pin the executable because the daemon may run as root with an inherited PATH.
const ALERT_SHELL_BIN: &str = "/bin/sh";

/// Remove the daemon's capabilities from an alert child before it execs the
/// configured shell. Securebits prevents UID 0 from regaining the bounding-set
/// capabilities at exec. systemd sets and locks NOROOT for the packaged unit;
/// direct root launches set it here while CAP_SETPCAP is still available.
/// Failure is fail-closed: `Command::spawn` reports the pre-exec error and the
/// alert hook is not run with an uncertain capability set.
#[cfg(target_os = "linux")]
fn drop_alert_child_capabilities() -> std::io::Result<()> {
    const PR_GET_SECUREBITS: libc::c_int = 27;
    const PR_SET_SECUREBITS: libc::c_int = 28;
    const PR_CAP_AMBIENT: libc::c_int = 47;
    const PR_CAP_AMBIENT_CLEAR_ALL: libc::c_ulong = 4;
    const SECBIT_NOROOT: libc::c_int = 1;
    const SECBIT_NOROOT_LOCKED: libc::c_int = 2;
    const SECBIT_NOROOT_MASK: libc::c_int = SECBIT_NOROOT | SECBIT_NOROOT_LOCKED;
    const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;

    #[repr(C)]
    struct CapHeader {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct CapData {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }

    // SAFETY: prctl is called with the documented scalar arguments. A negative
    // result is converted to the OS error immediately in this single-threaded
    // post-fork child.
    let securebits = unsafe { libc::prctl(PR_GET_SECUREBITS, 0, 0, 0, 0) };
    if securebits < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if securebits & SECBIT_NOROOT_MASK != SECBIT_NOROOT_MASK
        && unsafe { libc::prctl(PR_SET_SECUREBITS, securebits | SECBIT_NOROOT_MASK, 0, 0, 0) } < 0
    {
        return Err(std::io::Error::last_os_error());
    }

    // Clear ambient capabilities before clearing permitted/effective sets.
    // SAFETY: PR_CAP_AMBIENT_CLEAR_ALL takes no pointer arguments.
    if unsafe { libc::prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_CLEAR_ALL, 0, 0, 0) } < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let header = CapHeader {
        version: LINUX_CAPABILITY_VERSION_3,
        pid: 0,
    };
    let data = [CapData::default(), CapData::default()];
    // SAFETY: header and data use the Linux v3 capset ABI, remain valid for the
    // syscall, and describe no effective, permitted, or inheritable caps.
    if unsafe { libc::syscall(libc::SYS_capset, &header, data.as_ptr()) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn check_alerts(
    store: &Store,
    cfg: &AlertConfig,
    last: &mut HashMap<(i64, i64), i64>,
    children: &mut Vec<std::process::Child>,
) -> Result<()> {
    let now = now_secs();
    // Reap any finished alert children (avoid zombies) and drop them from the set.
    children.retain_mut(|c| !matches!(c.try_wait(), Ok(Some(_))));
    // Bound the debounce map: forget entries older than 2× the alert window so it
    // can't grow without limit across the daemon's lifetime.
    let stale_before = now - (cfg.window * 2).max(3600);
    last.retain(|_, &mut t| t >= stale_before);

    // bucket granularity means the window is rounded up to the next 5 min
    let cutoff = (now - cfg.window) / crate::store::GROWTH_BUCKET_SECS;
    let mut stmt = store.conn.prepare(
        "SELECT dev_id, inode, SUM(delta) d FROM growth_current
         WHERE bucket >= ?1 GROUP BY dev_id, inode HAVING d >= ?2 ORDER BY d DESC",
    )?;
    let rows = stmt.query_map(params![cutoff, cfg.threshold], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (dev, inode, delta) = row?;
        if let Some(&t) = last.get(&(dev, inode)) {
            if now - t < cfg.debounce {
                continue;
            }
        }
        last.insert((dev, inode), now);
        let path = store
            .path_buf_of(dev, inode)
            .unwrap_or_else(|_| PathBuf::from(format!("inode:{inode}")));
        let display_path = crate::util::display_path_bytes(path.as_os_str().as_bytes());
        tracing::warn!(
            "ALERT: {} grew {} in {}s",
            display_path,
            crate::util::human(delta),
            cfg.window
        );
        if let Some(cmd) = &cfg.exec {
            if children.len() >= MAX_ALERT_CHILDREN {
                tracing::warn!(
                    "alert exec skipped ({} already running) — raise the threshold or debounce",
                    children.len()
                );
                continue;
            }
            use std::os::unix::process::CommandExt;
            let mut command = std::process::Command::new(ALERT_SHELL_BIN);
            command
                .arg("-c")
                .arg(cmd)
                .env("DUX_PATH", path.as_os_str())
                .env("DUX_DELTA", delta.to_string())
                .env("DUX_DELTA_HUMAN", crate::util::human(delta))
                .env("DUX_WINDOW", cfg.window.to_string());
            // Reset the child's OOM score to default BEFORE exec — a user alert
            // script must not inherit dux's preferred-OOM-victim boost (B3). Raw
            // syscalls only (async-signal-safe in the post-fork/pre-exec child).
            let keep_capabilities = cfg.keep_capabilities;
            unsafe {
                command.pre_exec(move || {
                    let p = b"/proc/self/oom_score_adj\0";
                    let fd = libc::open(p.as_ptr() as *const libc::c_char, libc::O_WRONLY);
                    if fd >= 0 {
                        let v = b"0\n";
                        let _ = libc::write(fd, v.as_ptr() as *const libc::c_void, v.len());
                        libc::close(fd);
                    }
                    if !keep_capabilities {
                        drop_alert_child_capabilities()?;
                    }
                    Ok(())
                });
            }
            match command.spawn() {
                Ok(child) => children.push(child), // tracked + reaped next pass
                Err(error) => tracing::warn!(
                    "failed to spawn alert hook (capability drop or exec failed): {error}"
                ),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hint_reader_ring_is_bounded_and_reports_user_space_loss() {
        let (tx, rx) = mpsc::sync_channel(2);
        let dropped = AtomicBool::new(false);
        let queued = std::sync::atomic::AtomicUsize::new(0);
        queue_hint_chunk(&tx, vec![1], &queued, &dropped);
        queue_hint_chunk(&tx, vec![2], &queued, &dropped);
        queue_hint_chunk(&tx, vec![3], &queued, &dropped);

        assert!(dropped.load(Ordering::Acquire));
        assert_eq!(queued.load(Ordering::Acquire), 2);
        assert_eq!(rx.try_recv().unwrap(), vec![1]);
        assert_eq!(rx.try_recv().unwrap(), vec![2]);
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn scan_first_event_counter_validates_metadata_and_skips_self_events() {
        let mut records = Vec::new();
        for (pid, mask) in [
            (42i32, FAN_CREATE),
            (std::process::id() as i32, FAN_MODIFY),
            (42, FAN_Q_OVERFLOW),
        ] {
            let mut record = [0u8; FAN_EVENT_METADATA_LEN];
            record[0..4].copy_from_slice(&(FAN_EVENT_METADATA_LEN as u32).to_ne_bytes());
            record[4] = FANOTIFY_METADATA_VERSION;
            record[6..8].copy_from_slice(&(FAN_EVENT_METADATA_LEN as u16).to_ne_bytes());
            record[8..16].copy_from_slice(&mask.to_ne_bytes());
            record[16..20].copy_from_slice(&(-1i32).to_ne_bytes());
            record[20..24].copy_from_slice(&pid.to_ne_bytes());
            records.extend_from_slice(&record);
        }

        assert_eq!(count_fanotify_records(&records), (1, 1, false));
        assert_eq!(
            count_fanotify_records(&records[..records.len() - 1]),
            (1, 0, true)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn scan_first_reader_drains_events_without_a_bounded_payload_queue() {
        let mut pipe = [-1; 2];
        assert_eq!(
            unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) },
            0
        );
        let read_guard = RawFdGuard(pipe[0]);
        let write_guard = RawFdGuard(pipe[1]);
        let notify = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        assert!(notify >= 0);
        let notify_guard = RawFdGuard(notify);
        let (tx, rx) = mpsc::sync_channel(1);
        let kernel_empty = Arc::new(AtomicBool::new(true));
        let queued_chunks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let dropped = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let observed_events = Arc::new(AtomicU64::new(0));
        let kernel_overflows = Arc::new(AtomicU64::new(0));
        let thread_empty = Arc::clone(&kernel_empty);
        let thread_queued = Arc::clone(&queued_chunks);
        let thread_dropped = Arc::clone(&dropped);
        let thread_stop = Arc::clone(&stop);
        let thread_seen = Arc::clone(&observed_events);
        let thread_overflows = Arc::clone(&kernel_overflows);
        let join = std::thread::spawn(move || {
            fanotify_reader_loop(
                read_guard.0,
                notify,
                tx,
                FanotifyReaderState {
                    kernel_empty: thread_empty,
                    queued_chunks: thread_queued,
                    dropped: thread_dropped,
                    stop: thread_stop,
                    observed_events: thread_seen,
                    kernel_overflows: thread_overflows,
                },
                true,
            );
        });

        let mut event = [0u8; FAN_EVENT_METADATA_LEN];
        event[0..4].copy_from_slice(&(FAN_EVENT_METADATA_LEN as u32).to_ne_bytes());
        event[4] = FANOTIFY_METADATA_VERSION;
        event[6..8].copy_from_slice(&(FAN_EVENT_METADATA_LEN as u16).to_ne_bytes());
        event[8..16].copy_from_slice(&FAN_CREATE.to_ne_bytes());
        event[16..20].copy_from_slice(&(-1i32).to_ne_bytes());
        event[20..24].copy_from_slice(&42i32.to_ne_bytes());
        const BURST_EVENTS: u64 = 10_000;
        for _ in 0..BURST_EVENTS {
            loop {
                let written = unsafe {
                    libc::write(
                        write_guard.0,
                        event.as_ptr().cast::<libc::c_void>(),
                        event.len(),
                    )
                };
                if written == event.len() as isize {
                    break;
                }
                assert!(
                    written < 0
                        && matches!(
                            std::io::Error::last_os_error().raw_os_error(),
                            Some(libc::EAGAIN) | Some(libc::EINTR)
                        ),
                    "unexpected partial/failed pipe write: {written}"
                );
                std::thread::yield_now();
            }
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while observed_events.load(Ordering::Acquire) < BURST_EVENTS && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(observed_events.load(Ordering::Acquire), BURST_EVENTS);
        assert_eq!(kernel_overflows.load(Ordering::Acquire), 0);
        assert_eq!(queued_chunks.load(Ordering::Acquire), 0);
        assert!(!dropped.load(Ordering::Acquire));
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));

        stop.store(true, Ordering::Release);
        join.join().unwrap();
        drop(notify_guard);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dedicated_hint_reader_drains_fd_into_bounded_channel() {
        let mut pipe = [-1; 2];
        assert_eq!(
            unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) },
            0
        );
        let read_guard = RawFdGuard(pipe[0]);
        let write_guard = RawFdGuard(pipe[1]);
        let notify = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        assert!(notify >= 0);
        let notify_guard = RawFdGuard(notify);
        let (tx, rx) = mpsc::sync_channel(2);
        let kernel_empty = Arc::new(AtomicBool::new(true));
        let queued_chunks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let dropped = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_empty = Arc::clone(&kernel_empty);
        let thread_queued = Arc::clone(&queued_chunks);
        let thread_dropped = Arc::clone(&dropped);
        let thread_stop = Arc::clone(&stop);
        let join = std::thread::spawn(move || {
            let read_fd = read_guard.0;
            fanotify_reader_loop(
                read_fd,
                notify,
                tx,
                FanotifyReaderState {
                    kernel_empty: thread_empty,
                    queued_chunks: thread_queued,
                    dropped: thread_dropped,
                    stop: thread_stop,
                    observed_events: Arc::new(AtomicU64::new(0)),
                    kernel_overflows: Arc::new(AtomicU64::new(0)),
                },
                false,
            );
            drop(read_guard);
        });

        let payload = b"synthetic fanotify bytes";
        assert_eq!(
            unsafe {
                libc::write(
                    write_guard.0,
                    payload.as_ptr().cast::<libc::c_void>(),
                    payload.len(),
                )
            },
            payload.len() as isize
        );
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), payload);
        let deadline = Instant::now() + Duration::from_secs(1);
        while !kernel_empty.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(kernel_empty.load(Ordering::Acquire));
        assert_eq!(queued_chunks.load(Ordering::Acquire), 1);
        assert!(!dropped.load(Ordering::Acquire));

        stop.store(true, Ordering::Release);
        join.join().unwrap();
        drop(notify_guard);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dedicated_hint_reader_clears_stale_nonempty_state_after_idle_poll() {
        let mut pipe = [-1; 2];
        assert_eq!(
            unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) },
            0
        );
        let read_guard = RawFdGuard(pipe[0]);
        let write_guard = RawFdGuard(pipe[1]);
        let notify = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        assert!(notify >= 0);
        let notify_guard = RawFdGuard(notify);
        let (tx, _rx) = mpsc::sync_channel(2);
        let kernel_empty = Arc::new(AtomicBool::new(false));
        let queued_chunks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let dropped = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_empty = Arc::clone(&kernel_empty);
        let thread_queued = Arc::clone(&queued_chunks);
        let thread_dropped = Arc::clone(&dropped);
        let thread_stop = Arc::clone(&stop);
        let join = std::thread::spawn(move || {
            let read_fd = read_guard.0;
            fanotify_reader_loop(
                read_fd,
                notify,
                tx,
                FanotifyReaderState {
                    kernel_empty: thread_empty,
                    queued_chunks: thread_queued,
                    dropped: thread_dropped,
                    stop: thread_stop,
                    observed_events: Arc::new(AtomicU64::new(0)),
                    kernel_overflows: Arc::new(AtomicU64::new(0)),
                },
                false,
            );
            drop(read_guard);
        });

        let deadline = Instant::now() + Duration::from_secs(1);
        while !kernel_empty.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(
            kernel_empty.load(Ordering::Acquire),
            "an idle reader must clear the nonempty state after its poll timeout"
        );

        stop.store(true, Ordering::Release);
        join.join().unwrap();
        drop(write_guard);
        drop(notify_guard);
    }

    #[test]
    fn hint_resolver_processes_bounded_jobs_off_coordinator() {
        let notify_fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        assert!(notify_fd >= 0);
        let notify = RawFdGuard(notify_fd);
        let queued = Arc::new(std::sync::atomic::AtomicUsize::new(1));
        let mut resolver = FanotifyHintResolver::start(
            HashMap::new(),
            HashSet::new(),
            HashSet::new(),
            PathBuf::from("/"),
            notify_fd,
            Arc::clone(&queued),
        )
        .unwrap();

        resolver
            .tx
            .try_send(HintResolutionJob {
                handles: Vec::new(),
            })
            .unwrap();
        let result = resolver.rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(result.seen, 0);
        assert!(!result.lost);
        assert_eq!(queued.load(Ordering::Acquire), 1);
        resolver.stop_and_join();
        drop(notify);
    }

    #[test]
    fn resolver_result_queue_saturation_records_loss_without_blocking() {
        let notify_fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        assert!(notify_fd >= 0);
        let notify = RawFdGuard(notify_fd);
        let (tx, rx) = mpsc::sync_channel(1);
        let queued = std::sync::atomic::AtomicUsize::new(2);
        let dropped = AtomicBool::new(false);
        let result = HintResolutionResult {
            seen: 1,
            lost: false,
            priority_paths: Vec::new(),
        };

        queue_hint_resolution_result(&tx, result, &queued, &dropped, notify_fd);
        queue_hint_resolution_result(
            &tx,
            HintResolutionResult {
                seen: 2,
                lost: false,
                priority_paths: Vec::new(),
            },
            &queued,
            &dropped,
            notify_fd,
        );

        assert!(dropped.load(Ordering::Acquire));
        assert_eq!(queued.load(Ordering::Acquire), 1);
        assert_eq!(rx.try_recv().unwrap().seen, 1);
        drop(notify);
    }

    #[test]
    fn alert_shell_is_resolved_by_absolute_path() {
        assert_eq!(ALERT_SHELL_BIN, "/bin/sh");
        assert!(Path::new(ALERT_SHELL_BIN).is_absolute());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn alert_child_capabilities_are_cleared_before_exec() {
        use std::os::unix::process::CommandExt;

        let mut command = std::process::Command::new(ALERT_SHELL_BIN);
        command
            .args(["-c", "grep -E '^Cap(Eff|Prm|Inh|Amb):' /proc/self/status"])
            .stdout(std::process::Stdio::piped());
        // The normal test environment is root and has CAP_SETPCAP. In a
        // restricted root container, the test cannot establish NOROOT itself;
        // the packaged-unit integration check covers its locked-SecureBits
        // path. Do not weaken the production helper when that setup is absent.
        let securebits = unsafe { libc::prctl(27, 0, 0, 0, 0) };
        let mut status = String::new();
        if let Ok(contents) = std::fs::read_to_string("/proc/self/status") {
            status = contents;
        }
        let effective = status
            .lines()
            .find_map(|line| line.strip_prefix("CapEff:\t"))
            .and_then(|value| u64::from_str_radix(value, 16).ok())
            .unwrap_or(0);
        const CAP_SETPCAP: u64 = 1 << 8;
        const SECBIT_NOROOT_MASK: i32 = 3;
        if unsafe { libc::geteuid() } == 0
            && effective & CAP_SETPCAP == 0
            && (securebits < 0 || securebits & SECBIT_NOROOT_MASK != SECBIT_NOROOT_MASK)
        {
            eprintln!("skipping alert capability integration: cannot set or inherit locked NOROOT");
            return;
        }
        unsafe {
            command.pre_exec(drop_alert_child_capabilities);
        }
        let output = command.output().expect("spawn alert capability probe");
        assert!(output.status.success(), "probe failed: {:?}", output);
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let value = line.split_whitespace().nth(1).expect("capability value");
            assert_eq!(u64::from_str_radix(value, 16).unwrap(), 0, "{line}");
        }
    }

    #[test]
    fn raw_fd_guard_closes_descriptors_on_early_return() {
        let mut pipe_fds = [-1; 2];
        assert_eq!(unsafe { libc::pipe(pipe_fds.as_mut_ptr()) }, 0);
        let read_guard = RawFdGuard(pipe_fds[0]);
        let write_guard = RawFdGuard(pipe_fds[1]);
        let read_fd = read_guard.0;
        drop(read_guard);
        assert_eq!(unsafe { libc::fcntl(read_fd, libc::F_GETFD) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EBADF)
        );
        drop(write_guard);
    }

    #[test]
    fn failed_filesystem_identity_lookup_is_not_silently_accepted() {
        let error = statfs_fsid(Path::new("/dux-readiness-path-that-does-not-exist"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("statfs"), "{error}");
        assert!(error.contains("does-not-exist"), "{error}");
    }

    #[test]
    fn stat_id_treats_a_non_directory_ancestor_as_a_vanished_path() {
        let (dir, db, store) = fixture("stat-id-not-directory");
        let ancestor = dir.join("now-a-file");
        std::fs::write(&ancestor, b"file").unwrap();
        assert_eq!(stat_id(&ancestor.join("child")).unwrap(), None);
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn persisted_pause_transition_is_atomic_and_surfaces_write_errors() {
        let db = tmp("pause-metadata-atomic");
        cleanup(Path::new("/nonexistent/dux-test-root"), &db);
        let store = Store::create_fresh(&db).unwrap();
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER reject_pause_reason BEFORE INSERT ON meta
                 WHEN NEW.key='pause_reason'
                 BEGIN SELECT RAISE(ABORT, 'injected pause metadata failure'); END;",
            )
            .unwrap();
        let error = persist_pause_state(&store, Some("critical pressure")).unwrap_err();
        assert!(error
            .to_string()
            .contains("injected pause metadata failure"));
        assert_eq!(store.get_meta("paused_since").unwrap(), None);
        assert_eq!(store.get_meta("pause_reason").unwrap(), None);

        store
            .conn
            .execute_batch("DROP TRIGGER reject_pause_reason;")
            .unwrap();
        persist_pause_state(&store, Some("critical pressure")).unwrap();
        assert!(store.get_meta("paused_since").unwrap().is_some());
        assert_eq!(
            store.get_meta("pause_reason").unwrap().as_deref(),
            Some("critical pressure")
        );
        persist_pause_state(&store, None).unwrap();
        assert_eq!(store.get_meta("paused_since").unwrap(), None);
        assert_eq!(store.get_meta("pause_reason").unwrap(), None);
        drop(store);
        cleanup(Path::new("/nonexistent/dux-test-root"), &db);
    }

    #[test]
    fn wal_write_guard_pauses_only_for_oversized_uncheckpointed_wal() {
        assert!(wal_write_guard_transition(
            false,
            Some(MAX_WAL_BYTES + 1),
            Some((1, 10, 4))
        ));
        assert!(!wal_write_guard_transition(
            false,
            Some(MAX_WAL_BYTES + 1),
            Some((0, 10, 10))
        ));
        assert!(wal_write_guard_transition(true, Some(MAX_WAL_BYTES), None));
        assert!(wal_write_guard_transition(true, None, None));
        assert!(!wal_write_guard_transition(true, Some(0), None));
        assert!(!wal_write_guard_transition(
            true,
            Some(MAX_WAL_BYTES + 1),
            Some((0, 10, 10))
        ));
    }

    #[test]
    fn passive_checkpoint_reports_reader_pinned_frames_and_recovers() {
        let db = tmp("wal-pinned-reader");
        cleanup(Path::new("/nonexistent/dux-test-root"), &db);
        let writer = rusqlite::Connection::open(&db).unwrap();
        writer
            .execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE t(value INTEGER);")
            .unwrap();
        writer.execute("INSERT INTO t VALUES(1)", []).unwrap();

        let mut reader = rusqlite::Connection::open(&db).unwrap();
        let reader_tx = reader.transaction().unwrap();
        assert_eq!(
            reader_tx
                .query_row("SELECT count(*) FROM t", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        for value in 2..=64 {
            writer.execute("INSERT INTO t VALUES(?1)", [value]).unwrap();
        }

        let (_, log_frames, checkpointed_frames) = wal_checkpoint_progress(&writer).unwrap();
        assert!(
            log_frames > 0,
            "expected WAL frames while the reader pins a snapshot"
        );
        assert!(
            checkpointed_frames < log_frames,
            "checkpoint should report pinned frames"
        );
        drop(reader_tx);

        let (_, log_frames, checkpointed_frames) = wal_checkpoint_progress(&writer).unwrap();
        assert!(
            checkpointed_frames >= log_frames,
            "checkpoint should catch up after reader exits"
        );
        drop(reader);
        drop(writer);
        cleanup(Path::new("/nonexistent/dux-test-root"), &db);
    }

    #[test]
    fn best_effort_dirty_marker_persists_the_cause() {
        let db = tmp("best-effort-dirty-reason");
        cleanup(Path::new("/nonexistent/dux-test-root"), &db);
        let store = Store::create_fresh(&db).unwrap();

        assert!(persist_dirty_marker_best_effort(
            &store,
            "fanotify queue overflow"
        ));
        assert!(store.get_meta("dirty_since").unwrap().is_some());
        assert_eq!(
            store.get_meta("dirty_reason").unwrap().as_deref(),
            Some("fanotify queue overflow")
        );
        drop(store);
        cleanup(Path::new("/nonexistent/dux-test-root"), &db);
    }

    #[test]
    fn dirty_reason_write_failure_rolls_back_best_effort_dirty_marker() {
        let db = tmp("best-effort-dirty-reason-failure");
        cleanup(Path::new("/nonexistent/dux-test-root"), &db);
        let store = Store::create_fresh(&db).unwrap();
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER reject_dirty_reason BEFORE INSERT ON meta
                 WHEN NEW.key='dirty_reason' BEGIN SELECT RAISE(ABORT,'injected'); END;",
            )
            .unwrap();

        assert!(!persist_dirty_marker_best_effort(
            &store,
            "fanotify queue overflow"
        ));
        assert!(store.get_meta("dirty_since").unwrap().is_none());
        assert!(store.get_meta("dirty_reason").unwrap().is_none());
        drop(store);
        cleanup(Path::new("/nonexistent/dux-test-root"), &db);
    }

    #[test]
    fn startup_uncertainty_marker_must_persist() {
        let db = tmp("startup-dirty-marker");
        cleanup(Path::new("/nonexistent/dux-test-root"), &db);
        let store = Store::create_fresh(&db).unwrap();
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER reject_dirty_marker BEFORE INSERT ON meta
                 WHEN NEW.key='dirty_since' BEGIN SELECT RAISE(ABORT,'injected'); END;",
            )
            .unwrap();
        let error = mark_index_dirty(&store, "startup gap").unwrap_err();
        assert!(error.to_string().contains("persisting dirty index marker"));
        assert!(store.get_meta("dirty_since").unwrap().is_none());
        drop(store);
        cleanup(Path::new("/nonexistent/dux-test-root"), &db);
    }

    #[test]
    fn mount_change_marker_failure_is_reported_instead_of_silently_continuing() {
        let db = tmp("mount-change-dirty-marker");
        cleanup(Path::new("/nonexistent/dux-test-root"), &db);
        let store = Store::create_fresh(&db).unwrap();
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER reject_dirty_reason BEFORE INSERT ON meta
                 WHEN NEW.key='dirty_reason' BEGIN SELECT RAISE(ABORT,'injected'); END;",
            )
            .unwrap();
        let error = mark_index_dirty(&store, "non-watched mount changed").unwrap_err();
        assert!(
            error.to_string().contains("persisting dirty index reason"),
            "{error:#}"
        );
        assert!(store.get_meta("dirty_since").unwrap().is_some());
        assert!(store.get_meta("dirty_reason").unwrap().is_none());
        drop(store);
        cleanup(Path::new("/nonexistent/dux-test-root"), &db);
    }

    #[test]
    fn rebuild_source_stays_read_only_until_atomic_replacement() {
        let db = tmp("startup-rebuild-read-only-source");
        cleanup(Path::new("/nonexistent/dux-test-root"), &db);
        {
            let store = Store::create_fresh(&db).unwrap();
            store.set_meta("schema_version", "5").unwrap();
        }
        assert!(Store::needs_rebuild(&db).unwrap());
        let store = Store::open_rebuild_source(&db).unwrap();
        assert!(mark_index_dirty(&store, "startup gap").is_err());
        assert!(mark_index_dirty_before_reconciliation(&store, true).is_ok());
        assert!(store.get_meta("dirty_since").unwrap().is_none());
        drop(store);
        cleanup(Path::new("/nonexistent/dux-test-root"), &db);
    }

    #[test]
    fn root_match_requires_valid_readable_identity() {
        let dir = tmp("root-match");
        let db = tmp("root-match-db");
        cleanup(&dir, &db);
        std::fs::create_dir_all(&dir).unwrap();
        let id = id_of(&dir);
        let store = Store::create_fresh(&db).unwrap();
        store.finalize_bulk().unwrap();
        drop(store);

        assert!(
            !root_matches(&db, &dir).unwrap(),
            "missing identity requests rebuild"
        );
        let store = Store::open_rw(&db).unwrap();
        store.set_meta("root_dev", &id.0.to_string()).unwrap();
        store.set_meta("root_inode", "malformed").unwrap();
        drop(store);
        assert!(root_matches(&db, &dir)
            .unwrap_err()
            .to_string()
            .contains("invalid root_inode metadata"));

        let store = Store::open_rw(&db).unwrap();
        store.set_meta("root_inode", &id.1.to_string()).unwrap();
        drop(store);
        assert!(root_matches(&db, &dir).unwrap());
        cleanup(&dir, &db);
    }

    #[test]
    fn indexed_parent_path_cache_propagates_sql_errors_without_caching_them() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let mut paths = IndexedParentPathCache::default();

        assert!(paths.resolve(&conn, 1, 2).is_err());
        assert!(paths.resolve(&conn, 1, 2).is_err());
        assert_eq!(paths.lookups, 0);
        assert!(paths.paths.is_empty());
    }

    #[test]
    fn background_reconciliation_uses_two_low_priority_walker_jobs() {
        let cancel = Arc::new(AtomicBool::new(false));
        let opts = background_reconciliation_scan_options(true, Arc::clone(&cancel));

        assert!(opts.one_file_system);
        assert!(!opts.progress);
        assert!(opts.low_priority);
        assert_eq!(opts.jobs, Some(BACKGROUND_RECONCILIATION_JOBS));
        assert_eq!(opts.jobs, Some(2));
        assert!(Arc::ptr_eq(opts.cancel.as_ref().unwrap(), &cancel));
    }

    #[test]
    fn growth_writer_separates_inode_incarnations_and_skips_unknown_birth() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::store::SCHEMA_SQL).unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute(
            "INSERT INTO inodes(dev_id,inode,kind,blocks,uid,mtime,birth_ns)
             VALUES(7,9,'f',40,0,0,100)",
            [],
        )
        .unwrap();
        let bucket = 123;
        gro(&tx, bucket, 7, 9, 100, 40).unwrap();
        tx.execute(
            "UPDATE inodes SET birth_ns=200 WHERE dev_id=7 AND inode=9",
            [],
        )
        .unwrap();
        gro(&tx, bucket, 7, 9, 200, 70).unwrap();
        gro(&tx, bucket, 7, 9, 0, 900).unwrap();
        tx.commit().unwrap();

        let history_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM growth WHERE dev_id=7 AND inode=9",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(history_rows, 2, "distinct births get independent rows");
        let current_growth: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(delta),0) FROM growth_current WHERE dev_id=7 AND inode=9",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            current_growth, 70,
            "old and unknown generations are not attributed"
        );
    }

    #[test]
    fn watcher_learns_birth_time_when_existing_inode_metadata_lacked_it() {
        use std::os::unix::fs::MetadataExt;
        let (dir, db, mut store) = fixture("learn-birth-time");
        let path = dir.join("birth-known-file");
        std::fs::write(&path, vec![7_u8; 4096]).unwrap();
        let metadata = std::fs::metadata(&path).unwrap();
        let (dev, ino) = id_of(&path);
        let (parent_dev, parent_ino) = id_of(&dir);
        store.conn.execute(
            "INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime,birth_ns)
             VALUES(?1,?2,'f',?3,?3,1,?4,?5,0)",
            params![dev, ino, metadata.blocks() as i64 * 512, metadata.uid() as i64, metadata.mtime()],
        ).unwrap();
        store.conn.execute(
            "INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime) VALUES(?1,?2,?3,?4,?5,1)",
            params![parent_dev, parent_ino, &b"birth-known-file"[..], dev, ino],
        ).unwrap();

        let birth_ns = crate::util::inode_birth_ns_at(&metadata, &path);
        assert_ne!(birth_ns, 0, "the fixture filesystem must expose birth time");
        let tx = store.conn.transaction().unwrap();
        upsert_path(
            &tx,
            &mut AncestorChanges::default(),
            now_secs() / crate::store::GROWTH_BUCKET_SECS,
            &metadata,
            dev,
            ino,
            parent_dev,
            parent_ino,
            b"birth-known-file",
            birth_ns,
        )
        .unwrap();
        tx.commit().unwrap();
        let stored_birth: i64 = store
            .conn
            .query_row(
                "SELECT birth_ns FROM inodes WHERE dev_id=?1 AND inode=?2",
                params![dev, ino],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_birth, birth_ns);

        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn accounting_rejects_broken_parent_rows_before_accumulating() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::store::SCHEMA_SQL).unwrap();
        conn.execute_batch("INSERT INTO dirents VALUES(1,'bad-parent',X'61',1,2,1)")
            .unwrap();
        let tx = conn.transaction().unwrap();
        let mut anc = AncestorChanges::default();
        assert!(accrue(&tx, &mut anc, 1, 2, 4096, 1).is_err());
        assert!(anc.deltas.is_empty());
        assert!(would_cycle(&tx, 1, 99, 1, 2).is_err());
    }

    #[test]
    fn accounting_rejects_parent_cycles_and_delta_overflow() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::store::SCHEMA_SQL).unwrap();
        conn.execute_batch("INSERT INTO dirents VALUES(1,3,X'61',1,2,1),(1,2,X'62',1,3,1)")
            .unwrap();
        let tx = conn.transaction().unwrap();
        let mut anc = AncestorChanges::default();
        assert!(accrue(&tx, &mut anc, 1, 2, 4096, 1)
            .unwrap_err()
            .to_string()
            .contains("cycle"));
        assert!(anc.deltas.is_empty());
        tx.execute("DELETE FROM dirents", []).unwrap();
        anc.deltas.insert((1, 1), (i64::MAX, 0));
        assert!(accrue(&tx, &mut anc, 1, 1, 1, 0)
            .unwrap_err()
            .to_string()
            .contains("overflow"));
    }

    #[test]
    fn growth_bucket_rejects_sqlite_integer_overflow() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::store::SCHEMA_SQL).unwrap();
        let tx = conn.transaction().unwrap();
        gro(&tx, 10, 1, 2, 3, i64::MAX).unwrap();
        let error = gro(&tx, 10, 1, 2, 3, 1).unwrap_err().to_string();
        assert!(error.contains("growth delta exceeds the SQLite integer range"));
        let raw_sql_error = tx
            .execute(
                "UPDATE growth SET delta=delta+1 WHERE bucket=10 AND dev_id=1 AND inode=2",
                [],
            )
            .unwrap_err()
            .to_string();
        assert!(raw_sql_error.contains("growth delta must be an SQLite integer"));
        let (delta, storage_type): (i64, String) = tx
            .query_row(
                "SELECT delta,typeof(delta) FROM growth WHERE bucket=10 AND dev_id=1 AND inode=2",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(delta, i64::MAX);
        assert_eq!(storage_type, "integer");
    }

    #[test]
    fn rollup_overflow_falls_back_to_exact_final_graph() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::store::SCHEMA_SQL).unwrap();
        conn.execute_batch(
            "INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime)
                 VALUES(1,1,'d',4096,9223372036854775807,1,0,0),
                       (1,2,'f',4096,4096,1,0,0);
             INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
                 VALUES(1,1,X'66',1,2,1);",
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        let mut affected = AncestorChanges::default();
        affected.deltas.insert((1, 1), (1, 0));
        reconcile_affected_rollups(&tx, &affected).unwrap();
        let (bytes, inodes): (i64, i64) = tx
            .query_row(
                "SELECT recursive_bytes,recursive_inodes FROM inodes WHERE dev_id=1 AND inode=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((bytes, inodes), (8192, 2));
    }

    #[test]
    fn unlink_row_failure_rolls_back_instead_of_dropping_accounting() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::store::SCHEMA_SQL).unwrap();
        conn.execute_batch(
            "INSERT INTO dirents VALUES(1,1,X'61',1,2,1);
            INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,uid,mtime)
            VALUES(1,2,'f',4096,'bad-total',0,0)",
        )
        .unwrap();
        {
            let tx = conn.transaction().unwrap();
            assert!(unlink_dirent(&tx, &mut AncestorChanges::default(), 0, 1, 1, b"a").is_err());
        }
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM dirents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
    use crate::scan::{self, ScanOptions};
    use std::collections::{HashMap, HashSet};

    #[test]
    fn fanotify_overflow_is_counted_separately_from_userspace_drops() {
        let root = tmp("overflow-notification-root");
        let db = tmp("overflow-notification-db");
        cleanup(&root, &db);
        std::fs::create_dir_all(&root).unwrap();
        let mut store = Store::create_fresh(&db).unwrap();
        let metadata = FanotifyEventMetadata {
            event_len: FAN_EVENT_METADATA_LEN as u32,
            vers: FANOTIFY_METADATA_VERSION,
            reserved: 0,
            metadata_len: FAN_EVENT_METADATA_LEN as u16,
            mask: FAN_Q_OVERFLOW,
            fd: -1,
            pid: 0,
        };
        let event = unsafe {
            std::slice::from_raw_parts(
                (&metadata as *const FanotifyEventMetadata).cast::<u8>(),
                FAN_EVENT_METADATA_LEN,
            )
        };
        let mut pending = HashMap::new();
        let mut born = HashSet::new();
        let mut targets = HashSet::new();
        let mut parent_paths = IndexedParentPathCache::default();
        let mut lost = false;
        let (seen, resolved, overflows) = parse_events(
            event,
            &HashMap::new(),
            &mut HashSet::new(),
            &root,
            &db,
            &mut pending,
            &mut born,
            &mut targets,
            &mut parent_paths,
            &mut store,
            true,
            &mut lost,
        );
        assert_eq!((seen, resolved, overflows), (0, 0, 1));
        assert!(lost);
        assert!(store.get_meta("dirty_since").unwrap().is_some());

        // If SQLite refuses the persistent marker, coverage must still remain
        // uncertain in memory and parsing must continue without hiding the
        // persistence failure path behind an unchecked Result.
        store
            .conn
            .execute_batch(
                "DELETE FROM meta WHERE key='dirty_since';
                 CREATE TRIGGER reject_overflow_dirty_marker BEFORE INSERT ON meta
                 WHEN NEW.key='dirty_since' BEGIN SELECT RAISE(ABORT,'injected'); END;",
            )
            .unwrap();
        lost = false;
        let (_, _, overflows) = parse_events(
            event,
            &HashMap::new(),
            &mut HashSet::new(),
            &root,
            &db,
            &mut pending,
            &mut born,
            &mut targets,
            &mut parent_paths,
            &mut store,
            true,
            &mut lost,
        );
        assert_eq!(overflows, 1);
        assert!(
            lost,
            "overflow must leave coverage uncertain after marker failure"
        );
        assert!(store.get_meta("dirty_since").unwrap().is_none());
        drop(store);
        cleanup(&root, &db);
    }

    #[test]
    fn scan_first_hint_parser_coalesces_parent_handles_and_bounds_memory() {
        let mut bytes = synthetic_hint_record(std::process::id() as i32, 9, b"dux.db");
        bytes.extend(synthetic_hint_record(123, 42, b"one"));
        bytes.extend(synthetic_hint_record(123, 42, b"two"));
        bytes.extend(synthetic_overflow_record());
        let (overflows, consumed, lost, handles) = parse_fanotify_hints(&bytes, 8);
        assert_eq!(handles.len(), 1, "same parent handle must be coalesced");
        assert_eq!(handles[0].1, 2);
        assert_eq!(overflows, 1);
        assert_eq!(consumed, bytes.len());
        assert!(lost, "kernel overflow widens recovery to a full scan");

        let mut saturated = Vec::new();
        for ino in 1..=(MAX_FANOTIFY_HINT_HANDLES as u32 + 1) {
            saturated.extend(synthetic_hint_record(123, ino, b"x"));
        }
        let (overflows, consumed, lost, handles) =
            parse_fanotify_hints(&saturated, MAX_FANOTIFY_HINT_HANDLES + 1);
        assert_eq!(overflows, 0);
        assert_eq!(consumed, saturated.len());
        assert_eq!(handles.len(), MAX_FANOTIFY_HINT_HANDLES);
        assert!(
            lost,
            "a saturated hint set must request full reconciliation"
        );
    }

    #[test]
    fn hint_reader_filters_self_events_before_bounded_ring() {
        let self_event = synthetic_hint_record(std::process::id() as i32, 9, b"dux.db");
        let foreign_event = synthetic_hint_record(123, 42, b"user-file");
        let mut mixed = self_event.clone();
        mixed.extend_from_slice(&foreign_event);

        let filtered = filter_self_fanotify_records(&mixed).unwrap();
        assert_eq!(filtered, foreign_event);
        assert!(filter_self_fanotify_records(&self_event)
            .unwrap()
            .is_empty());

        let mut overflow = synthetic_overflow_record();
        // Overflow records must survive even if the reported pid matches Dux.
        let pid_offset = std::mem::offset_of!(FanotifyEventMetadata, pid);
        overflow[pid_offset..pid_offset + std::mem::size_of::<i32>()]
            .copy_from_slice(&(std::process::id() as i32).to_ne_bytes());
        assert_eq!(filter_self_fanotify_records(&overflow).unwrap(), overflow);
    }

    #[test]
    fn hint_reader_preserves_malformed_chunks_for_fail_closed_parser() {
        let mut malformed = synthetic_hint_record(std::process::id() as i32, 9, b"dux.db");
        malformed[0..4].copy_from_slice(&1u32.to_ne_bytes());
        assert!(filter_self_fanotify_records(&malformed).is_none());
    }

    #[test]
    fn scan_first_cadence_is_anchored_to_observation_start() {
        let started = Instant::now();
        let finished = started + Duration::from_secs(120);
        let just_before_due = finished + SCAN_FIRST_INTERVAL - Duration::from_secs(121);
        let due = finished + SCAN_FIRST_INTERVAL - Duration::from_secs(120);

        assert!(!scan_first_observation_due(started, just_before_due));
        assert!(scan_first_observation_due(started, due));
        assert!(
            due.duration_since(finished) < SCAN_FIRST_INTERVAL,
            "a raced scan must not add its duration to the next observation interval"
        );
    }

    #[test]
    fn scan_first_hint_loss_waits_for_fixed_cadence_but_event_mode_repairs() {
        assert!(!hint_loss_requests_immediate_scan(true));
        assert!(hint_loss_requests_immediate_scan(false));

        let started = Instant::now();
        let retry_time = started + Duration::from_secs(300);
        let cadence_time = started + SCAN_FIRST_INTERVAL;
        assert!(!dirty_retry_due_for_event_mode(
            true, true, started, retry_time
        ));
        assert!(dirty_retry_due_for_event_mode(
            false, true, started, retry_time
        ));
        assert!(!dirty_retry_due_for_event_mode(
            false, false, started, retry_time
        ));
        assert!(scan_first_observation_due(started, cadence_time));
        assert!(reconciliation_retry_gate_elapsed(true, true, false));
        assert!(reconciliation_retry_gate_elapsed(true, false, true));
        assert!(!reconciliation_retry_gate_elapsed(true, false, false));
        assert!(!reconciliation_retry_gate_elapsed(false, true, false));
    }

    #[test]
    fn scan_first_does_not_drop_manual_followup_when_observation_races() {
        assert!(scan_first_preserve_manual_followup(true, true));
        assert!(!scan_first_preserve_manual_followup(true, false));
        assert!(!scan_first_preserve_manual_followup(false, true));
        assert!(!scan_first_preserve_manual_followup(false, false));
    }

    #[test]
    fn scan_first_accepts_a_complete_last_observation_after_hint_loss() {
        assert!(scan_first_event_race_only(
            true, true, false, false, false, 0
        ));
        assert!(scan_first_event_race_only(
            true, true, true, false, false, 0
        ));
        assert!(scan_first_event_race_only(
            true, false, true, false, false, 0
        ));
        assert!(!scan_first_event_race_only(
            true, true, false, true, false, 0
        ));
        assert!(!scan_first_event_race_only(
            true, true, false, false, true, 0
        ));
        assert!(!scan_first_event_race_only(
            true, true, false, false, false, 1
        ));
        assert!(!scan_first_event_race_only(
            false, true, false, false, false, 0
        ));
    }

    #[test]
    fn scan_first_unresolved_hint_only_invalidates_an_active_observation() {
        assert!(scan_first_hint_invalidates_observation(true, false, true));
        assert!(scan_first_hint_invalidates_observation(false, true, false));
        assert!(!scan_first_hint_invalidates_observation(false, false, true));
        assert!(!scan_first_hint_invalidates_observation(
            false, false, false
        ));
    }

    #[test]
    fn scan_first_uses_atomic_upgrade_until_a_v7_baseline_is_ready() {
        assert!(!scan_first_can_use_in_place(true, false, false));
        assert!(scan_first_can_use_in_place(true, true, false));
        assert!(!scan_first_can_use_in_place(true, true, true));
        assert!(!scan_first_can_use_in_place(false, true, false));
    }

    #[test]
    fn scan_first_hint_parser_rejects_malformed_record_lengths() {
        let malformed = FanotifyEventMetadata {
            event_len: (FAN_EVENT_METADATA_LEN - 1) as u32,
            vers: FANOTIFY_METADATA_VERSION,
            reserved: 0,
            metadata_len: FAN_EVENT_METADATA_LEN as u16,
            mask: FAN_MODIFY,
            fd: -1,
            pid: 123,
        };
        let bytes = unsafe {
            std::slice::from_raw_parts(
                (&malformed as *const FanotifyEventMetadata).cast::<u8>(),
                FAN_EVENT_METADATA_LEN,
            )
        };
        let (overflows, consumed, lost, handles) = parse_fanotify_hints(bytes, 8);
        assert_eq!(overflows, 0);
        assert!(handles.is_empty());
        assert_eq!(consumed, bytes.len());
        assert!(lost);
    }

    #[test]
    fn scan_first_hint_parser_fails_closed_for_bad_or_unresolvable_parent_handles() {
        let mut malformed = synthetic_hint_record(123, 42, b"entry");
        malformed[36..40].copy_from_slice(&((MAX_HANDLE_SZ as u32) + 1).to_ne_bytes());
        let (overflows, consumed, lost, handles) = parse_fanotify_hints(&malformed, 8);
        assert_eq!(overflows, 0);
        assert_eq!(consumed, malformed.len());
        assert!(handles.is_empty());
        assert!(
            lost,
            "invalid handle lengths must request a full observation"
        );

        let encoded = synthetic_hint_record(123, 43, b"entry");
        let (overflows, consumed, lost, handles) = parse_fanotify_hints(&encoded, 8);
        assert_eq!(overflows, 0);
        assert!(!lost);
        assert_eq!(consumed, encoded.len());
        let (seen, unresolved, priority_paths) = resolve_fanotify_hint_scope(
            handles,
            &HashMap::new(),
            &mut HashSet::new(),
            &HashSet::new(),
            Path::new("/indexed-root"),
        );
        assert_eq!(seen, 0);
        assert!(unresolved, "unresolvable scope must widen to a full scan");
        assert!(priority_paths.is_empty());

        let (_, _, _, ambiguous_hints) = parse_fanotify_hints(&encoded, 8);
        let (seen, unresolved, priority_paths) = resolve_fanotify_hint_scope(
            ambiguous_hints,
            &HashMap::new(),
            &mut HashSet::new(),
            &HashSet::from([(1, 2)]),
            Path::new("/indexed-root"),
        );
        assert_eq!(seen, 1);
        assert!(
            priority_paths.is_empty(),
            "ambiguous aliases cannot safely prioritize a visible path"
        );
        assert!(
            !unresolved,
            "ambiguous mount aliases must conservatively count as in scope"
        );
    }

    fn synthetic_hint_record(pid: i32, inode: u32, name: &[u8]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&1i32.to_ne_bytes());
        payload.extend_from_slice(&2i32.to_ne_bytes());
        payload.extend_from_slice(&8u32.to_ne_bytes());
        payload.extend_from_slice(&FILEID_INO32_GEN.to_ne_bytes());
        payload.extend_from_slice(&inode.to_ne_bytes());
        payload.extend_from_slice(&1u32.to_ne_bytes());
        payload.extend_from_slice(name);
        payload.push(0);

        let info_len = std::mem::size_of::<InfoHeader>() + payload.len();
        let mut event = Vec::with_capacity(FAN_EVENT_METADATA_LEN + info_len);
        let metadata = FanotifyEventMetadata {
            event_len: (FAN_EVENT_METADATA_LEN + info_len) as u32,
            vers: FANOTIFY_METADATA_VERSION,
            reserved: 0,
            metadata_len: FAN_EVENT_METADATA_LEN as u16,
            mask: FAN_MODIFY,
            fd: -1,
            pid,
        };
        let metadata_bytes = unsafe {
            std::slice::from_raw_parts(
                (&metadata as *const FanotifyEventMetadata).cast::<u8>(),
                FAN_EVENT_METADATA_LEN,
            )
        };
        event.extend_from_slice(metadata_bytes);
        event.push(FAN_EVENT_INFO_TYPE_DFID_NAME);
        event.push(0);
        event.extend_from_slice(&(info_len as u16).to_ne_bytes());
        event.extend_from_slice(&payload);
        event
    }

    fn synthetic_overflow_record() -> Vec<u8> {
        let metadata = FanotifyEventMetadata {
            event_len: FAN_EVENT_METADATA_LEN as u32,
            vers: FANOTIFY_METADATA_VERSION,
            reserved: 0,
            metadata_len: FAN_EVENT_METADATA_LEN as u16,
            mask: FAN_Q_OVERFLOW,
            fd: -1,
            pid: 0,
        };
        unsafe {
            std::slice::from_raw_parts(
                (&metadata as *const FanotifyEventMetadata).cast::<u8>(),
                FAN_EVENT_METADATA_LEN,
            )
        }
        .to_vec()
    }

    #[test]
    fn one_identity_less_event_marks_coverage_uncertain() {
        let root = tmp("unresolved-event-root");
        let db = tmp("unresolved-event-db");
        cleanup(&root, &db);
        std::fs::create_dir_all(&root).unwrap();
        let mut store = Store::create_fresh(&db).unwrap();
        // A syntactically complete supported event with an unrecognized info
        // record models a mixed stream where this one notification cannot be
        // mapped even though neighboring notifications may resolve normally.
        let metadata = FanotifyEventMetadata {
            event_len: (FAN_EVENT_METADATA_LEN + 4) as u32,
            vers: FANOTIFY_METADATA_VERSION,
            reserved: 0,
            metadata_len: FAN_EVENT_METADATA_LEN as u16,
            mask: FAN_MODIFY,
            fd: -1,
            pid: 0,
        };
        let info = InfoHeader {
            info_type: 99,
            pad: 0,
            len: 4,
        };
        let event = unsafe {
            let mut bytes = vec![0u8; FAN_EVENT_METADATA_LEN + 4];
            std::ptr::copy_nonoverlapping(
                (&metadata as *const FanotifyEventMetadata).cast::<u8>(),
                bytes.as_mut_ptr(),
                FAN_EVENT_METADATA_LEN,
            );
            std::ptr::copy_nonoverlapping(
                (&info as *const InfoHeader).cast::<u8>(),
                bytes.as_mut_ptr().add(FAN_EVENT_METADATA_LEN),
                4,
            );
            bytes
        };
        let mut pending = HashMap::new();
        let mut born = HashSet::new();
        let mut targets = HashSet::new();
        let mut parent_paths = IndexedParentPathCache::default();
        let mut lost = false;
        let (seen, resolved, overflows) = parse_events(
            &event,
            &HashMap::new(),
            &mut HashSet::new(),
            &root,
            &db,
            &mut pending,
            &mut born,
            &mut targets,
            &mut parent_paths,
            &mut store,
            true,
            &mut lost,
        );
        assert_eq!((seen, resolved, overflows), (1, 0, 0));
        assert!(lost, "one unresolved event must invalidate coverage");
        drop(store);
        cleanup(&root, &db);
    }

    #[test]
    fn malformed_info_record_lengths_fail_closed_without_overread() {
        let root = tmp("malformed-info-root");
        let db = tmp("malformed-info-db");
        cleanup(&root, &db);
        std::fs::create_dir_all(&root).unwrap();
        let mut store = Store::create_fresh(&db).unwrap();
        for declared_len in [0, 3, u16::MAX] {
            let metadata = FanotifyEventMetadata {
                event_len: (FAN_EVENT_METADATA_LEN + 4) as u32,
                vers: FANOTIFY_METADATA_VERSION,
                reserved: 0,
                metadata_len: FAN_EVENT_METADATA_LEN as u16,
                mask: FAN_MODIFY,
                fd: -1,
                pid: 0,
            };
            let info = InfoHeader {
                info_type: FAN_EVENT_INFO_TYPE_FID,
                pad: 0,
                len: declared_len,
            };
            let mut event = vec![0u8; FAN_EVENT_METADATA_LEN + 4];
            unsafe {
                std::ptr::copy_nonoverlapping(
                    (&metadata as *const FanotifyEventMetadata).cast::<u8>(),
                    event.as_mut_ptr(),
                    FAN_EVENT_METADATA_LEN,
                );
                std::ptr::copy_nonoverlapping(
                    (&info as *const InfoHeader).cast::<u8>(),
                    event.as_mut_ptr().add(FAN_EVENT_METADATA_LEN),
                    std::mem::size_of::<InfoHeader>(),
                );
            }
            let mut pending = HashMap::new();
            let mut born = HashSet::new();
            let mut targets = HashSet::new();
            let mut parent_paths = IndexedParentPathCache::default();
            let mut lost = false;
            let result = parse_events(
                &event,
                &HashMap::new(),
                &mut HashSet::new(),
                &root,
                &db,
                &mut pending,
                &mut born,
                &mut targets,
                &mut parent_paths,
                &mut store,
                true,
                &mut lost,
            );
            assert_eq!(result, (1, 0, 0), "record length {declared_len}");
            assert!(
                lost,
                "malformed record length {declared_len} must mark loss"
            );
        }
        drop(store);
        cleanup(&root, &db);
    }

    #[test]
    fn unresolved_self_generated_attrib_does_not_poison_reconciliation() {
        let root = tmp("self-attrib-event-root");
        let db = tmp("self-attrib-event-db");
        cleanup(&root, &db);
        std::fs::create_dir_all(&root).unwrap();
        let mut store = Store::create_fresh(&db).unwrap();
        let metadata = FanotifyEventMetadata {
            event_len: (FAN_EVENT_METADATA_LEN + 4 + 24) as u32,
            vers: FANOTIFY_METADATA_VERSION,
            reserved: 0,
            metadata_len: FAN_EVENT_METADATA_LEN as u16,
            mask: FAN_ATTRIB,
            fd: -1,
            pid: std::process::id() as i32,
        };
        let info = InfoHeader {
            info_type: FAN_EVENT_INFO_TYPE_FID,
            pad: 0,
            len: 28,
        };
        let mut event = vec![0u8; metadata.event_len as usize];
        unsafe {
            std::ptr::copy_nonoverlapping(
                (&metadata as *const FanotifyEventMetadata).cast::<u8>(),
                event.as_mut_ptr(),
                FAN_EVENT_METADATA_LEN,
            );
            std::ptr::copy_nonoverlapping(
                (&info as *const InfoHeader).cast::<u8>(),
                event.as_mut_ptr().add(FAN_EVENT_METADATA_LEN),
                4,
            );
        }
        // A well-formed but unmapped target FID makes this record unresolved.
        event[FAN_EVENT_METADATA_LEN + 4..FAN_EVENT_METADATA_LEN + 8]
            .copy_from_slice(&17i32.to_ne_bytes());
        event[FAN_EVENT_METADATA_LEN + 8..FAN_EVENT_METADATA_LEN + 12]
            .copy_from_slice(&23i32.to_ne_bytes());
        event[FAN_EVENT_METADATA_LEN + 12..FAN_EVENT_METADATA_LEN + 16]
            .copy_from_slice(&8u32.to_ne_bytes());
        event[FAN_EVENT_METADATA_LEN + 16..FAN_EVENT_METADATA_LEN + 20]
            .copy_from_slice(&1i32.to_ne_bytes());
        assert!(self_generated_attrib_event(FAN_ATTRIB, metadata.pid));
        assert!(!self_generated_attrib_event(FAN_ATTRIB, 0));
        assert!(!self_generated_attrib_event(FAN_MODIFY, metadata.pid));

        let mut pending = HashMap::new();
        let mut born = HashSet::new();
        let mut targets = HashSet::new();
        let mut parent_paths = IndexedParentPathCache::default();
        let mut lost = false;
        let result = parse_events(
            &event,
            &HashMap::new(),
            &mut HashSet::new(),
            &root,
            &db,
            &mut pending,
            &mut born,
            &mut targets,
            &mut parent_paths,
            &mut store,
            true,
            &mut lost,
        );
        assert_eq!(result, (1, 0, 0));
        assert!(
            !lost,
            "a scanner-originated ATTRIB read must not poison coverage"
        );
        drop(store);
        cleanup(&root, &db);
    }

    #[test]
    fn stale_target_attrib_is_ignored_only_for_a_known_handle_capable_filesystem() {
        let fsid = (17, 23);
        let make_info_event = |info_types: &[u8], trailing: &[u8]| {
            let mut event = vec![0u8; FAN_EVENT_METADATA_LEN];
            for info_type in info_types {
                let header = InfoHeader {
                    info_type: *info_type,
                    pad: 0,
                    len: 12,
                };
                let offset = event.len();
                event.resize(offset + 12, 0);
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        (&header as *const InfoHeader).cast::<u8>(),
                        event.as_mut_ptr().add(offset),
                        std::mem::size_of::<InfoHeader>(),
                    );
                }
            }
            event.extend_from_slice(trailing);
            let metadata = FanotifyEventMetadata {
                event_len: event.len() as u32,
                vers: FANOTIFY_METADATA_VERSION,
                reserved: 0,
                metadata_len: FAN_EVENT_METADATA_LEN as u16,
                mask: FAN_ATTRIB,
                fd: -1,
                pid: 123,
            };
            unsafe {
                std::ptr::copy_nonoverlapping(
                    (&metadata as *const FanotifyEventMetadata).cast::<u8>(),
                    event.as_mut_ptr(),
                    FAN_EVENT_METADATA_LEN,
                );
            }
            event
        };
        let mut capable_fsids = HashSet::new();
        let target_only = resolve_record(
            &make_info_event(&[FAN_EVENT_INFO_TYPE_FID], &[]),
            FAN_EVENT_METADATA_LEN,
            FAN_ATTRIB,
            &HashMap::new(),
            &mut capable_fsids,
        );
        assert_eq!(target_only.info_type_mask, 1u64 << FAN_EVENT_INFO_TYPE_FID);
        assert_eq!(target_only.info_record_count, 1);
        assert!(target_only.info_records_complete);
        let compound = resolve_record(
            &make_info_event(
                &[FAN_EVENT_INFO_TYPE_FID, FAN_EVENT_INFO_TYPE_DFID_NAME],
                &[],
            ),
            FAN_EVENT_METADATA_LEN,
            FAN_ATTRIB,
            &HashMap::new(),
            &mut capable_fsids,
        );
        assert_eq!(
            compound.info_type_mask,
            (1u64 << FAN_EVENT_INFO_TYPE_FID) | (1u64 << FAN_EVENT_INFO_TYPE_DFID_NAME)
        );
        assert_eq!(compound.info_record_count, 2);
        assert!(compound.info_records_complete);
        let malformed = resolve_record(
            &make_info_event(&[FAN_EVENT_INFO_TYPE_FID], &[0, 1]),
            FAN_EVENT_METADATA_LEN,
            FAN_ATTRIB,
            &HashMap::new(),
            &mut capable_fsids,
        );
        assert!(!malformed.info_records_complete);

        let record = ResolvedEventRecord {
            entry: None,
            target_inode: None,
            stale_target_fsid: Some(fsid),
            stale_parent: None,
            info_type_mask: 1u64 << FAN_EVENT_INFO_TYPE_FID,
            info_record_count: 1,
            info_records_complete: true,
        };
        let known = HashSet::from([fsid]);
        assert!(stale_target_attrib_is_benign(FAN_ATTRIB, &record, &known));
        assert!(stale_target_attrib_is_benign(
            FAN_ATTRIB | FAN_ONDIR,
            &record,
            &known
        ));
        assert!(!stale_target_attrib_is_benign(FAN_MODIFY, &record, &known));
        assert!(!stale_target_attrib_is_benign(
            FAN_ATTRIB | FAN_CLOSE_WRITE,
            &record,
            &known
        ));
        assert!(!stale_target_attrib_is_benign(
            FAN_ATTRIB,
            &record,
            &HashSet::new()
        ));
        let compound_record = ResolvedEventRecord {
            info_type_mask: (1u64 << FAN_EVENT_INFO_TYPE_FID)
                | (1u64 << FAN_EVENT_INFO_TYPE_DFID_NAME),
            info_record_count: 2,
            ..record.clone()
        };
        assert!(!stale_target_attrib_is_benign(
            FAN_ATTRIB,
            &compound_record,
            &known
        ));
        let malformed_record = ResolvedEventRecord {
            info_records_complete: false,
            ..record
        };
        assert!(!stale_target_attrib_is_benign(
            FAN_ATTRIB,
            &malformed_record,
            &known
        ));

        let named_record = ResolvedEventRecord {
            entry: Some((PathBuf::from("/watched"), "file".into())),
            target_inode: None,
            stale_target_fsid: Some(fsid),
            stale_parent: None,
            info_type_mask: 1u64 << FAN_EVENT_INFO_TYPE_FID,
            info_record_count: 1,
            info_records_complete: true,
        };
        assert!(!stale_target_attrib_is_benign(
            FAN_ATTRIB,
            &named_record,
            &known
        ));
        assert_eq!(
            classify_handle_open_error(Some(libc::ESTALE)),
            HandleOpenError::Stale
        );
        assert_eq!(
            classify_handle_open_error(Some(libc::EACCES)),
            HandleOpenError::Other
        );
        assert_eq!(classify_handle_open_error(None), HandleOpenError::Other);
    }

    #[test]
    fn stale_parent_shape_and_ino32_handle_decoding() {
        let mut handle = 41u32.to_ne_bytes().to_vec();
        handle.extend_from_slice(&7u32.to_ne_bytes());
        assert_eq!(
            ino32_handle_identity(FILEID_INO32_GEN, &handle),
            Some((41, 7))
        );
        assert_eq!(ino32_handle_inode(FILEID_INO32_GEN, &handle), Some(41));
        assert_eq!(
            ino32_handle_inode(FILEID_INO32_GEN_PARENT, &handle),
            Some(41)
        );
        assert_eq!(ino32_handle_inode(0x81, &handle), None, "ino64 layout");
        assert_eq!(ino32_handle_inode(FILEID_INO32_GEN, &handle[..4]), None);
        let zero = [0u8; 8];
        assert_eq!(ino32_handle_inode(FILEID_INO32_GEN, &zero), None);

        let fsid = (5, 9);
        let dfid = 1u64 << FAN_EVENT_INFO_TYPE_DFID_NAME;
        let fid = 1u64 << FAN_EVENT_INFO_TYPE_FID;
        let record = ResolvedEventRecord {
            entry: None,
            target_inode: None,
            stale_target_fsid: None,
            stale_parent: Some(StaleParentEntry {
                fsid,
                handle_type: FILEID_INO32_GEN,
                handle,
                name: "child".into(),
            }),
            info_type_mask: dfid,
            info_record_count: 1,
            info_records_complete: true,
        };
        let known = HashSet::from([fsid]);
        assert!(stale_parent_event_shape_ok(&record, &known));
        let with_fid = ResolvedEventRecord {
            info_type_mask: dfid | fid,
            info_record_count: 2,
            ..record.clone()
        };
        assert!(stale_parent_event_shape_ok(&with_fid, &known));
        for rejected in [
            ResolvedEventRecord {
                info_type_mask: fid,
                ..record.clone()
            },
            ResolvedEventRecord {
                info_record_count: 2,
                ..record.clone()
            },
            ResolvedEventRecord {
                info_records_complete: false,
                ..record.clone()
            },
            ResolvedEventRecord {
                target_inode: Some((1, 2)),
                ..record.clone()
            },
            ResolvedEventRecord {
                entry: Some((PathBuf::from("/watched"), "child".into())),
                ..record.clone()
            },
            ResolvedEventRecord {
                stale_parent: None,
                ..record.clone()
            },
        ] {
            assert!(!stale_parent_event_shape_ok(&rejected, &known));
        }
        for unsafe_name in ["..", "nested/name"] {
            let mut rejected = record.clone();
            rejected.stale_parent.as_mut().unwrap().name = unsafe_name.into();
            assert!(
                !stale_parent_event_shape_ok(&rejected, &known),
                "accepted unsafe stale-parent event name {unsafe_name:?}"
            );
        }
        assert!(
            !stale_parent_event_shape_ok(&record, &HashSet::new()),
            "an unprobed filesystem must keep lost-coverage semantics"
        );
    }

    #[test]
    fn stale_parent_footprint_detects_unresolvable_inode_and_child_edges() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE inodes(dev_id INTEGER NOT NULL,inode INTEGER NOT NULL);
             CREATE TABLE dirents(parent_dev INTEGER NOT NULL,parent_inode INTEGER NOT NULL,
                                 dev_id INTEGER NOT NULL,inode INTEGER NOT NULL);",
        )
        .unwrap();
        assert!(!stale_parent_has_index_footprint(&conn, 8, 81).unwrap());

        // An inode row without a resolvable dirent path is already an index
        // footprint and must prevent the stale-event ignore optimization.
        conn.execute("INSERT INTO inodes VALUES(8,81)", []).unwrap();
        assert!(stale_parent_has_index_footprint(&conn, 8, 81).unwrap());

        conn.execute("DELETE FROM inodes WHERE dev_id=8 AND inode=81", [])
            .unwrap();
        conn.execute("INSERT INTO dirents VALUES(8,81,8,82)", [])
            .unwrap();
        assert!(stale_parent_has_index_footprint(&conn, 8, 81).unwrap());

        conn.execute("DELETE FROM dirents", []).unwrap();
        conn.execute("INSERT INTO dirents VALUES(8,80,8,81)", [])
            .unwrap();
        assert!(stale_parent_has_index_footprint(&conn, 8, 81).unwrap());
    }

    #[test]
    #[ignore = "requires handle-capable ext4; run explicitly in the ext4 qualification profile"]
    fn stale_parent_events_reconcile_indexed_paths_and_ignore_freed_unindexed_dirs() {
        use std::os::unix::ffi::OsStrExt;

        let root = tmp("stale-parent-root");
        let db = tmp("stale-parent-db");
        cleanup(&root, &db);
        std::fs::create_dir_all(root.join("gone")).unwrap();
        std::fs::write(root.join("gone/x"), b"indexed").unwrap();
        std::fs::create_dir_all(root.join("kept")).unwrap();
        let mount_fd = RawFdGuard(open_dir(&root).unwrap());
        let mut fs = std::mem::MaybeUninit::<libc::statfs>::uninit();
        assert_eq!(unsafe { libc::fstatfs(mount_fd.0, fs.as_mut_ptr()) }, 0);
        let ext4_supported = unsafe { fs.assume_init() }.f_type as u64 == EXT4_SUPER_MAGIC
            && probe_handle_capability(&root, mount_fd.0);
        if !ext4_supported {
            drop(mount_fd);
            cleanup(&root, &db);
            panic!("explicit stale-parent integration test requires handle-capable ext4 TMPDIR");
        }
        scan_into(&root, &db);
        // Created after the scan, so never committed to the index.
        std::fs::create_dir_all(root.join("transient")).unwrap();

        let handle_of = |path: &Path| -> FileHandle {
            let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            let mut handle = FileHandle {
                handle_bytes: MAX_HANDLE_SZ as u32,
                handle_type: 0,
                f_handle: [0; MAX_HANDLE_SZ],
            };
            let mut mount_id = 0;
            let rc = unsafe {
                libc::name_to_handle_at(
                    libc::AT_FDCWD,
                    c.as_ptr(),
                    (&mut handle as *mut FileHandle).cast::<libc::file_handle>(),
                    &mut mount_id,
                    0,
                )
            };
            assert_eq!(rc, 0, "name_to_handle_at {}", path.display());
            handle
        };
        let fsid = statfs_fsid(&root).unwrap();
        let dfid_name_event = |mask: u64, handle: &FileHandle, name: &[u8]| -> Vec<u8> {
            let hb = handle.handle_bytes as usize;
            let mut payload = Vec::new();
            payload.extend_from_slice(&fsid.0.to_ne_bytes());
            payload.extend_from_slice(&fsid.1.to_ne_bytes());
            payload.extend_from_slice(&handle.handle_bytes.to_ne_bytes());
            payload.extend_from_slice(&handle.handle_type.to_ne_bytes());
            payload.extend_from_slice(&handle.f_handle[..hb]);
            payload.extend_from_slice(name);
            payload.push(0);
            while (4 + payload.len()) % 4 != 0 {
                payload.push(0);
            }
            let info = InfoHeader {
                info_type: FAN_EVENT_INFO_TYPE_DFID_NAME,
                pad: 0,
                len: (4 + payload.len()) as u16,
            };
            let metadata = FanotifyEventMetadata {
                event_len: (FAN_EVENT_METADATA_LEN + 4 + payload.len()) as u32,
                vers: FANOTIFY_METADATA_VERSION,
                reserved: 0,
                metadata_len: FAN_EVENT_METADATA_LEN as u16,
                mask,
                fd: -1,
                pid: 123,
            };
            let mut event = vec![0u8; FAN_EVENT_METADATA_LEN + 4];
            unsafe {
                std::ptr::copy_nonoverlapping(
                    (&metadata as *const FanotifyEventMetadata).cast::<u8>(),
                    event.as_mut_ptr(),
                    FAN_EVENT_METADATA_LEN,
                );
                std::ptr::copy_nonoverlapping(
                    (&info as *const InfoHeader).cast::<u8>(),
                    event.as_mut_ptr().add(FAN_EVENT_METADATA_LEN),
                    4,
                );
            }
            event.extend_from_slice(&payload);
            event
        };
        let gone = handle_of(&root.join("gone"));
        let transient = handle_of(&root.join("transient"));
        let kept = handle_of(&root.join("kept"));
        let kept_identity = ino32_handle_identity(
            kept.handle_type,
            &kept.f_handle[..kept.handle_bytes as usize],
        )
        .expect("ext4 directory handle includes inode generation");
        let kept_path = root.join("kept");
        let kept_stat = std::fs::symlink_metadata(&kept_path).unwrap();
        let (kept_dev, kept_ino) = crate::util::indexed_identity(&kept_stat).unwrap();
        assert!(ext4_path_matches_handle(
            &kept_path,
            kept_dev,
            kept_ino,
            kept_identity.1
        ));
        assert!(!ext4_path_matches_handle(
            &kept_path,
            kept_dev,
            kept_ino,
            kept_identity.1.wrapping_add(1)
        ));
        std::fs::remove_dir_all(root.join("gone")).unwrap();
        std::fs::remove_dir(root.join("transient")).unwrap();
        if ino32_handle_inode(
            gone.handle_type,
            &gone.f_handle[..gone.handle_bytes as usize],
        )
        .is_none()
        {
            drop(mount_fd);
            cleanup(&root, &db);
            panic!("explicit stale-parent integration test requires ext4 ino32 handles");
        }

        let fsfds = HashMap::from([(fsid, mount_fd.0)]);
        let mut store = Store::open_rw(&db).unwrap();
        let run = |events: &[Vec<u8>], capable: HashSet<(i32, i32)>, store: &mut Store| {
            let buf: Vec<u8> = events.concat();
            let mut capable = capable;
            let mut pending = HashMap::new();
            let mut born = HashSet::new();
            let mut targets = HashSet::new();
            let mut parent_paths = IndexedParentPathCache::default();
            let mut lost = false;
            parse_events(
                &buf,
                &fsfds,
                &mut capable,
                &root,
                &db,
                &mut pending,
                &mut born,
                &mut targets,
                &mut parent_paths,
                store,
                true,
                &mut lost,
            );
            (pending, lost)
        };

        // Indexed parent whose handle went stale: reconcile its child by path.
        let (pending, lost) = run(
            &[dfid_name_event(FAN_DELETE, &gone, b"x")],
            HashSet::from([fsid]),
            &mut store,
        );
        assert!(
            !lost,
            "an indexed stale parent is repaired by path, not lost"
        );
        assert_eq!(pending.get(&root.join("gone/x")), Some(&Op::Reconcile));

        // A stale handle whose inode number aliases a live path but whose
        // generation differs must not reconcile against that replacement.
        let mut reused = kept;
        let stale_generation = kept_identity.1.wrapping_add(1);
        reused.f_handle[4..8].copy_from_slice(&stale_generation.to_ne_bytes());
        let (pending, lost) = run(
            &[dfid_name_event(FAN_DELETE, &reused, b"x")],
            HashSet::from([fsid]),
            &mut store,
        );
        assert!(
            lost,
            "inode reuse with a different generation must stay uncertain"
        );
        assert!(
            pending.is_empty(),
            "unsafe replacement path was queued: {pending:?}"
        );

        // An orphaned parent identity is different from a never-indexed
        // temporary directory. Removing its path edge while leaving its inode
        // and child rows simulates a partial index update; the stale event must
        // keep coverage uncertain instead of taking the ignore branch.
        let (gone_dev, gone_ino): (i64, i64) = store
            .conn
            .query_row(
                "SELECT dev_id,inode FROM dirents WHERE name=?1 LIMIT 1",
                [&b"gone"[..]],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        store
            .conn
            .execute(
                "DELETE FROM dirents WHERE dev_id=?1 AND inode=?2",
                params![gone_dev, gone_ino],
            )
            .unwrap();
        let (_, lost) = run(
            &[dfid_name_event(FAN_DELETE, &gone, b"x")],
            HashSet::from([fsid]),
            &mut store,
        );
        assert!(
            lost,
            "an unresolvable parent with indexed rows must not be ignored"
        );

        // Freed directory that was never indexed: nothing indexed can be under it.
        let (pending, lost) = run(
            &[
                dfid_name_event(FAN_CREATE, &transient, b"y"),
                dfid_name_event(FAN_DELETE, &transient, b"y"),
                dfid_name_event(FAN_CREATE | FAN_ONDIR, &transient, b"z"),
            ],
            HashSet::from([fsid]),
            &mut store,
        );
        assert!(!lost);
        assert!(pending.is_empty(), "unexpected pending: {pending:?}");

        // A live parent still resolves normally and is unaffected.
        let (pending, lost) = run(
            &[dfid_name_event(FAN_CREATE, &kept, b"new")],
            HashSet::from([fsid]),
            &mut store,
        );
        assert!(!lost);
        assert!(pending.contains_key(&root.join("kept/new")));

        // Without a prior successful handle round trip on this filesystem the
        // stale parent proves nothing, so coverage must stay uncertain.
        let (_, lost) = run(
            &[dfid_name_event(FAN_DELETE, &gone, b"x")],
            HashSet::new(),
            &mut store,
        );
        assert!(lost, "an unprobed filesystem keeps lost-coverage semantics");

        drop(store);
        drop(mount_fd);
        cleanup(&root, &db);
    }

    #[test]
    fn startup_handle_probe_prequalifies_stale_attrib_filter() {
        use std::os::unix::ffi::OsStrExt;

        let root = tmp("handle-capability-probe");
        let db = tmp("handle-capability-probe-db");
        cleanup(&root, &db);
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("deleted-target");
        std::fs::write(&target, b"short-lived").unwrap();
        let mount_fd = RawFdGuard(open_dir(&root).unwrap());
        let root_probe_succeeded = probe_handle_capability(&root, mount_fd.0);
        if !root_probe_succeeded {
            eprintln!("skipping stale-FID order check: filesystem cannot round-trip file handles");
            drop(mount_fd);
            cleanup(&root, &db);
            return;
        }

        let path = std::ffi::CString::new(target.as_os_str().as_bytes()).unwrap();
        let mut target_handle = FileHandle {
            handle_bytes: MAX_HANDLE_SZ as u32,
            handle_type: 0,
            f_handle: [0; MAX_HANDLE_SZ],
        };
        let mut mount_id = 0;
        let named = unsafe {
            libc::name_to_handle_at(
                libc::AT_FDCWD,
                path.as_ptr(),
                (&mut target_handle as *mut FileHandle).cast::<libc::file_handle>(),
                &mut mount_id,
                0,
            )
        };
        if named != 0 {
            eprintln!("skipping stale-FID order check: target handle is unsupported");
            drop(mount_fd);
            cleanup(&root, &db);
            return;
        }
        let fsid = statfs_fsid(&root).unwrap();
        let handle_bytes = target_handle.handle_bytes as usize;
        assert!((1..=MAX_HANDLE_SZ).contains(&handle_bytes));
        std::fs::remove_file(&target).unwrap();

        // Build a target-only FAN_ATTRIB carrying a handle that became stale
        // after unlink. The startup probe must make this benign before any
        // ordinary named event has populated the runtime capability set.
        let info_len = 4 + 8 + 4 + 4 + handle_bytes;
        let metadata = FanotifyEventMetadata {
            event_len: (FAN_EVENT_METADATA_LEN + info_len) as u32,
            vers: FANOTIFY_METADATA_VERSION,
            reserved: 0,
            metadata_len: FAN_EVENT_METADATA_LEN as u16,
            mask: FAN_ATTRIB,
            fd: -1,
            pid: 123,
        };
        let info = InfoHeader {
            info_type: FAN_EVENT_INFO_TYPE_FID,
            pad: 0,
            len: info_len as u16,
        };
        let mut event = vec![0u8; metadata.event_len as usize];
        unsafe {
            std::ptr::copy_nonoverlapping(
                (&metadata as *const FanotifyEventMetadata).cast::<u8>(),
                event.as_mut_ptr(),
                FAN_EVENT_METADATA_LEN,
            );
            std::ptr::copy_nonoverlapping(
                (&info as *const InfoHeader).cast::<u8>(),
                event.as_mut_ptr().add(FAN_EVENT_METADATA_LEN),
                std::mem::size_of::<InfoHeader>(),
            );
        }
        let payload = FAN_EVENT_METADATA_LEN + std::mem::size_of::<InfoHeader>();
        event[payload..payload + 4].copy_from_slice(&fsid.0.to_ne_bytes());
        event[payload + 4..payload + 8].copy_from_slice(&fsid.1.to_ne_bytes());
        event[payload + 8..payload + 12].copy_from_slice(&target_handle.handle_bytes.to_ne_bytes());
        event[payload + 12..payload + 16].copy_from_slice(&target_handle.handle_type.to_ne_bytes());
        event[payload + 16..payload + 16 + handle_bytes]
            .copy_from_slice(&target_handle.f_handle[..handle_bytes]);

        let mut store = Store::create_fresh(&db).unwrap();
        let mut pending = HashMap::new();
        let mut born = HashSet::new();
        let mut targets = HashSet::new();
        let mut parent_paths = IndexedParentPathCache::default();
        let mut lost = false;
        let fsfds = HashMap::from([(fsid, mount_fd.0)]);
        let mut capable_fsids = HashSet::from([fsid]);
        let result = parse_events(
            &event,
            &fsfds,
            &mut capable_fsids,
            &root,
            &db,
            &mut pending,
            &mut born,
            &mut targets,
            &mut parent_paths,
            &mut store,
            true,
            &mut lost,
        );
        assert_eq!(result, (1, 0, 0));
        assert!(!lost, "prequalified stale ATTRIB must not poison coverage");

        // Without the round-trip proof, the exact same event remains uncertain.
        lost = false;
        let mut unknown_fsids = HashSet::new();
        let mut parent_paths = IndexedParentPathCache::default();
        let result = parse_events(
            &event,
            &fsfds,
            &mut unknown_fsids,
            &root,
            &db,
            &mut pending,
            &mut born,
            &mut targets,
            &mut parent_paths,
            &mut store,
            true,
            &mut lost,
        );
        assert_eq!(result, (1, 0, 0));
        assert!(lost, "unqualified stale ATTRIB must remain conservative");

        drop(store);
        drop(mount_fd);
        cleanup(&root, &db);
    }

    #[test]
    fn unresolved_event_diagnostics_are_rate_limited_with_suppressed_count() {
        let last_logged_at = AtomicU64::new(0);
        let suppressed = AtomicU64::new(0);

        assert_eq!(
            unresolved_event_log_permit_at(1_000, &last_logged_at, &suppressed),
            Some(0)
        );
        assert_eq!(
            unresolved_event_log_permit_at(1_001, &last_logged_at, &suppressed),
            None
        );
        assert_eq!(
            unresolved_event_log_permit_at(
                1_000 + UNRESOLVED_EVENT_LOG_INTERVAL_SECS - 1,
                &last_logged_at,
                &suppressed,
            ),
            None
        );
        assert_eq!(suppressed.load(Ordering::Relaxed), 2);
        assert_eq!(
            unresolved_event_log_permit_at(
                1_000 + UNRESOLVED_EVENT_LOG_INTERVAL_SECS,
                &last_logged_at,
                &suppressed,
            ),
            Some(2)
        );
        assert_eq!(suppressed.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn bounded_fanotify_parse_returns_ordered_suffix_for_later_ticks() {
        let root = tmp("bounded-parse-root");
        let db = tmp("bounded-parse-db");
        cleanup(&root, &db);
        std::fs::create_dir_all(&root).unwrap();
        let mut store = Store::create_fresh(&db).unwrap();

        let make_event = || {
            let metadata = FanotifyEventMetadata {
                event_len: FAN_EVENT_METADATA_LEN as u32,
                vers: FANOTIFY_METADATA_VERSION,
                reserved: 0,
                metadata_len: FAN_EVENT_METADATA_LEN as u16,
                mask: 0,
                fd: -1,
                pid: 0,
            };
            unsafe {
                std::slice::from_raw_parts(
                    (&metadata as *const FanotifyEventMetadata).cast::<u8>(),
                    FAN_EVENT_METADATA_LEN,
                )
            }
            .to_vec()
        };
        let first = make_event();
        let second = make_event();
        let mut input = first.clone();
        input.extend_from_slice(&second);

        let mut pending = HashMap::new();
        let mut born = HashSet::new();
        let mut targets = HashSet::new();
        let mut parent_paths = IndexedParentPathCache::default();
        let mut lost = false;
        let parsed = parse_events_bounded(
            &input,
            &HashMap::new(),
            &mut HashSet::new(),
            &root,
            &db,
            &mut pending,
            &mut born,
            &mut targets,
            &mut parent_paths,
            &mut store,
            true,
            &mut lost,
            1,
            Instant::now() + Duration::from_secs(1),
        );
        assert_eq!(parsed, (1, 0, 0, first.len()));
        assert_eq!(&input[parsed.3..], second.as_slice());
        assert!(lost, "synthetic identity-less event must latch uncertainty");

        let deferred = &input[parsed.3..];
        let yielded = parse_events_bounded(
            deferred,
            &HashMap::new(),
            &mut HashSet::new(),
            &root,
            &db,
            &mut pending,
            &mut born,
            &mut targets,
            &mut parent_paths,
            &mut store,
            true,
            &mut lost,
            1,
            Instant::now() - Duration::from_millis(1),
        );
        assert_eq!(yielded, (0, 0, 0, 0));
        assert_eq!(deferred, second.as_slice());
        assert!(lost, "yielding cannot clear prior uncertainty");

        let resumed = parse_events_bounded(
            deferred,
            &HashMap::new(),
            &mut HashSet::new(),
            &root,
            &db,
            &mut pending,
            &mut born,
            &mut targets,
            &mut parent_paths,
            &mut store,
            true,
            &mut lost,
            1,
            Instant::now() + Duration::from_secs(1),
        );
        assert_eq!(resumed, (1, 0, 0, second.len()));
        assert!(lost, "resuming cannot clear prior uncertainty");
        drop(store);
        cleanup(&root, &db);
    }

    #[test]
    fn committed_root_gate_opens_only_after_a_baseline_is_installed() {
        let dir = tmp("committed-root-gate");
        let db = tmp("committed-root-gate-db");
        cleanup(&dir, &db);
        std::fs::create_dir_all(&dir).unwrap();

        let empty = Store::open_rw(&db).unwrap();
        assert!(!committed_root_indexed(&empty).unwrap());
        drop(empty);

        scan_into(&dir, &db);
        let indexed = Store::open_rw(&db).unwrap();
        assert!(committed_root_indexed(&indexed).unwrap());
        drop(indexed);
        cleanup(&dir, &db);
    }

    #[test]
    fn target_identity_is_resolved_only_when_path_metadata_is_insufficient() {
        assert!(!needs_target_identity(FAN_CREATE, true));
        assert!(!needs_target_identity(FAN_MOVED_FROM, true));
        assert!(!needs_target_identity(FAN_MOVED_TO, true));
        assert!(!needs_target_identity(FAN_MODIFY, true));
        assert!(!needs_target_identity(FAN_ATTRIB | FAN_CLOSE_WRITE, true));

        // Target-only metadata events need identity to find every indexed link.
        assert!(needs_target_identity(FAN_MODIFY, false));
        // A delete can remove its named path before flush, so retain the FID for
        // refreshing hardlinks that survive the deleted alias.
        assert!(needs_target_identity(FAN_DELETE, true));
        assert!(needs_target_identity(FAN_DELETE, false));
    }

    #[test]
    fn accounting_check_yields_to_watcher_work_or_critical_pressure() {
        assert!(!accounting_check_should_cancel(
            crate::guard::Pressure::Normal,
            true,
            0,
            0,
            false,
        ));
        assert!(accounting_check_should_cancel(
            crate::guard::Pressure::Normal,
            false,
            0,
            0,
            false,
        ));
        assert!(accounting_check_should_cancel(
            crate::guard::Pressure::Normal,
            true,
            1,
            0,
            false,
        ));
        assert!(accounting_check_should_cancel(
            crate::guard::Pressure::Critical,
            true,
            0,
            0,
            false,
        ));
        assert!(
            !accounting_check_should_cancel(
                crate::guard::Pressure::Normal,
                false,
                0,
                0,
                true,
            ),
            "scan-first accounting may verify a last-observed view while irrelevant kernel hints drain"
        );
        assert!(accounting_check_should_cancel(
            crate::guard::Pressure::Critical,
            false,
            0,
            0,
            true,
        ));
    }

    #[test]
    fn background_accounting_check_uses_a_read_only_snapshot() {
        let dir = tmp("accounting-worker");
        let db = tmp("accounting-worker-db");
        cleanup(&dir, &db);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sample"), b"accounted").unwrap();
        scan_into(&dir, &db);

        let check = start_accounting_check(&db, AccountingCheckKind::Periodic).unwrap();
        assert_eq!(
            check.join.join().unwrap().unwrap(),
            Some(true),
            "the asynchronous verifier should validate the scanned snapshot"
        );
        let store = Store::open_rw(&db).unwrap();
        assert!(store.totals_consistent().unwrap());
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn background_accounting_check_cancels_an_active_large_snapshot() {
        let (dir, db, mut store) = fixture("accounting-worker-cancel");
        let (root_dev, root_inode) = id_of(&dir);
        let root_blocks: i64 = store
            .conn
            .query_row(
                "SELECT blocks FROM inodes WHERE dev_id=?1 AND inode=?2",
                params![root_dev, root_inode],
                |row| row.get(0),
            )
            .unwrap();

        // A large but compact synthetic tree keeps this test deterministic and
        // exercises cancellation inside SQLite's progress handler without
        // requiring hundreds of thousands of filesystem mutations.
        const CHILDREN: i64 = 500_000;
        let tx = store.conn.transaction().unwrap();
        tx.execute(
            "WITH RECURSIVE seq(x) AS (
                 VALUES(0) UNION ALL SELECT x+1 FROM seq WHERE x+1 < ?1
             )
             INSERT INTO inodes
                 (dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime,birth_ns)
             SELECT ?2,9223372036854775807-x,'f',4096,4096,1,0,0,0 FROM seq",
            params![CHILDREN, root_dev],
        )
        .unwrap();
        tx.execute(
            "WITH RECURSIVE seq(x) AS (
                 VALUES(0) UNION ALL SELECT x+1 FROM seq WHERE x+1 < ?1
             )
             INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
             SELECT ?2,?3,CAST('synthetic_'||printf('%015x',x) AS BLOB),
                    ?2,9223372036854775807-x,1 FROM seq",
            params![CHILDREN, root_dev, root_inode],
        )
        .unwrap();
        tx.execute(
            "UPDATE inodes SET recursive_bytes=?3,recursive_inodes=?4
             WHERE dev_id=?1 AND inode=?2",
            params![
                root_dev,
                root_inode,
                root_blocks + CHILDREN * 4096,
                CHILDREN + 1
            ],
        )
        .unwrap();
        tx.commit().unwrap();
        drop(store);

        let check = start_accounting_check(&db, AccountingCheckKind::Periodic).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            !check.join.is_finished(),
            "the 10%-CPU verifier should still be working on this large snapshot"
        );

        let cancelled_at = Instant::now();
        check.cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            check.join.join().unwrap().unwrap(),
            None,
            "an interrupted read-only verification must be discarded, not reported as a mismatch"
        );
        assert!(
            cancelled_at.elapsed() < Duration::from_secs(2),
            "the SQLite progress handler should interrupt the snapshot promptly"
        );

        cleanup(&dir, &db);
    }

    #[test]
    fn active_reconciliation_caps_pressure_sampling_interval() {
        let flush = Duration::from_secs(2);
        assert_eq!(watcher_poll_timeout(flush, Duration::ZERO, true), 100);
        assert_eq!(
            watcher_poll_timeout(flush, Duration::from_millis(500), true),
            100
        );
        assert_eq!(
            watcher_poll_timeout(flush, Duration::from_millis(500), false),
            1500
        );
        assert_eq!(watcher_poll_timeout(flush, Duration::from_secs(3), true), 0);
    }

    #[test]
    fn mount_topology_tracks_pseudo_and_duplicate_view_mounts() {
        let info = concat!(
            "10 1 8:1 / / rw - ext4 /dev/root rw\n",
            "11 10 8:2 / /srv/data rw - xfs /dev/data rw\n",
            "12 11 0:5 / /srv/data/proc rw - proc proc rw\n",
            "13 11 0:6 / /srv/data/container\\040view rw - overlay overlay rw\n",
            "14 11 0:7 / /elsewhere rw - tmpfs tmpfs rw\n",
        );

        let mounts = mount_topology_from_info(Path::new("/srv/data"), false, info);
        assert_eq!(mounts.len(), 4);
        assert!(mounts
            .iter()
            .any(|mount| mount.ends_with("/srv/data/proc proc")));
        assert!(mounts
            .iter()
            .any(|mount| mount.ends_with("/srv/data/container\\040view overlay")));
        assert!(!mounts.iter().any(|mount| mount.contains("/elsewhere")));
    }

    #[test]
    fn watcher_skips_real_mounts_hidden_below_excluded_mounts() {
        let info = concat!(
            "10 1 8:1 / / rw - ext4 /dev/root rw\n",
            "11 10 0:42 / /run rw - tmpfs tmpfs rw\n",
            "12 11 8:3 / /run/dux-inspection rw - xfs /dev/nbd0p4 ro\n",
            "13 10 8:4 / /srv rw - xfs /dev/data rw\n",
            "14 13 0:43 / /srv/merged rw - overlay overlay rw\n",
            "15 14 8:5 / /srv/merged/child rw - ext4 /dev/child rw\n",
        );
        let mounts = watchable_mounts_from_info(Path::new("/"), info);
        assert!(mounts.contains(&PathBuf::from("/srv")));
        assert!(!mounts.contains(&PathBuf::from("/run/dux-inspection")));
        assert!(!mounts.contains(&PathBuf::from("/srv/merged")));
        assert!(!mounts.contains(&PathBuf::from("/srv/merged/child")));

        // An explicitly indexed pseudo root may still contain watchable real
        // submounts; only ancestors below the configured root are pruning edges.
        let explicit_root = watchable_mounts_from_info(
            Path::new("/run"),
            concat!(
                "10 1 0:42 / /run rw - tmpfs tmpfs rw\n",
                "12 10 8:3 / /run/dux-inspection rw - xfs /dev/nbd0p4 ro\n",
            ),
        );
        assert_eq!(explicit_root, vec![PathBuf::from("/run/dux-inspection")]);
    }

    #[test]
    fn non_watched_mount_changes_reconcile_without_restarting_watch_coverage() {
        let before = vec!["10 8:1 / /srv/data xfs".to_owned()];
        let after = vec![
            "10 8:1 / /srv/data xfs".to_owned(),
            "12 0:5 / /srv/data/proc proc".to_owned(),
            "13 0:6 / /srv/data/container overlay".to_owned(),
        ];

        assert!(!mount_topology_requires_watch_refresh(&before, &after));
        assert!(mount_topology_requires_visible_reconcile(&before, &after));
        assert_eq!(
            mount_topology_action(&before, &after),
            MountTopologyAction::ReconcileVisibleTree
        );
        assert!(mount_topology_requires_watch_refresh(
            &before,
            &[
                "10 8:1 / /srv/data xfs".to_owned(),
                "14 8:2 / /srv/data/extra ext4".to_owned()
            ]
        ));
    }

    #[test]
    fn mount_churn_beneath_excluded_pseudo_subtree_does_not_reconcile() {
        let before = concat!(
            "10 1 8:1 / / rw - ext4 /dev/root rw\n",
            "11 10 0:42 / /run rw - tmpfs tmpfs rw\n",
            "12 11 8:3 / /run/dux-fixture rw - ext4 /dev/loop0 ro\n",
        );
        let after = concat!(
            "10 1 8:1 / / rw - ext4 /dev/root rw\n",
            "11 10 0:42 / /run rw - tmpfs tmpfs rw\n",
            "13 11 8:4 / /run/dux-fixture rw - xfs /dev/nbd0p4 ro\n",
        );
        let before = mount_topology_from_info(Path::new("/"), false, before);
        let after = mount_topology_from_info(Path::new("/"), false, after);

        assert_eq!(before, after);
        assert_eq!(
            mount_topology_action(&before, &after),
            MountTopologyAction::DuplicateStorageViewOnly
        );
    }

    #[test]
    fn explicit_pseudo_root_still_tracks_indexed_real_submounts() {
        let info = concat!(
            "10 1 0:42 / /run rw - tmpfs tmpfs rw\n",
            "11 10 8:3 / /run/dux-fixture rw - ext4 /dev/loop0 ro\n",
        );
        let mounts = mount_topology_from_info(Path::new("/run"), false, info);
        assert_eq!(mounts.len(), 2);
        assert!(mounts.iter().any(|mount| mount.ends_with("/run tmpfs")));
        assert!(mounts
            .iter()
            .any(|mount| mount.ends_with("/run/dux-fixture ext4")));
    }

    #[test]
    fn duplicate_overlay_mount_churn_does_not_reconcile_indexed_tree() {
        let before = vec![
            "10 8:1 / /srv/data xfs".to_owned(),
            "12 0:6 / /srv/data/container-a overlay".to_owned(),
        ];
        let after = vec![
            "10 8:1 / /srv/data xfs".to_owned(),
            "13 0:7 / /srv/data/container-b overlay".to_owned(),
        ];

        assert!(!mount_topology_requires_watch_refresh(&before, &after));
        assert!(!mount_topology_requires_visible_reconcile(&before, &after));
        assert_eq!(
            mount_topology_action(&before, &after),
            MountTopologyAction::DuplicateStorageViewOnly
        );
    }

    #[test]
    fn one_filesystem_mount_topology_tracks_root_but_not_descendants() {
        let info = concat!(
            "10 1 8:1 / / rw - ext4 /dev/root rw\n",
            "11 10 8:2 / /srv/data rw - xfs /dev/data rw\n",
            "12 11 0:5 / /srv/data/proc rw - proc proc rw\n",
        );

        let mounts = mount_topology_from_info(Path::new("/srv/data"), true, info);
        assert_eq!(mounts.len(), 2);
        assert!(mounts.iter().any(|mount| mount.ends_with("/srv/data xfs")));
        assert!(!mounts
            .iter()
            .any(|mount| mount.ends_with("/srv/data/proc proc")));
    }

    #[test]
    fn reconciliation_never_refreshes_caught_up_clock() {
        assert!(!pipeline_is_caught_up(
            true, true, true, true, true, false, false,
        ));
        assert!(!pipeline_is_caught_up(
            false, true, true, true, true, true, false,
        ));
        assert!(!pipeline_is_caught_up(
            false, true, true, true, true, false, true,
        ));
        assert!(!pipeline_is_caught_up(
            false, false, true, true, true, false, false,
        ));
        assert!(!pipeline_is_caught_up(
            false, true, false, true, true, false, false,
        ));
        assert!(!pipeline_is_caught_up(
            false, true, true, false, true, false, false,
        ));
        assert!(!pipeline_is_caught_up(
            false, true, true, true, false, false, false,
        ));
        assert!(pipeline_is_caught_up(
            false, true, true, true, true, false, false,
        ));
    }

    #[test]
    fn persistent_backlog_can_start_reconciliation_but_retries_are_bounded() {
        assert!(automatic_reconciliation_request_allowed(
            false, false, false, true,
        ));
        assert!(!automatic_reconciliation_request_allowed(
            true, false, false, true,
        ));
        assert!(!automatic_reconciliation_request_allowed(
            false, true, false, true,
        ));
        assert!(!automatic_reconciliation_request_allowed(
            false, false, true, true,
        ));
        assert!(!automatic_reconciliation_request_allowed(
            false, false, false, false,
        ));
        assert!(!reconciliation_scan_ready(
            false,
            true,
            Duration::from_secs(600),
            true,
        ));
        assert!(!reconciliation_scan_ready(
            true,
            false,
            Duration::from_secs(29),
            true,
        ));
        assert!(!reconciliation_scan_ready(
            true,
            true,
            Duration::ZERO,
            false,
        ));
        assert!(reconciliation_scan_ready(true, true, Duration::ZERO, true,));
        assert!(reconciliation_scan_ready(
            true,
            false,
            Duration::from_secs(30),
            true,
        ));
        assert_eq!(reconciliation_retry_delay(1), Duration::from_secs(60));
        assert_eq!(reconciliation_retry_delay(2), Duration::from_secs(120));
        assert!(!automatic_reconciliation_circuit_open(1));
        assert!(automatic_reconciliation_circuit_open(2));
        assert!(!reconciliation_retry_should_pause(true, u32::MAX));
        assert!(reconciliation_retry_should_pause(false, u32::MAX));
        assert!(!reconciliation_retry_circuit_blocks_request(false, false));
        assert!(reconciliation_retry_circuit_blocks_request(true, false));
        assert!(!reconciliation_retry_circuit_blocks_request(true, true));
        assert_eq!(
            bounded_retry_remaining_seconds(Some("150"), 100, Duration::from_secs(120)),
            (50, false)
        );
        assert_eq!(
            bounded_retry_remaining_seconds(Some("90"), 100, Duration::from_secs(120)),
            (0, false)
        );
        assert_eq!(
            bounded_retry_remaining_seconds(Some("1000"), 100, Duration::from_secs(120)),
            (120, false)
        );
        assert_eq!(
            bounded_retry_remaining_seconds(Some("corrupt"), 100, Duration::from_secs(120)),
            (120, true)
        );
        assert_eq!(
            bounded_retry_remaining_seconds(None, 100, Duration::from_secs(120)),
            (60, false)
        );
        assert_eq!(
            reconciliation_retry_remaining_seconds(Some("150"), 100),
            Some(50)
        );
        assert_eq!(
            reconciliation_retry_remaining_seconds(Some("90"), 100),
            Some(0)
        );
        assert_eq!(
            reconciliation_retry_remaining_seconds(Some("corrupt"), 100),
            None
        );
    }

    #[test]
    fn malformed_reconciliation_retry_state_fails_closed() {
        assert_eq!(parse_reconciliation_attempts(None), (0, false));
        assert_eq!(parse_reconciliation_attempts(Some("1")), (1, false));
        for corrupt in ["", "-1", "not-a-number", "4294967296"] {
            let (attempts, corrupt_state) = parse_reconciliation_attempts(Some(corrupt));
            assert!(corrupt_state, "accepted malformed retry count {corrupt:?}");
            assert_eq!(attempts, u32::MAX);
            assert!(automatic_reconciliation_circuit_open(attempts));
            assert_eq!(
                reconciliation_retry_delay(attempts),
                Duration::from_secs(30_720)
            );
        }
    }

    #[test]
    fn persisted_reconciliation_retry_deadline_survives_restart_bounded() {
        let dir = tmp("retry-deadline");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("index.db");
        let store = Store::open_rw(&db).unwrap();
        let now = crate::util::now_secs();

        // Legacy indexes may have failed-attempt metadata without a persisted
        // deadline. Upgrade with only the short base delay, not the 8.5-hour
        // maximum delay implied by a large historic attempt count.
        let restored = restore_reconciliation_retry_deadline(&store, 2, true).unwrap();
        let remaining = restored.saturating_duration_since(Instant::now()).as_secs();
        assert!((45..=60).contains(&remaining));
        let normalized = store
            .get_meta(RECONCILIATION_RETRY_AFTER_META)
            .unwrap()
            .unwrap()
            .parse::<i64>()
            .unwrap();
        assert!(normalized <= crate::util::now_secs() + 60);

        store
            .set_meta(RECONCILIATION_RETRY_AFTER_META, &(now + 50).to_string())
            .unwrap();
        let restored = restore_reconciliation_retry_deadline(&store, 2, true).unwrap();
        let remaining = restored.saturating_duration_since(Instant::now()).as_secs();
        assert!((48..=50).contains(&remaining));

        store
            .set_meta(RECONCILIATION_RETRY_AFTER_META, &(now + 10_000).to_string())
            .unwrap();
        let restored = restore_reconciliation_retry_deadline(&store, 2, true).unwrap();
        let remaining = restored.saturating_duration_since(Instant::now()).as_secs();
        assert!((118..=120).contains(&remaining));
        let normalized = store
            .get_meta(RECONCILIATION_RETRY_AFTER_META)
            .unwrap()
            .unwrap()
            .parse::<i64>()
            .unwrap();
        assert!(normalized <= crate::util::now_secs() + 120);

        store
            .set_meta(RECONCILIATION_RETRY_AFTER_META, "invalid")
            .unwrap();
        let restored = restore_reconciliation_retry_deadline(&store, u32::MAX, true).unwrap();
        let remaining = restored.saturating_duration_since(Instant::now()).as_secs();
        assert!((30_718..=30_720).contains(&remaining));

        store
            .set_meta(RECONCILIATION_RETRY_AFTER_META, "legacy-read-only")
            .unwrap();
        let restored = restore_reconciliation_retry_deadline(&store, 2, false).unwrap();
        let remaining = restored.saturating_duration_since(Instant::now()).as_secs();
        assert!((118..=120).contains(&remaining));
        assert_eq!(
            store
                .get_meta(RECONCILIATION_RETRY_AFTER_META)
                .unwrap()
                .as_deref(),
            Some("legacy-read-only")
        );

        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn reconciliation_pressure_deferral_is_reported_once_per_episode() {
        use crate::guard::Pressure;

        // A default Health is deliberately fail-closed because it has no
        // available-memory or disk telemetry. Supply a genuinely healthy
        // sample so the Normal-pressure transition below tests deferral
        // reporting instead of the missing-telemetry safety path.
        let health = crate::guard::Health {
            mem_avail: 8 << 30,
            mem_total: 16 << 30,
            disk_avail: 20 << 30,
            load1: 1.0,
            ncpu: 12.0,
            ..Default::default()
        };
        assert_eq!(
            reconciliation_deferral_transition(false, Pressure::Elevated, &health, MIN_FREE_BYTES),
            (true, true)
        );
        let cpu_elevated = crate::guard::Health {
            mem_avail: 8 << 30,
            mem_total: 16 << 30,
            disk_avail: 20 << 30,
            load1: 1.0,
            ncpu: 12.0,
            psi_cpu10: 25.0,
            ..Default::default()
        };
        assert_eq!(
            reconciliation_deferral_transition(
                false,
                Pressure::Elevated,
                &cpu_elevated,
                MIN_FREE_BYTES,
            ),
            (false, false)
        );
        let io_elevated = crate::guard::Health {
            mem_avail: 8 << 30,
            mem_total: 16 << 30,
            disk_avail: 20 << 30,
            load1: 1.0,
            ncpu: 12.0,
            psi_io10: 20.0,
            ..Default::default()
        };
        assert_eq!(
            reconciliation_deferral_transition(
                false,
                Pressure::Elevated,
                &io_elevated,
                MIN_FREE_BYTES,
            ),
            (false, false)
        );
        assert_eq!(
            reconciliation_deferral_transition(true, Pressure::Elevated, &health, MIN_FREE_BYTES),
            (true, false)
        );
        assert_eq!(
            reconciliation_deferral_transition(true, Pressure::Critical, &health, MIN_FREE_BYTES),
            (true, false)
        );
        assert_eq!(
            reconciliation_deferral_transition(true, Pressure::Normal, &health, MIN_FREE_BYTES),
            (false, false)
        );
        assert_eq!(
            reconciliation_deferral_transition(false, Pressure::Elevated, &health, MIN_FREE_BYTES),
            (true, true)
        );
    }

    #[test]
    fn reconciliation_cannot_clear_uncertainty_without_verified_accounting() {
        assert!(reconciliation_is_uncertain(false, false, false, 0, false));
        assert!(!reconciliation_is_uncertain(false, false, false, 0, true));
        assert!(reconciliation_is_uncertain(false, false, false, 1, true));
        assert!(reconciliation_is_uncertain(true, false, false, 0, true));
        assert!(reconciliation_is_uncertain(false, true, false, 0, true));
        assert!(reconciliation_is_uncertain(false, false, true, 0, true));
    }

    #[test]
    fn governor_holds_target_duty_cycle() {
        // 50ms CPU over 50ms wall = 100% duty; at a 25% target, work should span
        // 200ms, so sleep 150ms.
        assert!((throttle_sleep_secs(0.05, 0.05, 0.25) - 0.15).abs() < 1e-9);
        // Idle iteration (no CPU) → never sleep, whatever the wall time.
        assert_eq!(throttle_sleep_secs(0.0, 2.0, 0.10), 0.0);
        // Already under target (10ms CPU over 1s wall at 25%) → no sleep.
        assert_eq!(throttle_sleep_secs(0.01, 1.0, 0.25), 0.0);
        // A huge spike's payback is bounded by the cap so it can't stall forever.
        assert_eq!(
            throttle_sleep_secs(100.0, 0.0, 0.25),
            MAX_THROTTLE_SLEEP_SECS
        );
        // Lower target ⇒ more sleep for the same work (10ms cpu, ~0 wall):
        assert!(throttle_sleep_secs(0.01, 0.0, 0.05) > throttle_sleep_secs(0.01, 0.0, 0.25));
    }

    #[test]
    fn fanotify_queue_watermarks_scale_with_configured_capacity() {
        let (high, low) = fanotify_queue_watermarks(100);
        assert_eq!(high, 100 * FAN_EVENT_METADATA_LEN as u64 * 75 / 100);
        assert_eq!(low, 100 * FAN_EVENT_METADATA_LEN as u64 / 2);
        assert!(fanotify_queue_watermarks(0).0 >= FAN_EVENT_METADATA_LEN as u64);
    }

    #[test]
    fn fanotify_queue_pressure_uses_hysteresis_and_fails_safe() {
        let (high, low) = fanotify_queue_watermarks(100);
        assert!(!fanotify_queue_pressure_override(
            false,
            Some(high - 1),
            None,
            high,
            low
        ));
        assert!(fanotify_queue_pressure_override(
            false,
            Some(high),
            None,
            high,
            low
        ));
        assert!(fanotify_queue_pressure_override(
            true,
            Some(low + 1),
            None,
            high,
            low
        ));
        assert!(!fanotify_queue_pressure_override(
            true,
            Some(low),
            None,
            high,
            low
        ));
        assert!(fanotify_queue_pressure_override(
            false,
            None,
            Some(true),
            high,
            low
        ));
        assert!(!fanotify_queue_pressure_override(
            false,
            None,
            Some(false),
            high,
            low
        ));
        // Unknown queue state must prefer draining over sleeping.
        assert!(fanotify_queue_pressure_override(
            false, None, None, high, low
        ));
    }

    #[test]
    fn governor_counts_queue_pressure_episodes_and_duration() {
        let mut governor = CpuGovernor::new(0.25, 100);
        let started = Instant::now();
        governor.last_queue_bytes = Some(governor.queue_high_watermark_bytes);
        governor.set_queue_pressure(true, started);
        governor.set_queue_pressure(true, started + Duration::from_secs(1));
        assert_eq!(governor.queue_pressure_episodes, 1);
        assert_eq!(governor.queue_pressure_total, Duration::ZERO);

        governor.last_queue_bytes = Some(governor.queue_low_watermark_bytes);
        governor.set_queue_pressure(false, started + Duration::from_millis(2_500));
        assert_eq!(governor.queue_pressure_episodes, 1);
        assert_eq!(governor.queue_pressure_total, Duration::from_millis(2_500));
        assert!(governor.queue_pressure_started.is_none());
    }

    #[test]
    fn throttled_watch_status_stays_available_without_claiming_empty_kernel_queue() {
        let db = std::env::temp_dir().join(format!(
            "dux-throttled-status-{}-{}.db",
            std::process::id(),
            now_secs()
        ));
        let status_path = crate::util::runtime_path(&db, "watch.status");
        publish_throttled_watch_status(
            &db,
            ThrottleWatchSnapshot {
                pending: 17,
                events_seen: 101,
                events_resolved: 93,
                updates_committed: 42,
                last_commit_at: 123,
                updates_dropped: 2,
                behind_ms: 5_000,
                kernel_overflow_notifications: 3,
                uncertain: false,
                scan_first: false,
            },
            Duration::from_secs(4),
        );

        let status = crate::util::read_watch_status(&db).unwrap();
        assert_eq!(status.pending, 17);
        assert!(!status.kernel_empty);
        assert_eq!(status.events_seen, 101);
        assert_eq!(status.events_resolved, 93);
        assert_eq!(status.updates_committed, 42);
        assert_eq!(status.last_commit_at, 123);
        assert_eq!(status.updates_dropped, 2);
        assert_eq!(status.behind_ms, 9_000);
        assert_eq!(status.kernel_overflow_notifications, Some(3));
        assert!(!status.uncertain);
        let _ = std::fs::remove_file(status_path);
    }

    #[test]
    fn pending_rescan_does_not_bypass_cpu_governor() {
        let old_rescan = RESCAN.swap(true, Ordering::SeqCst);
        let old_shutdown = SHUTDOWN.swap(false, Ordering::SeqCst);
        let db = std::env::temp_dir().join(format!(
            "dux-governor-status-{}-{}.db",
            std::process::id(),
            now_secs()
        ));
        let status_path = crate::util::runtime_path(&db, "watch.status");
        let mut pipe = [0; 2];
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        let (read_fd, write_fd) = (pipe[0], pipe[1]);
        let mut governor = CpuGovernor::new(0.10, 100);
        let cpu_start = thread_cpu_secs();
        let wall_start = Instant::now();
        // Inject synthetic duty-cycle debt instead of busy-spinning. Under a
        // 10% cgroup quota, burning 500ms of thread CPU already takes about
        // five seconds of wall time, leaving no throttle interval in which to
        // exercise the periodic status publication below.
        governor.cpu_ref = cpu_start - 0.5;
        governor.wall_ref = wall_start;
        governor.throttle(
            read_fd,
            &db,
            Some(ThrottleWatchSnapshot {
                pending: 4,
                events_seen: 10,
                events_resolved: 8,
                updates_committed: 6,
                last_commit_at: now_secs(),
                updates_dropped: 0,
                behind_ms: 1_000,
                kernel_overflow_notifications: 0,
                uncertain: false,
                scan_first: false,
            }),
        );
        let elapsed = wall_start.elapsed().as_secs_f64();
        let cpu_fraction = (thread_cpu_secs() - cpu_start) / elapsed;
        let status = crate::util::read_watch_status(&db);
        RESCAN.store(old_rescan, Ordering::SeqCst);
        SHUTDOWN.store(old_shutdown, Ordering::SeqCst);
        let _ = std::fs::remove_file(status_path);
        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }

        assert!(elapsed >= 2.0, "governor skipped wait: {elapsed:.3}s");
        assert!(
            cpu_fraction <= 0.15,
            "pending RESCAN bypassed the 10% CPU duty-cycle target: {cpu_fraction:.3}"
        );
        let status = status.expect("governor did not refresh throttled watch metrics");
        assert_eq!(status.pending, 4);
        assert!(!status.kernel_empty);
    }

    #[test]
    fn create_delete_keeps_final_state_check_without_index_probe() {
        let mut pending: HashMap<PathBuf, Op> = HashMap::new();
        let mut born: HashSet<PathBuf> = HashSet::new();
        let p = PathBuf::from("/tmp/go-build123/x");

        // CREATE then MODIFY then DELETE has no on-capture DB probe. Retain one
        // delete-state check for the batched writer: the path may have existed
        // in the committed index before this event window.
        ingest_event(&mut pending, &mut born, p.clone(), FAN_CREATE);
        ingest_event(&mut pending, &mut born, p.clone(), FAN_MODIFY);
        ingest_event(&mut pending, &mut born, p.clone(), FAN_DELETE);
        assert!(
            pending.get(&p) == Some(&Op::Delete),
            "create+delete must remove a stale indexed entry if the name is now absent"
        );
        assert!(
            born.is_empty(),
            "the create/delete classification is complete"
        );

        // A delete of a PRE-EXISTING path (never created this window) is a real
        // delete and must be kept.
        let q = PathBuf::from("/data/real.log");
        ingest_event(&mut pending, &mut born, q.clone(), FAN_DELETE);
        assert_eq!(
            pending.get(&q),
            Some(&Op::Delete),
            "real delete is preserved"
        );

        // A create that SURVIVES the window (no delete) stays as an upsert.
        let s = PathBuf::from("/data/survivor");
        ingest_event(&mut pending, &mut born, s.clone(), FAN_CREATE);
        assert_eq!(
            pending.get(&s),
            Some(&Op::Upsert),
            "surviving create is indexed"
        );
        assert!(born.contains(&s), "still born until the window flushes");

        // A later CREATE at a path already committed by an earlier rename is
        // not transient with respect to the index. Its DELETE must remove the
        // previously indexed destination instead of cancelling both events.
        let moved_destination = PathBuf::from("/data/moved-destination");
        ingest_event_with_indexed_path(
            &mut pending,
            &mut born,
            moved_destination.clone(),
            FAN_CREATE,
            true,
        );
        assert!(!born.contains(&moved_destination));
        ingest_event(
            &mut pending,
            &mut born,
            moved_destination.clone(),
            FAN_DELETE,
        );
        assert_eq!(
            pending.get(&moved_destination),
            Some(&Op::Delete),
            "same-window transient must not leave a pre-existing indexed dirent"
        );
    }

    #[test]
    fn combined_create_delete_reconciles_the_final_path_state() {
        use std::os::unix::fs::MetadataExt;

        let dir = tmp("combined-mask");
        let db = tmp("combined-mask-db");
        cleanup(&dir, &db);
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source");
        let alias = dir.join("link-007");
        std::fs::write(&source, b"old inode").unwrap();
        scan_into(&dir, &db);
        let old_id = id_of(&source);

        let mut store = Store::open_rw(&db).unwrap();
        std::fs::hard_link(&source, &alias).unwrap();
        let mut pending = HashMap::new();
        let mut born = HashSet::new();
        ingest_event(&mut pending, &mut born, alias.clone(), FAN_CREATE);
        assert_eq!(pending.get(&alias), Some(&Op::Upsert));
        // A fast create/delete/replace sequence can be delivered as one merged
        // mask while this final path still names the old inode via its hardlink.
        let replacement = dir.join("source.tmp");
        std::fs::write(&replacement, b"replacement inode").unwrap();
        std::fs::rename(&replacement, &source).unwrap();
        ingest_event(
            &mut pending,
            &mut born,
            alias.clone(),
            FAN_CREATE | FAN_DELETE,
        );
        ingest_event(&mut pending, &mut born, source.clone(), FAN_CREATE);
        assert_eq!(pending.get(&alias), Some(&Op::Reconcile));
        assert!(!born.contains(&alias));
        flush(
            &mut store,
            &mut pending,
            &mut HashMap::new(),
            &db,
            7 * 86400,
        )
        .unwrap();
        let (parent_dev, parent_ino) = id_of(&dir);
        let alias_indexed: bool = store
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM dirents WHERE parent_dev=?1 AND parent_inode=?2 AND name=?3 AND dev_id=?4 AND inode=?5)",
                params![parent_dev, parent_ino, b"link-007", old_id.0, old_id.1],
                |row| row.get(0),
            )
            .unwrap();
        assert!(alias_indexed, "surviving hardlink must be indexed");

        // The same ambiguous mask with an absent final path must remove stale
        // state and its byte/inode attribution rather than leaving a ghost row.
        std::fs::remove_file(&alias).unwrap();
        ingest_event(
            &mut pending,
            &mut born,
            alias.clone(),
            FAN_CREATE | FAN_DELETE,
        );
        flush(
            &mut store,
            &mut pending,
            &mut HashMap::new(),
            &db,
            7 * 86400,
        )
        .unwrap();
        let alias_count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM dirents WHERE parent_dev=?1 AND parent_inode=?2 AND name=?3",
                params![parent_dev, parent_ino, b"link-007"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(alias_count, 0, "absent final path must be removed");
        let totals: (i64, i64) = store
            .conn
            .query_row(
                "SELECT recursive_bytes,recursive_inodes FROM inodes WHERE dev_id=?1 AND inode=?2",
                params![parent_dev, parent_ino],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let computed: (i64, i64) = store
            .conn
            .query_row("SELECT SUM(blocks),COUNT(*) FROM inodes", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(totals, computed, "path reconciliation preserves accounting");
        assert!(dir.metadata().unwrap().is_dir());
        assert!(source.metadata().unwrap().is_file());
        assert_ne!(source.metadata().unwrap().ino() as i64, old_id.1);
        cleanup(&dir, &db);
    }

    fn tmp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dux-wt-{tag}-{}", std::process::id()))
    }
    fn cleanup(dir: &Path, db: &Path) {
        let _ = std::fs::remove_dir_all(dir);
        for s in ["", "-wal", "-shm", ".lock"] {
            let _ = std::fs::remove_file(format!("{}{s}", db.display()));
        }
    }
    fn id_of(p: &Path) -> (i64, i64) {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::symlink_metadata(p).unwrap();
        (m.dev() as i64, m.ino() as i64)
    }
    fn count_dirents(store: &Store, dev: i64, ino: i64) -> i64 {
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM dirents WHERE dev_id=?1 AND inode=?2",
                params![dev, ino],
                |r| r.get(0),
            )
            .unwrap()
    }
    fn inode_rows(store: &Store, dev: i64, ino: i64) -> i64 {
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM inodes WHERE dev_id=?1 AND inode=?2",
                params![dev, ino],
                |r| r.get(0),
            )
            .unwrap()
    }
    fn rbytes(store: &Store, dev: i64, ino: i64) -> i64 {
        store
            .conn
            .query_row(
                "SELECT recursive_bytes FROM inodes WHERE dev_id=?1 AND inode=?2",
                params![dev, ino],
                |r| r.get(0),
            )
            .unwrap_or(0)
    }
    fn scan_into(dir: &Path, db: &Path) {
        let mut s = Store::open_rw(db).unwrap();
        scan::scan(
            &mut s,
            dir,
            &ScanOptions {
                progress: false,
                // Test trees live under tmpfs-backed TMPDIR; pseudo mount policy
                // is tested separately, while these fixtures exercise mutations.
                include_pseudo: true,
                ..Default::default()
            },
        )
        .unwrap();
    }

    // A hardlink is added, then both links are removed one at a time. The inode's
    // blocks must be counted once, both paths must be searchable, the inode must
    // survive while ANY link remains, and vanish only on the last unlink.
    #[test]
    fn hardlink_lifecycle() {
        let dir = tmp("hl");
        let db = tmp("hl-db");
        cleanup(&dir, &db);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.dat"), vec![1u8; 4096]).unwrap();
        scan_into(&dir, &db);

        let mut store = Store::open_rw(&db).unwrap();
        let fid = id_of(&dir.join("a.dat"));
        let did = id_of(&dir);
        let base = rbytes(&store, did.0, did.1);

        // add a hardlink -> a second dirent, but the inode is counted once
        std::fs::hard_link(dir.join("a.dat"), dir.join("b.dat")).unwrap();
        let mut p = HashMap::new();
        p.insert(dir.join("b.dat"), Op::Upsert);
        flush(
            &mut store,
            &mut p,
            &mut std::collections::HashMap::new(),
            &db,
            7 * 86400,
        )
        .unwrap();
        assert_eq!(count_dirents(&store, fid.0, fid.1), 2, "both paths indexed");
        assert_eq!(inode_rows(&store, fid.0, fid.1), 1, "one inode row");
        assert_eq!(
            rbytes(&store, did.0, did.1),
            base,
            "hardlink must not change the dir total (counted once)"
        );

        // remove the original (prime) link: inode survives via the other link
        std::fs::remove_file(dir.join("a.dat")).unwrap();
        let mut p = HashMap::new();
        p.insert(dir.join("a.dat"), Op::Delete);
        flush(
            &mut store,
            &mut p,
            &mut std::collections::HashMap::new(),
            &db,
            7 * 86400,
        )
        .unwrap();
        assert_eq!(count_dirents(&store, fid.0, fid.1), 1, "one link remains");
        assert_eq!(inode_rows(&store, fid.0, fid.1), 1, "inode survives");
        assert_eq!(rbytes(&store, did.0, did.1), base, "total unchanged");

        // remove the last link: inode and its blocks are gone
        std::fs::remove_file(dir.join("b.dat")).unwrap();
        let mut p = HashMap::new();
        p.insert(dir.join("b.dat"), Op::Delete);
        flush(
            &mut store,
            &mut p,
            &mut std::collections::HashMap::new(),
            &db,
            7 * 86400,
        )
        .unwrap();
        assert_eq!(
            inode_rows(&store, fid.0, fid.1),
            0,
            "inode removed on last link"
        );
        assert!(
            rbytes(&store, did.0, did.1) < base,
            "blocks subtracted on last unlink"
        );

        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn target_inode_refresh_reuses_parent_paths_across_distinct_inodes() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::MetadataExt;
        let dir = tmp("target-parent-path-cache");
        let db = tmp("target-parent-path-cache-db");
        cleanup(&dir, &db);
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first");
        let second = dir.join("second");
        std::fs::write(&first, b"first inode").unwrap();
        std::fs::write(&second, b"second inode").unwrap();
        let store = Store::create_fresh(&db).unwrap();
        let root_meta = std::fs::symlink_metadata(&dir).unwrap();
        let root_id = (root_meta.dev() as i64, root_meta.ino() as i64);
        store
            .conn
            .execute(
                "INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime,birth_ns)
                 VALUES(?1,?2,'d',?3,?3,3,?4,?5,?6)",
                params![
                    root_id.0,
                    root_id.1,
                    (root_meta.blocks() as i64) * 512,
                    root_meta.uid() as i64,
                    root_meta.mtime(),
                    crate::util::inode_birth_ns_at(&root_meta, &dir),
                ],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
                 VALUES(?1,?2,?3,?1,?2,1)",
                params![root_id.0, root_id.1, dir.as_os_str().as_bytes()],
            )
            .unwrap();
        let first_meta = std::fs::symlink_metadata(&first).unwrap();
        let second_meta = std::fs::symlink_metadata(&second).unwrap();
        for (path, meta) in [(&first, &first_meta), (&second, &second_meta)] {
            let id = (meta.dev() as i64, meta.ino() as i64);
            store
                .conn
                .execute(
                    "INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime,birth_ns)
                     VALUES(?1,?2,'f',?3,?3,1,?4,?5,?6)",
                    params![
                        id.0,
                        id.1,
                        (meta.blocks() as i64) * 512,
                        meta.uid() as i64,
                        meta.mtime(),
                        crate::util::inode_birth_ns_at(meta, path),
                    ],
                )
                .unwrap();
            store
                .conn
                .execute(
                    "INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
                     VALUES(?1,?2,?3,?4,?5,1)",
                    params![
                        root_id.0,
                        root_id.1,
                        path.file_name().unwrap().as_bytes(),
                        id.0,
                        id.1
                    ],
                )
                .unwrap();
        }
        // Cross the 256-row database page boundary with hundreds of aliases
        // to exercise bounded hardlink expansion, not just two-file caching.
        for index in 0..300 {
            let alias = dir.join(format!("first-alias-{index:03}"));
            std::fs::hard_link(&first, &alias).unwrap();
            store
                .conn
                .execute(
                    "INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
                     VALUES(?1,?2,?3,?4,?5,1)",
                    params![
                        root_id.0,
                        root_id.1,
                        alias.file_name().unwrap().as_bytes(),
                        first_meta.dev() as i64,
                        first_meta.ino() as i64
                    ],
                )
                .unwrap();
        }
        let mut pending = HashMap::new();
        let mut parent_paths = IndexedParentPathCache::default();

        assert!(enqueue_inode_refreshes(
            &mut pending,
            &store,
            &dir,
            first_meta.dev() as i64,
            first_meta.ino() as i64,
            &mut parent_paths,
            0,
        )
        .is_err());
        assert!(pending.is_empty(), "zero path budget must admit no links");

        assert!(enqueue_inode_refreshes(
            &mut pending,
            &store,
            &dir,
            first_meta.dev() as i64,
            first_meta.ino() as i64,
            &mut parent_paths,
            275,
        )
        .is_err());
        assert_eq!(
            pending.len(),
            275,
            "refresh must stop at its remaining budget"
        );
        enqueue_inode_refreshes(
            &mut pending,
            &store,
            &dir,
            first_meta.dev() as i64,
            first_meta.ino() as i64,
            &mut parent_paths,
            MAX_PENDING,
        )
        .unwrap();
        assert_eq!(
            pending.len(),
            301,
            "paged refresh must include all hardlinks"
        );
        enqueue_inode_refreshes(
            &mut pending,
            &store,
            &dir,
            second_meta.dev() as i64,
            second_meta.ino() as i64,
            &mut parent_paths,
            302,
        )
        .unwrap();

        assert_eq!(pending.get(&first), Some(&Op::Upsert));
        assert_eq!(pending.get(&second), Some(&Op::Upsert));
        assert_eq!(
            pending.len(),
            302,
            "next inode consumes only remaining capacity"
        );
        assert_eq!(
            parent_paths.lookups, 1,
            "shared parent path should be resolved once"
        );
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn transient_written_hardlink_refreshes_surviving_inode_allocation() {
        use std::os::unix::fs::MetadataExt;
        let (dir, db, mut store) = fixture("transient-hardlink-write");
        let source = dir.join("source.dat");
        std::fs::write(&source, vec![1u8; 4096]).unwrap();
        let mut pending = HashMap::from([(source.clone(), Op::Upsert)]);
        let mut deferred = HashMap::new();
        flush(&mut store, &mut pending, &mut deferred, &db, 86400).unwrap();

        let alias = dir.join("transient-link");
        std::fs::hard_link(&source, &alias).unwrap();
        std::fs::write(&alias, vec![2u8; 16384]).unwrap();
        std::fs::remove_file(&alias).unwrap();

        let metadata = std::fs::symlink_metadata(&source).unwrap();
        let mut parent_paths = IndexedParentPathCache::default();
        enqueue_inode_refreshes(
            &mut pending,
            &store,
            &dir,
            metadata.dev() as i64,
            metadata.ino() as i64,
            &mut parent_paths,
            MAX_PENDING,
        )
        .unwrap();
        assert_eq!(pending.get(&source), Some(&Op::Upsert));
        flush(&mut store, &mut pending, &mut deferred, &db, 86400).unwrap();

        assert_filesystem(&store, &dir);
        drop(store);
        cleanup(&dir, &db);
    }

    // A name reused by a DIFFERENT inode (a missed delete + recreate) must not
    // leave two inodes at one path: the stale occupant is dropped, UNIQUE holds.
    #[test]
    fn duplicate_path_prevented() {
        let dir = tmp("dup");
        let db = tmp("dup-db");
        cleanup(&dir, &db);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("x"), vec![1u8; 4096]).unwrap();
        scan_into(&dir, &db);
        let mut store = Store::open_rw(&db).unwrap();
        let did = id_of(&dir);

        // replace x with a brand-new inode, but only deliver the upsert (the delete
        // event was "missed").
        std::fs::remove_file(dir.join("x")).unwrap();
        std::fs::write(dir.join("x"), vec![2u8; 8192]).unwrap();
        let newid = id_of(&dir.join("x"));
        let mut p = HashMap::new();
        p.insert(dir.join("x"), Op::Upsert);
        flush(
            &mut store,
            &mut p,
            &mut std::collections::HashMap::new(),
            &db,
            7 * 86400,
        )
        .unwrap();

        let n: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM dirents WHERE parent_dev=?1 AND parent_inode=?2 AND name=?3",
                params![did.0, did.1, b"x".to_vec()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "exactly one inode occupies the path");
        let target: (i64, i64) = store
            .conn
            .query_row(
                "SELECT dev_id, inode FROM dirents WHERE parent_dev=?1 AND parent_inode=?2 AND name=?3",
                params![did.0, did.1, b"x".to_vec()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(target, newid, "path points at the NEW inode");
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn reused_directory_inode_replaces_stale_subtree_on_file_upsert() {
        use std::os::unix::fs::MetadataExt;

        let (dir, db, mut store) = fixture("reused-directory-inode");
        let path = dir.join("last_heartbeat");
        std::fs::write(&path, b"current heartbeat file").unwrap();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        let file = (metadata.dev() as i64, metadata.ino() as i64);
        let root_meta = std::fs::symlink_metadata(&dir).unwrap();
        let root = (root_meta.dev() as i64, root_meta.ino() as i64);
        let blocks = metadata.blocks() as i64 * 512;
        let stale_children: Vec<(i64, i64, i64)> = (0..62)
            .map(|index| {
                let blocks = match index {
                    0 => 6_316_032,
                    1 => 17_956_864,
                    _ => 4_096,
                };
                (file.0, i64::MAX - 100 - index, blocks)
            })
            .collect();
        let stale_child_bytes: i64 = stale_children.iter().map(|child| child.2).sum();
        assert_eq!(stale_child_bytes, 24_518_656);

        let mut pending = HashMap::from([(path.clone(), Op::Upsert)]);
        let mut deferred = HashMap::new();
        flush(&mut store, &mut pending, &mut deferred, &db, 86_400).unwrap();

        // Reproduce the live-v4 failure shape deterministically: the filesystem
        // has a regular file, while the stale database incarnation at that same
        // (device,inode) is a directory with 62 leftover children. This mirrors
        // the production incident, including a directory rollup far below the
        // children's actual allocation. Keep root totals balanced so the nested
        // rollup is the only accounting disagreement.
        let changed = store
            .conn
            .execute(
                "UPDATE inodes SET kind='d', recursive_bytes=5996544, recursive_inodes=63
             WHERE dev_id=?1 AND inode=?2",
                params![file.0, file.1],
            )
            .unwrap();
        assert_eq!(changed, 1, "the file identity must exist in the fixture");
        for (index, &(child_dev, child_ino, child_blocks)) in stale_children.iter().enumerate() {
            store
                .conn
                .execute(
                    "INSERT INTO inodes
                 (dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime,birth_ns)
                 VALUES(?1,?2,'f',?3,?3,1,0,0,0)",
                    params![child_dev, child_ino, child_blocks],
                )
                .unwrap();
            store
                .conn
                .execute(
                    "INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
                 VALUES(?1,?2,?3,?4,?5,1)",
                    params![
                        file.0,
                        file.1,
                        format!("stale-child-{index}"),
                        child_dev,
                        child_ino
                    ],
                )
                .unwrap();
        }
        // The old root aggregate was internally balanced against inode rows,
        // despite the one nested directory rollup being 18,526,208 bytes low.
        store
            .conn
            .execute(
                "UPDATE inodes SET recursive_bytes=(SELECT SUM(blocks) FROM inodes),
             recursive_inodes=(SELECT COUNT(*) FROM inodes)
             WHERE dev_id=?1 AND inode=?2",
                params![root.0, root.1],
            )
            .unwrap();
        assert!(
            !store.totals_consistent().unwrap(),
            "the injected production shape must be detected"
        );

        pending.insert(path, Op::Upsert);
        flush(&mut store, &mut pending, &mut deferred, &db, 86_400).unwrap();

        assert_filesystem(&store, &dir);
        let row: (String, i64, i64) = store.conn.query_row(
            "SELECT kind,recursive_bytes,recursive_inodes FROM inodes WHERE dev_id=?1 AND inode=?2",
            params![file.0, file.1],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap();
        assert_eq!(row, ("f".to_owned(), blocks, 1));
        for &(child_dev, child_ino, _) in &stale_children {
            assert_eq!(inode_rows(&store, child_dev, child_ino), 0);
        }
        assert!(store.totals_consistent().unwrap());
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn reused_file_inode_replaces_stale_leaf_on_directory_upsert() {
        use std::os::unix::fs::MetadataExt;

        let (dir, db, mut store) = fixture("reused-file-inode");
        let path = dir.join("global");
        std::fs::create_dir(&path).unwrap();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        let directory = (metadata.dev() as i64, metadata.ino() as i64);
        let root_meta = std::fs::symlink_metadata(&dir).unwrap();
        let root = (root_meta.dev() as i64, root_meta.ino() as i64);
        let blocks = metadata.blocks() as i64 * 512;
        let stale_children: Vec<(i64, i64, i64)> = (0..43)
            .map(|index| {
                let blocks = if index == 0 { 81 * 4096 } else { 4096 };
                (directory.0, i64::MAX - 300 - index, blocks)
            })
            .collect();
        let stale_child_bytes: i64 = stale_children.iter().map(|child| child.2).sum();
        assert_eq!(stale_child_bytes, 503_808);

        let mut pending = HashMap::from([(path.clone(), Op::Upsert)]);
        let mut deferred = HashMap::new();
        flush(&mut store, &mut pending, &mut deferred, &db, 86_400).unwrap();

        // Reproduce the opposite orientation of the live v4 mismatch: the
        // filesystem has a directory, while the stale database incarnation at
        // the same identity is a leaf retaining 43 old descendants.
        let changed = store
            .conn
            .execute(
                "UPDATE inodes SET kind='f', recursive_bytes=?3, recursive_inodes=44
                 WHERE dev_id=?1 AND inode=?2",
                params![directory.0, directory.1, blocks + stale_child_bytes],
            )
            .unwrap();
        assert_eq!(
            changed, 1,
            "the directory identity must exist in the fixture"
        );
        for (index, &(child_dev, child_ino, child_blocks)) in stale_children.iter().enumerate() {
            store
                .conn
                .execute(
                    "INSERT INTO inodes
                     (dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime,birth_ns)
                     VALUES(?1,?2,'f',?3,?3,1,0,0,0)",
                    params![child_dev, child_ino, child_blocks],
                )
                .unwrap();
            store
                .conn
                .execute(
                    "INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
                     VALUES(?1,?2,?3,?4,?5,1)",
                    params![
                        directory.0,
                        directory.1,
                        format!("stale-child-{index}"),
                        child_dev,
                        child_ino
                    ],
                )
                .unwrap();
        }
        store
            .conn
            .execute(
                "UPDATE inodes SET recursive_bytes=(SELECT SUM(blocks) FROM inodes),
                 recursive_inodes=(SELECT COUNT(*) FROM inodes)
                 WHERE dev_id=?1 AND inode=?2",
                params![root.0, root.1],
            )
            .unwrap();
        assert!(
            !store.totals_consistent().unwrap(),
            "the injected file-as-parent state must be rejected"
        );

        pending.insert(path, Op::Upsert);
        flush(&mut store, &mut pending, &mut deferred, &db, 86_400).unwrap();

        assert_filesystem(&store, &dir);
        let row: (String, i64, i64) = store
            .conn
            .query_row(
                "SELECT kind,recursive_bytes,recursive_inodes FROM inodes WHERE dev_id=?1 AND inode=?2",
                params![directory.0, directory.1],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(row, ("d".to_owned(), blocks, 1));
        for &(child_dev, child_ino, _) in &stale_children {
            assert_eq!(inode_rows(&store, child_dev, child_ino), 0);
        }
        assert!(store.totals_consistent().unwrap());
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn nested_rollup_verification_detects_drift_when_root_matches_inode_aggregate() {
        use std::os::unix::fs::MetadataExt;

        let (dir, db, store) = fixture("nested-rollup-verification");
        let root_id = id_of(&dir);
        let root_blocks = std::fs::symlink_metadata(&dir).unwrap().blocks() as i64 * 512;
        let nested_id = (root_id.0, i64::MAX - 10);
        let file_id = (root_id.0, i64::MAX - 11);
        store
            .conn
            .execute(
                "INSERT INTO inodes
             (dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,uid,mtime,birth_ns)
             VALUES(?1,?2,'d',4096,8192,2,0,0,0),
                   (?1,?3,'f',4096,4096,1,0,0,0)",
                params![root_id.0, nested_id.1, file_id.1],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
             VALUES(?1,?2,'nested',?1,?3,1), (?1,?3,'file',?1,?4,1)",
                params![root_id.0, root_id.1, nested_id.1, file_id.1],
            )
            .unwrap();
        store
            .conn
            .execute(
                "UPDATE inodes SET recursive_bytes=?3,recursive_inodes=3
             WHERE dev_id=?1 AND inode=?2",
                params![root_id.0, root_id.1, root_blocks + 8192],
            )
            .unwrap();
        assert!(store.totals_consistent().unwrap());

        let changed = store
            .conn
            .execute(
                "UPDATE inodes SET recursive_bytes=recursive_bytes+4096,
             recursive_inodes=recursive_inodes+1 WHERE dev_id=?1 AND inode=?2",
                params![nested_id.0, nested_id.1],
            )
            .unwrap();
        assert_eq!(changed, 1, "the nested directory must be indexed");
        let root_matches_inode_aggregate: bool = store
            .conn
            .query_row(
                "SELECT r.recursive_bytes=SUM(i.blocks) AND r.recursive_inodes=COUNT(*)
             FROM inodes r CROSS JOIN inodes i
             WHERE r.dev_id=?1 AND r.inode=?2",
                params![root_id.0, root_id.1],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            root_matches_inode_aggregate,
            "the injected defect must leave global totals balanced"
        );
        assert!(
            !store.totals_consistent().unwrap(),
            "the full verifier must detect nested drift despite balanced root totals"
        );

        store
            .conn
            .execute(
                "UPDATE inodes SET recursive_bytes=recursive_bytes-4096,
             recursive_inodes=recursive_inodes-1 WHERE dev_id=?1 AND inode=?2",
                params![nested_id.0, nested_id.1],
            )
            .unwrap();
        assert!(store.totals_consistent().unwrap());
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn affected_rollup_reconciliation_repairs_stale_leaf_allocation() {
        use std::os::unix::fs::MetadataExt;

        let (dir, db, mut store) = fixture("stale-leaf-rollup");
        let path = dir.join("growing-file");
        std::fs::write(&path, vec![7; 4096]).unwrap();
        let root = id_of(&dir);
        let file = id_of(&path);
        let file_blocks = std::fs::symlink_metadata(&path).unwrap().blocks() as i64 * 512;
        let root_blocks = std::fs::symlink_metadata(&dir).unwrap().blocks() as i64 * 512;
        store
            .conn
            .execute(
                "INSERT INTO inodes(dev_id,inode,kind,blocks,recursive_bytes,recursive_inodes,
                                uid,mtime,birth_ns)
             VALUES(?1,?2,'f',?3,?3,1,0,0,0)",
                params![file.0, file.1, file_blocks],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO dirents(parent_dev,parent_inode,name,dev_id,inode,prime)
             VALUES(?1,?2,'growing-file',?3,?4,1)",
                params![root.0, root.1, file.0, file.1],
            )
            .unwrap();
        store
            .conn
            .execute(
                "UPDATE inodes SET blocks=?3,recursive_bytes=?4,recursive_inodes=2
             WHERE dev_id=?1 AND inode=?2",
                params![root.0, root.1, root_blocks, root_blocks + file_blocks],
            )
            .unwrap();
        // Inject the live failure shape: a flush can leave a leaf with zero
        // recursive totals even though its allocation and root rollup are
        // present. The next affected-directory reconciliation must restore
        // both the leaf and every ancestor.
        store
            .conn
            .execute(
                "UPDATE inodes SET recursive_bytes=0,recursive_inodes=0
             WHERE dev_id=?1 AND inode=?2",
                params![file.0, file.1],
            )
            .unwrap();
        assert!(!store.totals_consistent().unwrap());

        let tx = store.conn.transaction().unwrap();
        let mut affected = AncestorChanges::default();
        affected.require_exact();
        affected.insert(root, (0, 0));
        reconcile_affected_rollups(&tx, &affected).unwrap();
        tx.commit().unwrap();

        assert_totals(&store);
        let leaf: (i64, i64) = store
            .conn
            .query_row(
                "SELECT blocks,recursive_bytes FROM inodes WHERE dev_id=?1 AND inode=?2",
                params![file.0, file.1],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(leaf.0, leaf.1);
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn affected_leaf_repair_query_is_driven_by_indexed_directory_children() {
        let (dir, db, store) = fixture("leaf-repair-query-plan");
        let root = id_of(&dir);
        let sql = format!("EXPLAIN QUERY PLAN {REPAIR_AFFECTED_LEAVES_SQL}");
        let mut statement = store.conn.prepare(&sql).unwrap();
        let details: Vec<String> = statement
            .query_map(params![root.0, root.1], |row| row.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();

        assert!(
            details
                .iter()
                .any(|detail| detail.contains("idx_dirents_parent")),
            "repair must enumerate children through the parent index: {details:?}"
        );
        assert!(
            details.iter().any(|detail| detail.contains("PRIMARY KEY")),
            "repair must address only child inodes through their primary key: {details:?}"
        );
        assert!(
            details.iter().all(|detail| !detail.contains("SCAN inodes")),
            "repair must not rescan every inode for each affected directory: {details:?}"
        );
        drop(statement);
        drop(store);
        cleanup(&dir, &db);
    }

    // Two filenames with different invalid-UTF-8 bytes must remain distinct rows
    // (the old lossy-String storage collapsed both to one replacement string).
    #[test]
    fn non_utf8_names_distinct() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tmp("nu");
        let db = tmp("nu-db");
        cleanup(&dir, &db);
        std::fs::create_dir_all(&dir).unwrap();
        let n1 = std::ffi::OsStr::from_bytes(b"bad\xff\x01");
        let n2 = std::ffi::OsStr::from_bytes(b"bad\xfe\x02");
        std::fs::write(dir.join(n1), b"x").unwrap();
        std::fs::write(dir.join(n2), b"y").unwrap();
        scan_into(&dir, &db);
        let store = Store::open_rw(&db).unwrap();
        let did = id_of(&dir);
        let n: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM dirents WHERE parent_dev=?1 AND parent_inode=?2
                 AND NOT (dev_id=?1 AND inode=?2)",
                params![did.0, did.1],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2, "distinct non-UTF-8 names must not collapse");
        let names: Vec<Vec<u8>> = {
            let mut st = store
                .conn
                .prepare(
                    "SELECT name FROM dirents WHERE parent_dev=?1 AND parent_inode=?2
                     AND NOT (dev_id=?1 AND inode=?2)",
                )
                .unwrap();
            st.query_map(params![did.0, did.1], |r| r.get::<_, Vec<u8>>(0))
                .unwrap()
                .filter_map(|x| x.ok())
                .collect()
        };
        assert!(names.iter().any(|b| b.as_slice() == b"bad\xff\x01"));
        assert!(names.iter().any(|b| b.as_slice() == b"bad\xfe\x02"));
        drop(store);
        cleanup(&dir, &db);
    }

    // A rescan (atomic rebuild) reconciles everything, so it must CLEAR a stale
    // dirty flag — otherwise the index would read "DIRTY" forever after one
    // transient overflow (the dirty-state lifecycle the review flagged).
    #[test]
    fn rescan_clears_dirty() {
        let dir = tmp("dirty");
        let db = tmp("dirty-db");
        cleanup(&dir, &db);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f"), b"x").unwrap();
        scan_into(&dir, &db);
        {
            let s = Store::open_rw(&db).unwrap();
            s.set_meta("dirty_since", "123").unwrap();
        }
        assert!(
            Store::open_ro(&db)
                .unwrap()
                .get_meta("dirty_since")
                .unwrap()
                .is_some(),
            "precondition: dirty flag is set"
        );
        scan::rebuild_atomic(
            &db,
            &dir,
            &ScanOptions {
                progress: false,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            Store::open_ro(&db)
                .unwrap()
                .get_meta("dirty_since")
                .unwrap()
                .is_none(),
            "a rescan must clear dirty_since"
        );
        cleanup(&dir, &db);
    }

    fn dirent_exists(store: &Store, pdev: i64, pino: i64, name: &[u8]) -> bool {
        store
            .conn
            .query_row(
                "SELECT 1 FROM dirents WHERE parent_dev=?1 AND parent_inode=?2 AND name=?3",
                params![pdev, pino, name],
                |_| Ok(()),
            )
            .is_ok()
    }

    // C1: a directory rename whose MOVED_FROM and MOVED_TO land in DIFFERENT
    // flushes must still be treated as a rename — the subtree must NOT vanish
    // between flushes, and no spurious growth rows are written.
    #[test]
    fn rename_split_across_flushes() {
        let dir = tmp("rmv");
        let db = tmp("rmv-db");
        cleanup(&dir, &db);
        std::fs::create_dir_all(dir.join("old")).unwrap();
        std::fs::write(dir.join("old/f.bin"), vec![1u8; 4096]).unwrap();
        scan_into(&dir, &db);
        let mut store = Store::open_rw(&db).unwrap();
        let d = id_of(&dir); // parent of old/new
        let old_ino = id_of(&dir.join("old"));
        let inodes_before = inode_rows(&store, old_ino.0, old_ino.1);

        // rename on disk, then deliver ONLY the MOVED_FROM in flush 1
        std::fs::rename(dir.join("old"), dir.join("new")).unwrap();
        let mut deferred: HashMap<(i64, i64), DeferredFrom> = HashMap::new();
        let mut p1 = HashMap::new();
        p1.insert(dir.join("old"), Op::MovedFrom);
        flush(&mut store, &mut p1, &mut deferred, &db, 7 * 86400).unwrap();
        // subtree must still be present (deferred, not deleted)
        assert!(
            dirent_exists(&store, d.0, d.1, b"old"),
            "FROM-only flush must NOT drop the renamed dir (deferred)"
        );
        assert_eq!(
            inode_rows(&store, old_ino.0, old_ino.1),
            inodes_before,
            "the moved inode must survive the FROM-only flush"
        );

        // deliver the MOVED_TO in flush 2 (same deferred map) → it's a rename
        let mut p2 = HashMap::new();
        p2.insert(dir.join("new"), Op::MovedTo);
        flush(&mut store, &mut p2, &mut deferred, &db, 7 * 86400).unwrap();
        assert!(
            dirent_exists(&store, d.0, d.1, b"new"),
            "after the TO flush the dir is at its new name"
        );
        assert!(
            !dirent_exists(&store, d.0, d.1, b"old"),
            "old name is gone after the rename completes"
        );
        // the child followed the inode (subtree intact)
        assert!(
            dirent_exists(&store, old_ino.0, old_ino.1, b"f.bin"),
            "the subtree (f.bin) must follow the renamed directory"
        );
        // the paired deferred FROM must be forgotten only AFTER the commit — and it
        // must indeed be forgotten, else a later flush's Phase F would expire it and
        // wrongly unlink the just-renamed dir. (Regression guard for the rename
        // rollback-safety fix: peek-not-remove + post-commit cleanup.)
        assert!(
            deferred.is_empty(),
            "deferred map must be empty after the rename pairs"
        );
        let mut p3 = HashMap::new();
        flush(&mut store, &mut p3, &mut deferred, &db, 7 * 86400).unwrap();
        assert!(
            dirent_exists(&store, d.0, d.1, b"new"),
            "a subsequent (empty) flush must not disturb the renamed dir"
        );
        assert!(
            !dirent_exists(&store, d.0, d.1, b"old"),
            "old name stays gone across later flushes"
        );

        // a rename is zero-delta: no growth rows written
        let growth_rows: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM growth", [], |r| r.get(0))
            .unwrap();
        assert_eq!(growth_rows, 0, "a rename must not write growth rows");

        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn unrelated_same_inode_moved_to_does_not_consume_source_rename() {
        let dir = tmp("same-inode-move-to");
        let db = tmp("same-inode-move-to-db");
        cleanup(&dir, &db);
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source");
        let alias = dir.join("existing-alias");
        let destination = dir.join("destination");
        std::fs::write(&source, vec![7u8; 4096]).unwrap();
        std::fs::hard_link(&source, &alias).unwrap();
        scan_into(&dir, &db);
        let mut store = Store::open_rw(&db).unwrap();
        std::fs::rename(&source, &destination).unwrap();

        let mut deferred = HashMap::new();
        let mut first = HashMap::from([
            (source.clone(), Op::MovedFrom),
            // Model an unrelated/no-op MovedTo for an already indexed hardlink
            // that resolves to this same inode during the source flush.
            (alias.clone(), Op::MovedTo),
        ]);
        flush(&mut store, &mut first, &mut deferred, &db, 7 * 86400).unwrap();
        assert_eq!(
            deferred.len(),
            1,
            "source must remain pending for its real TO"
        );
        let root = id_of(&dir);
        assert!(dirent_exists(&store, root.0, root.1, b"source"));

        let mut second = HashMap::from([(destination.clone(), Op::MovedTo)]);
        flush(&mut store, &mut second, &mut deferred, &db, 7 * 86400).unwrap();
        assert!(
            deferred.is_empty(),
            "the real destination consumes the source"
        );
        assert_filesystem(&store, &dir);
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn superseded_missing_move_from_does_not_leave_a_ghost_hardlink() {
        let dir = tmp("superseded-move-from");
        let db = tmp("superseded-move-from-db");
        cleanup(&dir, &db);
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first");
        let removed = dir.join("removed");
        let destination = dir.join("destination");
        std::fs::write(&first, vec![9u8; 4096]).unwrap();
        std::fs::hard_link(&first, &removed).unwrap();
        scan_into(&dir, &db);
        let mut store = Store::open_rw(&db).unwrap();

        // Two source names for one inode disappear before its destination event
        // is flushed. Only `first` was renamed; the second hardlink was removed.
        std::fs::rename(&first, &destination).unwrap();
        std::fs::remove_file(&removed).unwrap();
        let mut pending = HashMap::from([
            (first, Op::MovedFrom),
            (removed, Op::MovedFrom),
            (destination, Op::MovedTo),
        ]);
        let mut deferred = HashMap::new();
        flush(&mut store, &mut pending, &mut deferred, &db, 7 * 86400).unwrap();
        assert!(deferred.is_empty());
        assert_filesystem(&store, &dir);
        drop(store);
        cleanup(&dir, &db);
    }

    // C2: an OVERWRITE-rename — rename onto an EXISTING name (`mv -f a b`, or the
    // ubiquitous write-temp-then-rename-over-original atomic-save pattern used by
    // editors, dpkg, rsync, build tools) — must atomically REPLACE the destination,
    // not fail the flush. The bare UPDATE used to collide with the destination
    // dirent's UNIQUE(parent,name), erroring the WHOLE flush transaction so NOTHING
    // committed: the daemon reported "live" while the index silently froze and went
    // stale. Regression guard for that root cause.
    #[test]
    fn overwrite_rename_replaces_destination() {
        let dir = tmp("ovr");
        let db = tmp("ovr-db");
        cleanup(&dir, &db);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a"), vec![1u8; 4096]).unwrap();
        std::fs::write(dir.join("b"), vec![2u8; 8192]).unwrap();
        scan_into(&dir, &db);
        let mut store = Store::open_rw(&db).unwrap();
        let d = id_of(&dir);
        let a_inode = id_of(&dir.join("a"));
        let b_inode = id_of(&dir.join("b")); // the link that gets OVERWRITTEN

        // rename a onto existing b (overwrite). On disk, `b` now IS a's inode and
        // the old b inode has one fewer link (here: zero → gone).
        std::fs::rename(dir.join("a"), dir.join("b")).unwrap();
        let mut deferred: HashMap<(i64, i64), DeferredFrom> = HashMap::new();
        let mut p = HashMap::new();
        p.insert(dir.join("a"), Op::MovedFrom);
        p.insert(dir.join("b"), Op::MovedTo);

        // BEFORE the fix this returns Err(UNIQUE constraint failed: dirents…) and the
        // entire batch rolls back — the whole point of the bug.
        flush(&mut store, &mut p, &mut deferred, &db, 7 * 86400)
            .expect("overwrite-rename flush must succeed, not poison the batch");

        assert!(dirent_exists(&store, d.0, d.1, b"b"), "b must remain");
        assert!(!dirent_exists(&store, d.0, d.1, b"a"), "a must be gone");
        let b_target: (i64, i64) = store
            .conn
            .query_row(
                "SELECT dev_id, inode FROM dirents
                 WHERE parent_dev=?1 AND parent_inode=?2 AND name=?3",
                params![d.0, d.1, b"b"],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            b_target, a_inode,
            "b must now resolve to the renamed (a) inode"
        );
        assert_eq!(
            inode_rows(&store, b_inode.0, b_inode.1),
            0,
            "the overwritten destination inode must be removed (its last link is gone)"
        );
        assert!(
            inode_rows(&store, a_inode.0, a_inode.1) > 0,
            "the renamed inode survives at its new name"
        );

        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn cross_directory_overwrite_moves_allocation_between_exact_rollups() {
        let dir = tmp("cross-dir-overwrite-rollup");
        let db = tmp("cross-dir-overwrite-rollup-db");
        cleanup(&dir, &db);
        let source_dir = dir.join("source");
        let destination_dir = dir.join("destination");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&destination_dir).unwrap();
        std::fs::write(source_dir.join("payload"), vec![1u8; 8192]).unwrap();
        std::fs::write(destination_dir.join("payload"), vec![2u8; 4096]).unwrap();
        scan_into(&dir, &db);
        let mut store = Store::open_rw(&db).unwrap();
        let source_id = id_of(&source_dir);
        let destination_id = id_of(&destination_dir);

        // Cross-parent overwrite moves one prime inode's allocation and removes
        // the old destination inode. This is the minimal event shape for an
        // inverse (+bytes,+inode) rollup drift between sibling directories.
        std::fs::rename(source_dir.join("payload"), destination_dir.join("payload")).unwrap();
        let mut pending = HashMap::from([
            (source_dir.join("payload"), Op::MovedFrom),
            (destination_dir.join("payload"), Op::MovedTo),
        ]);
        let mut deferred = HashMap::new();
        flush(&mut store, &mut pending, &mut deferred, &db, 7 * 86400).unwrap();

        assert_eq!(
            crate::store::indexed_path(&store.conn, &destination_dir.join("payload")).unwrap(),
            Some(id_of(&destination_dir.join("payload")))
        );
        assert!(!source_dir.join("payload").exists());
        let source_rollup = rbytes(&store, source_id.0, source_id.1);
        let destination_rollup = rbytes(&store, destination_id.0, destination_id.1);
        assert!(source_rollup < destination_rollup);
        assert!(
            store.totals_consistent().unwrap(),
            "stale scan fixture must be consistent before replay"
        );
        assert_totals(&store);
        assert_filesystem(&store, &dir);

        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn cross_directory_rename_repairs_reciprocal_stale_rollups() {
        let dir = tmp("rename-rollup-repair");
        let db = tmp("rename-rollup-repair-db");
        cleanup(&dir, &db);
        let left = dir.join("left");
        let right = dir.join("right");
        std::fs::create_dir_all(&left).unwrap();
        std::fs::create_dir_all(&right).unwrap();
        std::fs::write(left.join("source"), vec![1u8; 4096]).unwrap();
        std::fs::write(right.join("destination"), vec![2u8; 4096]).unwrap();
        scan_into(&dir, &db);
        let mut store = Store::open_rw(&db).unwrap();
        let left_id = id_of(&left);
        let right_id = id_of(&right);

        // Reproduce the production symptom: equal and opposite one-inode
        // attribution drift between sibling parents, with root totals unchanged.
        store
            .conn
            .execute(
                "UPDATE inodes SET recursive_bytes=recursive_bytes-4096,
             recursive_inodes=recursive_inodes-1 WHERE dev_id=?1 AND inode=?2",
                params![left_id.0, left_id.1],
            )
            .unwrap();
        store
            .conn
            .execute(
                "UPDATE inodes SET recursive_bytes=recursive_bytes+4096,
             recursive_inodes=recursive_inodes+1 WHERE dev_id=?1 AND inode=?2",
                params![right_id.0, right_id.1],
            )
            .unwrap();
        assert!(!store.totals_consistent().unwrap());

        std::fs::rename(left.join("source"), right.join("destination")).unwrap();
        let mut pending = HashMap::from([
            (left.join("source"), Op::MovedFrom),
            (right.join("destination"), Op::MovedTo),
        ]);
        let mut deferred = HashMap::new();
        flush(&mut store, &mut pending, &mut deferred, &db, 7 * 86400).unwrap();

        assert_filesystem(&store, &dir);
        assert!(store.totals_consistent().unwrap());
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn rename_replay_after_scan_prime_promotion_retargets_accounting() {
        let dir = tmp("scan-prime-promotion-replay");
        let db = tmp("scan-prime-promotion-replay-db");
        cleanup(&dir, &db);
        let source_dir = dir.join("z-source");
        let destination_dir = dir.join("a-destination");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&destination_dir).unwrap();
        let source = source_dir.join("payload");
        let destination = destination_dir.join("payload");
        std::fs::write(&source, vec![7u8; 4096]).unwrap();
        std::fs::hard_link(&source, &destination).unwrap();
        scan_into(&dir, &db);
        let mut store = Store::open_rw(&db).unwrap();
        let source_parent = id_of(&source_dir);
        let destination_parent = id_of(&destination_dir);
        let inode = id_of(&source);
        let blocks: i64 = store
            .conn
            .query_row(
                "SELECT blocks FROM inodes WHERE dev_id=?1 AND inode=?2",
                params![inode.0, inode.1],
                |row| row.get(0),
            )
            .unwrap();
        let old_prime_parent: (i64, i64) = store
            .conn
            .query_row(
                "SELECT parent_dev,parent_inode FROM dirents
                 WHERE dev_id=?1 AND inode=?2 AND prime=1",
                params![inode.0, inode.1],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        // A concurrent scan can observe both names for one inode and assign the
        // destination as prime, even though the source event is later replayed.
        // Install that valid two-link scan state deterministically.
        store
            .conn
            .execute(
                "UPDATE dirents SET prime=CASE
                   WHEN parent_dev=?1 AND parent_inode=?2 AND name=?3 THEN 0
                   WHEN parent_dev=?4 AND parent_inode=?5 AND name=?3 THEN 1
                   ELSE prime END
                 WHERE dev_id=?6 AND inode=?7",
                params![
                    source_parent.0,
                    source_parent.1,
                    b"payload",
                    destination_parent.0,
                    destination_parent.1,
                    inode.0,
                    inode.1,
                ],
            )
            .unwrap();
        if old_prime_parent != destination_parent {
            for (parent, sign) in [(source_parent, -1i64), (destination_parent, 1i64)] {
                store
                    .conn
                    .execute(
                        "UPDATE inodes SET recursive_bytes=recursive_bytes+?3,
                           recursive_inodes=recursive_inodes+?4
                         WHERE dev_id=?1 AND inode=?2",
                        params![parent.0, parent.1, sign * blocks, sign],
                    )
                    .unwrap();
            }
        }
        assert_totals(&store);

        // Model the scan racing the final move: the stale generation contains
        // both aliases with the destination prime, while the filesystem now has
        // only the destination. Replay the rename pair through the real reducer.
        std::fs::remove_file(&source).unwrap();
        let mut pending = HashMap::from([
            (source.clone(), Op::MovedFrom),
            (destination.clone(), Op::MovedTo),
        ]);
        let mut deferred = HashMap::new();
        flush(&mut store, &mut pending, &mut deferred, &db, 7 * 86400).unwrap();

        assert_eq!(
            crate::store::indexed_path(&store.conn, &destination).unwrap(),
            Some(inode)
        );
        assert!(!source.exists());
        assert_totals(&store);
        assert_filesystem(&store, &dir);
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn rename_relocates_source_even_if_destination_row_already_matches() {
        let dir = tmp("stale-destination-rename");
        let db = tmp("stale-destination-rename-db");
        cleanup(&dir, &db);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("source"), vec![1u8; 4096]).unwrap();
        std::fs::write(dir.join("destination"), vec![2u8; 8192]).unwrap();
        scan_into(&dir, &db);
        let mut store = Store::open_rw(&db).unwrap();
        let parent = id_of(&dir);
        let source = id_of(&dir.join("source"));

        // Model a concurrent rebuild that observed the source inode at the
        // destination while the atomic rename was in flight. The subsequent
        // MOVED_FROM/MOVED_TO pair must still remove the now-vanished source.
        store
            .conn
            .execute(
                "UPDATE dirents SET dev_id=?1,inode=?2,prime=0
                 WHERE parent_dev=?3 AND parent_inode=?4 AND name=?5",
                params![source.0, source.1, parent.0, parent.1, b"destination"],
            )
            .unwrap();
        std::fs::rename(dir.join("source"), dir.join("destination")).unwrap();
        let mut pending = HashMap::from([
            (dir.join("source"), Op::MovedFrom),
            (dir.join("destination"), Op::MovedTo),
        ]);
        flush(
            &mut store,
            &mut pending,
            &mut HashMap::new(),
            &db,
            7 * 86400,
        )
        .unwrap();

        assert!(!dirent_exists(&store, parent.0, parent.1, b"source"));
        let target: (i64, i64) = store
            .conn
            .query_row(
                "SELECT dev_id,inode FROM dirents
                 WHERE parent_dev=?1 AND parent_inode=?2 AND name=?3",
                params![parent.0, parent.1, b"destination"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(target, source);

        drop(store);
        cleanup(&dir, &db);
    }

    // B8: a file hardlinked OUTSIDE a deleted subtree must survive the delete, and
    // the daemon-maintained totals must equal a fresh scan of the post-delete tree.
    #[test]
    fn hardlink_outside_deleted_subtree_survives() {
        let dir = tmp("b8");
        let db = tmp("b8-db");
        let db2 = tmp("b8-db2");
        cleanup(&dir, &db);
        cleanup(&dir, &db2);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/inside"), vec![7u8; 8192]).unwrap();
        std::fs::hard_link(dir.join("sub/inside"), dir.join("outside")).unwrap();
        scan_into(&dir, &db);
        let mut store = Store::open_rw(&db).unwrap();
        let fid = id_of(&dir.join("outside")); // shared inode
        let did = id_of(&dir);

        // delete the subtree on disk, then deliver the Delete event to the daemon
        std::fs::remove_dir_all(dir.join("sub")).unwrap();
        let mut p = HashMap::new();
        p.insert(dir.join("sub"), Op::Delete);
        flush(&mut store, &mut p, &mut HashMap::new(), &db, 7 * 86400).unwrap();

        assert_eq!(
            inode_rows(&store, fid.0, fid.1),
            1,
            "the inode hardlinked outside the subtree must survive the delete"
        );
        assert!(
            dirent_exists(&store, did.0, did.1, b"outside"),
            "the external link must remain findable"
        );
        assert!(!dirent_exists(&store, did.0, did.1, b"sub"), "sub is gone");

        // gold standard: daemon-maintained total == fresh scan of the same tree
        let live_total = rbytes(&store, did.0, did.1);
        scan_into(&dir, &db2);
        let store2 = Store::open_ro(&db2).unwrap();
        let fresh_total = rbytes(&store2, did.0, did.1);
        assert_eq!(
            live_total, fresh_total,
            "daemon delete with a surviving hardlink must match a fresh scan"
        );

        drop(store);
        drop(store2);
        cleanup(&dir, &db);
        cleanup(&dir, &db2);
    }

    fn fixture(tag: &str) -> (PathBuf, PathBuf, Store) {
        let dir = tmp(tag);
        let db = tmp(&format!("{tag}-db"));
        cleanup(&dir, &db);
        std::fs::create_dir_all(&dir).unwrap();
        scan_into(&dir, &db);
        let store = Store::open_rw(&db).unwrap();
        (dir, db, store)
    }

    fn assert_totals(store: &Store) {
        assert!(
            store.totals_consistent().unwrap(),
            "root must equal SUM(blocks),COUNT(inodes)"
        );
        let bad: i64 = store.conn.query_row(
            "SELECT COUNT(*) FROM inodes i WHERE kind='d' AND (
             recursive_bytes != blocks+COALESCE((SELECT SUM(ch.recursive_bytes) FROM dirents d
             JOIN inodes ch ON ch.dev_id=d.dev_id AND ch.inode=d.inode WHERE d.parent_dev=i.dev_id
             AND d.parent_inode=i.inode AND d.prime=1 AND NOT(d.dev_id=i.dev_id AND d.inode=i.inode)),0)
             OR recursive_inodes != 1+COALESCE((SELECT SUM(ch.recursive_inodes) FROM dirents d
             JOIN inodes ch ON ch.dev_id=d.dev_id AND ch.inode=d.inode WHERE d.parent_dev=i.dev_id
             AND d.parent_inode=i.inode AND d.prime=1 AND NOT(d.dev_id=i.dev_id AND d.inode=i.inode)),0))", [], |r| r.get(0)).unwrap();
        assert_eq!(
            bad, 0,
            "every directory must equal its own allocation plus prime children"
        );
        let bad_leaves: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM inodes WHERE kind!='d'
             AND (recursive_bytes!=blocks OR recursive_inodes!=1)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            bad_leaves, 0,
            "every non-directory inode must equal its allocation"
        );
    }

    fn rollup_mismatch_details(store: &Store) -> Vec<String> {
        let mut stmt = store
            .conn
            .prepare(
                "SELECT i.dev_id,i.inode,i.blocks,i.recursive_bytes,i.recursive_inodes,
                    COALESCE(SUM(ch.recursive_bytes),0),
                    COALESCE(SUM(ch.recursive_inodes),0)
             FROM inodes i LEFT JOIN dirents d
               ON d.parent_dev=i.dev_id AND d.parent_inode=i.inode AND d.prime=1
                  AND NOT(d.dev_id=i.dev_id AND d.inode=i.inode)
             LEFT JOIN inodes ch ON ch.dev_id=d.dev_id AND ch.inode=d.inode
             WHERE i.kind='d'
             GROUP BY i.dev_id,i.inode
             HAVING i.recursive_bytes != i.blocks+COALESCE(SUM(ch.recursive_bytes),0)
                 OR i.recursive_inodes != 1+COALESCE(SUM(ch.recursive_inodes),0)",
            )
            .unwrap();
        stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?, row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?, row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?, row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        }).unwrap().map(|row| {
            let (dev, ino, blocks, recursive_bytes, recursive_inodes, child_bytes, child_inodes) = row.unwrap();
            let path = crate::store::indexed_inode_path(&store.conn, dev, ino)
                .ok().flatten().map(|p| p.display().to_string()).unwrap_or_else(|| "<unresolved>".into());
            format!("{path} ({dev},{ino}): bytes {recursive_bytes} != {blocks}+{child_bytes}; inodes {recursive_inodes} != 1+{child_inodes}")
        }).collect()
    }

    fn invariant_mismatch_details(store: &Store) -> String {
        let root = store
            .conn
            .query_row(
                "SELECT CAST((SELECT value FROM meta WHERE key='root_dev') AS INTEGER),
                    CAST((SELECT value FROM meta WHERE key='root_inode') AS INTEGER)",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap();
        let root_totals: Option<(i64, i64)> = store
            .conn
            .query_row(
                "SELECT recursive_bytes,recursive_inodes FROM inodes WHERE dev_id=?1 AND inode=?2",
                rusqlite::params![root.0, root.1],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .unwrap();
        let inode_totals: (i64, i64) = store
            .conn
            .query_row(
                "SELECT COALESCE(SUM(blocks),0),COUNT(*) FROM inodes",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let non_directory_mismatches: Vec<String> = store.conn.prepare(
            "SELECT dev_id,inode,kind,blocks,recursive_bytes FROM inodes
             WHERE kind!='d' AND recursive_bytes!=blocks ORDER BY dev_id,inode",
        ).unwrap().query_map([], |row| Ok((
            row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?, row.get::<_, i64>(4)?,
        ))).unwrap().map(|row| {
            let (dev, ino, kind, blocks, recursive_bytes) = row.unwrap();
            let path = crate::store::indexed_inode_path(&store.conn, dev, ino)
                .ok().flatten().map(|p| p.display().to_string()).unwrap_or_else(|| "<unresolved>".into());
            format!("{path} ({dev},{ino},kind={kind}): blocks={blocks}, recursive_bytes={recursive_bytes}")
        }).collect();
        format!("root={root:?} root_totals={root_totals:?} inode_totals={inode_totals:?} non_directory_mismatches={non_directory_mismatches:?}")
    }

    fn assert_filesystem(store: &Store, root: &Path) {
        use std::os::unix::fs::MetadataExt;
        let mut stack = vec![root.to_path_buf()];
        let mut unique = std::collections::HashSet::new();
        let mut entries = 0i64;
        let mut bytes = 0i64;
        while let Some(path) = stack.pop() {
            let m = std::fs::symlink_metadata(&path).unwrap();
            let id = (m.dev() as i64, m.ino() as i64);
            assert_eq!(
                crate::store::indexed_path(&store.conn, &path).unwrap(),
                Some(id),
                "{}",
                path.display()
            );
            let kind: String = store
                .conn
                .query_row(
                    "SELECT kind FROM inodes WHERE dev_id=?1 AND inode=?2",
                    params![id.0, id.1],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(kind, kind_of(&m), "{}", path.display());
            entries += 1;
            if unique.insert(id) {
                bytes += m.blocks() as i64 * 512;
            }
            if m.is_dir() {
                stack.extend(
                    std::fs::read_dir(&path)
                        .unwrap()
                        .map(|entry| entry.unwrap().path()),
                );
            }
        }
        let indexed: (i64, i64, i64) = store
            .conn
            .query_row(
                "SELECT SUM(blocks),COUNT(*),(SELECT COUNT(*) FROM dirents) FROM inodes",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            indexed,
            (bytes, unique.len() as i64, entries),
            "all indexed entries and allocations must exactly match the filesystem"
        );
        assert_totals(store);
    }

    #[test]
    fn failed_batch_preserves_events_and_born_set() {
        let (dir, db, mut store) = fixture("batch-retry");
        let mut pending = HashMap::new();
        let mut born = std::collections::HashSet::new();
        for i in 0..8 {
            let path = dir.join(format!("f{i}"));
            std::fs::write(&path, vec![i as u8; 4096]).unwrap();
            born.insert(path.clone());
            pending.insert(path, Op::Upsert);
        }
        let original = pending.clone();
        let original_born = born.clone();
        store.conn.execute_batch("CREATE TRIGGER fail_insert BEFORE INSERT ON inodes WHEN new.kind!='d' BEGIN SELECT RAISE(ABORT,'injected failure'); END;").unwrap();
        let mut deferred = HashMap::new();
        assert!(flush_batch(
            &mut store,
            &mut pending,
            &mut born,
            &mut deferred,
            &db,
            86400,
            4,
            true
        )
        .is_err());
        assert_eq!(pending, original);
        assert_eq!(born, original_born);
        assert!(deferred.is_empty());
        store
            .conn
            .execute_batch("DROP TRIGGER fail_insert")
            .unwrap();
        while !pending.is_empty() {
            flush_batch(
                &mut store,
                &mut pending,
                &mut born,
                &mut deferred,
                &db,
                86400,
                4,
                true,
            )
            .unwrap();
        }
        assert!(born.is_empty());
        assert_totals(&store);
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn child_before_parent_creates_complete_ancestor_chain() {
        let (dir, db, mut store) = fixture("parent-order");
        std::fs::create_dir_all(dir.join("a/b/c")).unwrap();
        let child = dir.join("a/b/c/file");
        std::fs::write(&child, vec![1; 8192]).unwrap();
        let mut pending = HashMap::from([(child.clone(), Op::Upsert)]);
        flush(&mut store, &mut pending, &mut HashMap::new(), &db, 86400).unwrap();
        for path in ["a", "a/b", "a/b/c", "a/b/c/file"] {
            assert!(crate::store::indexed_path(&store.conn, &dir.join(path))
                .unwrap()
                .is_some());
        }
        assert_totals(&store);
        pending.insert(dir.join("a"), Op::Upsert);
        pending.insert(dir.join("a/b"), Op::Upsert);
        flush(&mut store, &mut pending, &mut HashMap::new(), &db, 86400).unwrap();
        assert_totals(&store);
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn idle_move_out_expires_and_rollback_preserves_deferred_state() {
        let (dir, db, mut store) = fixture("idle-expiry");
        let file = dir.join("out");
        std::fs::write(&file, vec![1; 4096]).unwrap();
        let mut pending = HashMap::from([(file.clone(), Op::Upsert)]);
        let mut deferred = HashMap::new();
        flush(&mut store, &mut pending, &mut deferred, &db, 86400).unwrap();
        let id = id_of(&file);
        std::fs::remove_file(&file).unwrap();
        pending.insert(file, Op::MovedFrom);
        flush(&mut store, &mut pending, &mut deferred, &db, 86400).unwrap();
        assert_eq!(deferred.len(), 1);
        deferred.get_mut(&id).unwrap().expires = Instant::now() - Duration::from_secs(1);
        let expiry = deferred[&id].expires;
        store.conn.execute_batch("CREATE TRIGGER fail_delete BEFORE DELETE ON dirents BEGIN SELECT RAISE(ABORT,'injected delete failure'); END;").unwrap();
        assert!(flush(&mut store, &mut pending, &mut deferred, &db, 86400).is_err());
        assert_eq!(deferred[&id].expires, expiry);
        store
            .conn
            .execute_batch("DROP TRIGGER fail_delete")
            .unwrap();
        flush_batch(
            &mut store,
            &mut pending,
            &mut std::collections::HashSet::new(),
            &mut deferred,
            &db,
            86400,
            4,
            true,
        )
        .unwrap();
        assert!(deferred.is_empty());
        assert_eq!(inode_rows(&store, id.0, id.1), 0);
        assert_totals(&store);
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn stale_delete_reconciles_reused_path() {
        let (dir, db, mut store) = fixture("stale-delete");
        let path = dir.join("file");
        std::fs::write(&path, vec![1; 4096]).unwrap();
        let mut pending = HashMap::from([(path.clone(), Op::Upsert)]);
        flush(&mut store, &mut pending, &mut HashMap::new(), &db, 86400).unwrap();
        std::fs::write(dir.join("replacement"), vec![2; 12288]).unwrap();
        std::fs::rename(dir.join("replacement"), &path).unwrap();
        pending.insert(path.clone(), Op::Delete);
        flush(&mut store, &mut pending, &mut HashMap::new(), &db, 86400).unwrap();
        assert_eq!(
            crate::store::indexed_path(&store.conn, &path).unwrap(),
            Some(id_of(&path))
        );
        assert_totals(&store);
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn create_then_delete_of_previously_indexed_missing_path_removes_ghost() {
        let (dir, db, mut store) = fixture("create-delete-old-indexed");
        let path = dir.join("transient-replacement");
        std::fs::write(&path, vec![1; 4096]).unwrap();
        let mut pending = HashMap::from([(path.clone(), Op::Upsert)]);
        flush(&mut store, &mut pending, &mut HashMap::new(), &db, 86400).unwrap();
        assert!(crate::store::indexed_path(&store.conn, &path)
            .unwrap()
            .is_some());

        // Simulate a delayed CREATE followed by DELETE for a name that had
        // already existed in the committed generation. The capture path no
        // longer probes SQLite to classify CREATE, so flush must safely remove
        // the absent old dirent instead of cancelling the pair as "new-born".
        std::fs::remove_file(&path).unwrap();
        let mut born = HashSet::new();
        ingest_event(&mut pending, &mut born, path.clone(), FAN_CREATE);
        ingest_event(&mut pending, &mut born, path.clone(), FAN_DELETE);
        assert_eq!(pending.get(&path), Some(&Op::Delete));
        flush(&mut store, &mut pending, &mut HashMap::new(), &db, 86400).unwrap();
        assert!(crate::store::indexed_path(&store.conn, &path)
            .unwrap()
            .is_none());
        assert_totals(&store);
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn deleting_directory_with_understated_rollup_subtracts_subtree_truth() {
        use std::os::unix::fs::MetadataExt;

        let (dir, db, store) = fixture("delete-understated-directory");
        let subtree = dir.join("tmp-artifact");
        std::fs::create_dir_all(subtree.join("nested/deeper")).unwrap();
        std::fs::write(subtree.join("nested/deeper/payload"), vec![7; 32 * 1024]).unwrap();

        drop(store);
        for suffix in ["", "-wal", "-shm", ".lock"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
        scan_into(&dir, &db);
        let mut store = Store::open_rw(&db).unwrap();
        let mut pending = HashMap::new();
        let mut deferred = HashMap::new();
        let id = id_of(&subtree);
        let actual: (i64, i64) = store
            .conn
            .query_row(
                "WITH RECURSIVE subtree(dev_id,inode) AS (
                    SELECT ?1,?2
                    UNION
                    SELECT d.dev_id,d.inode FROM dirents d
                    JOIN subtree p ON d.parent_dev=p.dev_id AND d.parent_inode=p.inode
                    WHERE d.prime=1 AND NOT(d.dev_id=d.parent_dev AND d.inode=d.parent_inode)
                 ) SELECT SUM(i.blocks),COUNT(*) FROM subtree s
                   JOIN inodes i ON i.dev_id=s.dev_id AND i.inode=s.inode",
                params![id.0, id.1],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let own_blocks = std::fs::symlink_metadata(&subtree).unwrap().blocks() as i64 * 512;
        assert!(
            actual.0 > own_blocks,
            "fixture must have allocated descendants: actual={actual:?}, own={own_blocks}"
        );
        assert!(actual.1 > 1, "fixture must contain descendants: {actual:?}");

        // Model a directory whose cached subtree total lost descendant deltas.
        // A later rmdir must subtract the actual prime-attributed inode set,
        // otherwise the containing /tmp-like directory retains phantom usage.
        store
            .conn
            .execute(
                "UPDATE inodes SET recursive_bytes=?3,recursive_inodes=1
                 WHERE dev_id=?1 AND inode=?2",
                params![id.0, id.1, own_blocks],
            )
            .unwrap();
        assert!(!store.totals_consistent().unwrap());

        std::fs::remove_dir_all(&subtree).unwrap();
        pending.insert(subtree.clone(), Op::Delete);
        flush(&mut store, &mut pending, &mut deferred, &db, 86_400).unwrap();

        assert!(crate::store::indexed_path(&store.conn, &subtree)
            .unwrap()
            .is_none());
        assert_eq!(inode_rows(&store, id.0, id.1), 0);
        assert!(store.totals_consistent().unwrap());
        assert_filesystem(&store, &dir);
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn deleting_live_sized_stale_build_tree_restores_exact_rollups() {
        use std::io::Write;
        use std::os::unix::fs::MetadataExt;

        // The live recurrence retained a removed `/tmp/go-build…` directory
        // with 799 indexed inodes and a 28.9 MB recursive allocation. Recreate
        // that subtree size and then make its cached rollup stale before the
        // delete event is applied.
        let dir = tmp("live-sized-stale-build-tree");
        let db = tmp("live-sized-stale-build-tree-db");
        cleanup(&dir, &db);
        std::fs::create_dir_all(&dir).unwrap();
        let subtree = dir.join("go-build-live-shape");
        std::fs::create_dir(&subtree).unwrap();
        for index in 0..798 {
            let mut file =
                std::fs::File::create(subtree.join(format!("artifact-{index:03}"))).unwrap();
            let len = if index < 674 { 32_769 } else { 32_768 };
            file.write_all(&vec![b'x'; len]).unwrap();
        }
        scan_into(&dir, &db);
        let mut store = Store::open_rw(&db).unwrap();
        let (dev, ino) = id_of(&subtree);
        let actual: (i64, i64) = store
            .conn
            .query_row(
                "WITH RECURSIVE subtree(dev_id,inode) AS (
                    SELECT ?1,?2
                    UNION
                    SELECT d.dev_id,d.inode FROM dirents d
                    JOIN subtree p ON d.parent_dev=p.dev_id AND d.parent_inode=p.inode
                    WHERE d.prime=1 AND NOT(d.dev_id=d.parent_dev AND d.inode=d.parent_inode)
                 ) SELECT SUM(i.blocks),COUNT(*) FROM subtree s
                   JOIN inodes i ON i.dev_id=s.dev_id AND i.inode=s.inode",
                params![dev, ino],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let own_blocks = std::fs::symlink_metadata(&subtree).unwrap().blocks() as i64 * 512;
        assert_eq!(actual.1, 799, "root plus 798 unique file inodes");
        assert!(
            actual.0 > 20 * 1024 * 1024,
            "large live-like tree: {actual:?}"
        );

        store
            .conn
            .execute(
                "UPDATE inodes SET recursive_bytes=?3,recursive_inodes=1
                 WHERE dev_id=?1 AND inode=?2",
                params![dev, ino, own_blocks],
            )
            .unwrap();
        assert!(!store.totals_consistent().unwrap());

        std::fs::remove_dir_all(&subtree).unwrap();
        let mut pending = HashMap::from([(subtree.clone(), Op::Delete)]);
        flush(&mut store, &mut pending, &mut HashMap::new(), &db, 86_400).unwrap();

        assert_eq!(inode_rows(&store, dev, ino), 0);
        assert!(store.totals_consistent().unwrap());
        assert_filesystem(&store, &dir);
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn delayed_metadata_cannot_cancel_move_from_or_delete() {
        let (dir, db, mut store) = fixture("delayed-metadata-removal");
        let source = dir.join("source");
        let destination = dir.join("destination");
        std::fs::write(&source, b"rename me").unwrap();
        let mut pending = HashMap::from([(source.clone(), Op::Upsert)]);
        let mut born = HashSet::new();
        let mut deferred = HashMap::new();
        flush(&mut store, &mut pending, &mut deferred, &db, 86400).unwrap();

        std::fs::rename(&source, &destination).unwrap();
        ingest_event(&mut pending, &mut born, source.clone(), FAN_MOVED_FROM);
        ingest_event(&mut pending, &mut born, source.clone(), FAN_ATTRIB);
        assert_eq!(pending.get(&source), Some(&Op::MovedFrom));
        ingest_event(&mut pending, &mut born, destination.clone(), FAN_MOVED_TO);
        flush(&mut store, &mut pending, &mut deferred, &db, 86400).unwrap();
        assert!(crate::store::indexed_path(&store.conn, &source)
            .unwrap()
            .is_none());
        assert_eq!(
            crate::store::indexed_path(&store.conn, &destination).unwrap(),
            Some(id_of(&destination))
        );

        std::fs::remove_file(&destination).unwrap();
        ingest_event(&mut pending, &mut born, destination.clone(), FAN_DELETE);
        ingest_event(&mut pending, &mut born, destination.clone(), FAN_ATTRIB);
        assert_eq!(pending.get(&destination), Some(&Op::Delete));
        flush(&mut store, &mut pending, &mut deferred, &db, 86400).unwrap();
        assert!(crate::store::indexed_path(&store.conn, &destination)
            .unwrap()
            .is_none());
        assert_filesystem(&store, &dir);
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn directory_move_without_from_relocates_instead_of_aliasing() {
        let (dir, db, mut store) = fixture("late-move");
        std::fs::create_dir(dir.join("old")).unwrap();
        std::fs::write(dir.join("old/file"), vec![1; 4096]).unwrap();
        let mut pending = HashMap::from([(dir.join("old"), Op::MovedTo)]);
        flush(&mut store, &mut pending, &mut HashMap::new(), &db, 86400).unwrap();
        let id = id_of(&dir.join("old"));
        std::fs::rename(dir.join("old"), dir.join("new")).unwrap();
        pending.insert(dir.join("new"), Op::MovedTo);
        flush(&mut store, &mut pending, &mut HashMap::new(), &db, 86400).unwrap();
        assert_eq!(count_dirents(&store, id.0, id.1), 1);
        assert!(crate::store::indexed_path(&store.conn, &dir.join("old"))
            .unwrap()
            .is_none());
        assert!(
            crate::store::indexed_path(&store.conn, &dir.join("new/file"))
                .unwrap()
                .is_some()
        );
        assert_totals(&store);
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn directory_allocation_changes_are_counted() {
        let (dir, db, mut store) = fixture("directory-blocks");
        let mut pending = HashMap::new();
        for i in 0..180 {
            let path = dir.join(format!(
                "a-long-filename-to-grow-directory-allocation-{i:04}"
            ));
            std::fs::write(&path, b"x").unwrap();
            pending.insert(path, Op::Upsert);
        }
        flush(&mut store, &mut pending, &mut HashMap::new(), &db, 86400).unwrap();
        use std::os::unix::fs::MetadataExt;
        let root = id_of(&dir);
        let stored: i64 = store
            .conn
            .query_row(
                "SELECT blocks FROM inodes WHERE dev_id=?1 AND inode=?2",
                params![root.0, root.1],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            stored,
            std::fs::metadata(&dir).unwrap().blocks() as i64 * 512
        );
        assert_totals(&store);
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn recreate_then_delete_does_not_cancel_original_deletion() {
        let (dir, db, mut store) = fixture("recreate-delete");
        let path = dir.join("file");
        std::fs::write(&path, b"old").unwrap();
        let mut pending = HashMap::from([(path.clone(), Op::Upsert)]);
        let mut deferred = HashMap::new();
        flush(&mut store, &mut pending, &mut deferred, &db, 86400).unwrap();
        let mut born = std::collections::HashSet::new();
        std::fs::remove_file(&path).unwrap();
        ingest_event(&mut pending, &mut born, path.clone(), FAN_DELETE);
        std::fs::write(&path, b"replacement").unwrap();
        ingest_event(&mut pending, &mut born, path.clone(), FAN_CREATE);
        std::fs::remove_file(&path).unwrap();
        ingest_event(&mut pending, &mut born, path.clone(), FAN_DELETE);
        assert_eq!(pending.get(&path), Some(&Op::Delete));
        flush(&mut store, &mut pending, &mut deferred, &db, 86400).unwrap();
        assert!(crate::store::indexed_path(&store.conn, &path)
            .unwrap()
            .is_none());
        assert_totals(&store);
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn mixed_churn_with_partial_batches_matches_filesystem() {
        let (dir, db, mut store) = fixture("mixed-churn");
        let mut pending = HashMap::new();
        let mut born = std::collections::HashSet::new();
        let mut deferred = HashMap::new();
        for i in 0..240 {
            let path = dir.join(format!("file-{}", i % 31));
            match i % 4 {
                0 | 1 => {
                    let new = !path.exists();
                    std::fs::write(&path, vec![i as u8; 4096 * ((i % 3) + 1)]).unwrap();
                    ingest_event(
                        &mut pending,
                        &mut born,
                        path,
                        if new { FAN_CREATE } else { FAN_MODIFY },
                    );
                }
                2 if path.exists() => {
                    let dest = dir.join(format!("file-{}", (i + 7) % 31));
                    std::fs::rename(&path, &dest).unwrap();
                    ingest_event(&mut pending, &mut born, path, FAN_MOVED_FROM);
                    ingest_event(&mut pending, &mut born, dest, FAN_MOVED_TO);
                }
                _ if path.exists() => {
                    std::fs::remove_file(&path).unwrap();
                    ingest_event(&mut pending, &mut born, path, FAN_DELETE);
                }
                _ => {}
            }
            if i % 7 == 0 {
                flush_batch(
                    &mut store,
                    &mut pending,
                    &mut born,
                    &mut deferred,
                    &db,
                    86400,
                    3,
                    true,
                )
                .unwrap();
            }
        }
        while !pending.is_empty() {
            flush_batch(
                &mut store,
                &mut pending,
                &mut born,
                &mut deferred,
                &db,
                86400,
                3,
                true,
            )
            .unwrap();
        }
        for from in deferred.values_mut() {
            from.expires = Instant::now();
        }
        flush(&mut store, &mut pending, &mut deferred, &db, 86400).unwrap();
        assert_totals(&store);
        use std::os::unix::fs::MetadataExt;
        let mut actual = std::fs::metadata(&dir).unwrap().blocks() as i64 * 512;
        let mut count = 1;
        for e in std::fs::read_dir(&dir).unwrap() {
            let e = e.unwrap();
            let m = e.metadata().unwrap();
            actual += m.blocks() as i64 * 512;
            count += 1;
            assert_eq!(
                crate::store::indexed_path(&store.conn, &e.path()).unwrap(),
                Some((m.dev() as i64, m.ino() as i64))
            );
        }
        let id = id_of(&dir);
        let indexed: (i64, i64) = store
            .conn
            .query_row(
                "SELECT recursive_bytes,recursive_inodes FROM inodes WHERE dev_id=?1 AND inode=?2",
                params![id.0, id.1],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            indexed,
            (actual, count),
            "all delayed/coalesced mutations must converge to current filesystem state"
        );
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn delete_under_renamed_parent_in_same_batch() {
        let (dir, db, mut store) = fixture("rename-parent-delete");
        std::fs::create_dir_all(dir.join("old/nested")).unwrap();
        std::fs::write(dir.join("old/nested/file"), vec![1; 4096]).unwrap();
        let mut pending = HashMap::from([(dir.join("old"), Op::MovedTo)]);
        let mut deferred = HashMap::new();
        flush(&mut store, &mut pending, &mut deferred, &db, 86400).unwrap();
        std::fs::rename(dir.join("old"), dir.join("new")).unwrap();
        std::fs::remove_file(dir.join("new/nested/file")).unwrap();
        pending.extend([
            (dir.join("old"), Op::MovedFrom),
            (dir.join("new"), Op::MovedTo),
            (dir.join("new/nested/file"), Op::Delete),
        ]);
        flush(&mut store, &mut pending, &mut deferred, &db, 86400).unwrap();
        assert!(
            crate::store::indexed_path(&store.conn, &dir.join("new/nested/file"))
                .unwrap()
                .is_none()
        );
        assert_totals(&store);
        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM inodes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3);
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn delete_with_stat_identity_skips_indexed_parent_fallback_query() {
        use rusqlite::hooks::{AuthAction, Authorization};
        use std::sync::atomic::AtomicUsize;

        let (dir, db, mut store) = fixture("delete-lazy-parent-lookup");
        let file = dir.join("payload");
        std::fs::write(&file, b"payload").unwrap();
        let mut pending = HashMap::from([(file.clone(), Op::Upsert)]);
        let mut deferred = HashMap::new();
        flush(&mut store, &mut pending, &mut deferred, &db, 86400).unwrap();
        std::fs::remove_file(&file).unwrap();

        // `indexed_path` begins by reading `meta.value` for `last_scan_root`.
        // Count those authorizer callbacks during a delete whose parent still
        // stats successfully; the normal delete path must not perform fallback.
        let metadata_value_reads = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&metadata_value_reads);
        store
            .conn
            .authorizer(Some(move |ctx: rusqlite::hooks::AuthContext<'_>| {
                if matches!(
                    ctx.action,
                    AuthAction::Read {
                        table_name: "meta",
                        column_name: "value"
                    }
                ) {
                    observed.fetch_add(1, Ordering::Relaxed);
                }
                Authorization::Allow
            }))
            .unwrap();

        // Positive control: make sure the authorizer sees the query that this
        // test is intended to rule out, then clear the count before the delete.
        assert!(crate::store::indexed_path(&store.conn, &dir)
            .unwrap()
            .is_some());
        assert!(metadata_value_reads.load(Ordering::Relaxed) > 0);
        metadata_value_reads.store(0, Ordering::Relaxed);

        pending.insert(file, Op::Delete);
        flush(&mut store, &mut pending, &mut deferred, &db, 86400).unwrap();
        assert_eq!(metadata_value_reads.load(Ordering::Relaxed), 0);
        store
            .conn
            .authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> Authorization>)
            .unwrap();

        assert_totals(&store);
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn backlog_budget_includes_deferred_moves() {
        assert!(!backlog_exceeded(MAX_PENDING, 0, 0));
        assert!(!backlog_exceeded(100, MAX_PENDING - 100, 0));
        assert!(backlog_exceeded(100, MAX_PENDING, 0));
        assert!(backlog_exceeded(0, 0, MAX_PENDING + 1));
        assert!(backlog_exceeded(usize::MAX, 1, 0));
    }

    #[test]
    fn target_refreshes_are_applied_in_bounded_batches() {
        let mut queued: HashSet<_> = (0..MAX_TARGET_REFRESHES_PER_CYCLE as i64 + 3)
            .map(|inode| (1, inode))
            .collect();
        let first = take_target_refresh_batch(&mut queued, MAX_TARGET_REFRESHES_PER_CYCLE);
        assert_eq!(first.len(), MAX_TARGET_REFRESHES_PER_CYCLE);
        assert_eq!(queued.len(), 3);
        for id in &first {
            assert!(!queued.contains(id));
        }
        let last = take_target_refresh_batch(&mut queued, MAX_TARGET_REFRESHES_PER_CYCLE);
        assert_eq!(last.len(), 3);
        assert!(queued.is_empty());
    }

    #[test]
    fn shutdown_requires_eagain_and_no_buffered_records_to_claim_drain() {
        assert!(!shutdown_drain_incomplete(true, 0));
        assert!(shutdown_drain_incomplete(false, 0));
        assert!(shutdown_drain_incomplete(true, 1));
        assert!(shutdown_drain_incomplete(false, 1));
    }

    #[test]
    fn shutdown_event_flush_respects_wal_guard_and_active_scan() {
        assert!(shutdown_event_flush_allowed(true, false, false, true));
        assert!(!shutdown_event_flush_allowed(true, false, true, true));
        assert!(!shutdown_event_flush_allowed(true, true, false, true));
        assert!(!shutdown_event_flush_allowed(false, false, false, true));
        assert!(!shutdown_event_flush_allowed(true, false, false, false));
    }

    #[test]
    fn nested_build_artifact_churn_keeps_every_rollup_consistent() {
        for seed in 1..=4u64 {
            let (dir, db, mut store) = fixture(&format!("build-churn-{seed}"));
            let mut pending = HashMap::new();
            let mut born = std::collections::HashSet::new();
            let mut deferred = HashMap::new();
            let mut state = seed;
            let mut history = std::collections::VecDeque::new();
            for step in 0..600 {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let source = dir.join(format!("slot-{}", state % 8));
                let target = dir.join(format!("slot-{}", (state >> 8) % 8));
                let file = source.join(format!("artifact-{}", (state >> 16) % 12));
                let other = target.join(format!("artifact-{}", (state >> 24) % 12));
                let operation = (state >> 32) % 8;
                history.push_back(format!(
                    "{step}: {operation} {} -> {}",
                    file.display(),
                    other.display()
                ));
                if history.len() > 16 {
                    history.pop_front();
                }
                match operation {
                    0 | 1 => {
                        std::fs::create_dir_all(&source).unwrap();
                        let temp = source.join("write.tmp");
                        std::fs::write(&temp, vec![step as u8; 4096 * (1 + step % 3)]).unwrap();
                        ingest_event(&mut pending, &mut born, temp.clone(), FAN_CREATE);
                        std::fs::rename(&temp, &file).unwrap();
                        ingest_event(&mut pending, &mut born, temp, FAN_MOVED_FROM);
                        ingest_event(&mut pending, &mut born, file, FAN_MOVED_TO);
                    }
                    2 if file.exists() => {
                        std::fs::create_dir_all(&target).unwrap();
                        if other.exists() {
                            std::fs::remove_file(&other).unwrap();
                            ingest_event(&mut pending, &mut born, other.clone(), FAN_DELETE);
                        }
                        if file.exists() {
                            std::fs::hard_link(&file, &other).unwrap();
                            ingest_event(&mut pending, &mut born, other, FAN_CREATE);
                        }
                    }
                    3 if source.exists() && source != target => {
                        if target.exists() {
                            std::fs::remove_dir_all(&target).unwrap();
                            ingest_event(&mut pending, &mut born, target.clone(), FAN_DELETE);
                        }
                        std::fs::rename(&source, &target).unwrap();
                        ingest_event(&mut pending, &mut born, source, FAN_MOVED_FROM);
                        ingest_event(&mut pending, &mut born, target, FAN_MOVED_TO);
                    }
                    4 if file.exists() => {
                        std::fs::remove_file(&file).unwrap();
                        ingest_event(&mut pending, &mut born, file, FAN_DELETE);
                    }
                    5 if source.exists() => {
                        std::fs::remove_dir_all(&source).unwrap();
                        ingest_event(&mut pending, &mut born, source, FAN_DELETE);
                    }
                    6 => {
                        let child = source.join("nested/deeper/child");
                        std::fs::create_dir_all(child.parent().unwrap()).unwrap();
                        std::fs::write(&child, b"nested").unwrap();
                        ingest_event(&mut pending, &mut born, child, FAN_CREATE);
                    }
                    _ if file.exists() && file != other => {
                        std::fs::create_dir_all(&target).unwrap();
                        std::fs::rename(&file, &other).unwrap();
                        ingest_event(&mut pending, &mut born, file, FAN_MOVED_FROM);
                        ingest_event(&mut pending, &mut born, other, FAN_MOVED_TO);
                    }
                    _ => {}
                }
                if step % 7 == 0 {
                    flush_batch(
                        &mut store,
                        &mut pending,
                        &mut born,
                        &mut deferred,
                        &db,
                        86400,
                        7,
                        true,
                    )
                    .unwrap();
                    assert!(
                        store.totals_consistent().unwrap(),
                        "seed={seed}, history={history:?}, bad_rollups={:?}, invariants={}",
                        rollup_mismatch_details(&store),
                        invariant_mismatch_details(&store)
                    );
                    let consistent = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        assert_totals(&store)
                    }));
                    assert!(
                        consistent.is_ok(),
                        "seed={seed}, step={step}, history={history:?}"
                    );
                }
            }
            while !pending.is_empty() {
                flush_batch(
                    &mut store,
                    &mut pending,
                    &mut born,
                    &mut deferred,
                    &db,
                    86400,
                    7,
                    true,
                )
                .unwrap();
                assert_totals(&store);
            }
            for from in deferred.values_mut() {
                from.expires = Instant::now();
            }
            flush(&mut store, &mut pending, &mut deferred, &db, 86400).unwrap();
            assert_filesystem(&store, &dir);
            drop(store);
            cleanup(&dir, &db);
        }
    }

    #[test]
    fn deferred_rename_cannot_move_or_delete_replacement_at_source_name() {
        let (dir, db, mut store) = fixture("stale-rename-source");
        let source = dir.join("source");
        let destination = dir.join("destination");
        std::fs::write(&source, vec![1; 4096]).unwrap();
        let mut pending = HashMap::from([(source.clone(), Op::Upsert)]);
        let mut deferred = HashMap::new();
        flush(&mut store, &mut pending, &mut deferred, &db, 86400).unwrap();
        std::fs::rename(&source, &destination).unwrap();
        pending.insert(source.clone(), Op::MovedFrom);
        flush(&mut store, &mut pending, &mut deferred, &db, 86400).unwrap();
        assert!(!deferred.is_empty());
        std::fs::write(&source, vec![2; 8192]).unwrap();
        pending.insert(source.clone(), Op::Upsert);
        flush(&mut store, &mut pending, &mut deferred, &db, 86400).unwrap();
        pending.insert(destination.clone(), Op::MovedTo);
        flush(&mut store, &mut pending, &mut deferred, &db, 86400).unwrap();
        for from in deferred.values_mut() {
            from.expires = Instant::now();
        }
        flush(&mut store, &mut pending, &mut deferred, &db, 86400).unwrap();
        assert_filesystem(&store, &dir);
        drop(store);
        cleanup(&dir, &db);
    }

    #[test]
    fn directory_rename_repairs_children_waiting_in_later_batches() {
        let (dir, db, mut store) = fixture("rename-partial-children");
        let old = dir.join("old");
        let new = dir.join("new");
        std::fs::create_dir(&old).unwrap();
        std::fs::write(old.join("deleted"), vec![1; 4096]).unwrap();
        let mut pending = HashMap::from([(old.clone(), Op::MovedTo)]);
        let mut deferred = HashMap::new();
        flush(&mut store, &mut pending, &mut deferred, &db, 86400).unwrap();
        std::fs::remove_file(old.join("deleted")).unwrap();
        std::fs::write(old.join("created"), vec![2; 8192]).unwrap();
        std::fs::rename(&old, &new).unwrap();
        // The stale child batch can be consumed before the parent TO arrives.
        let mut stale_children = HashMap::from([
            (old.join("created"), Op::Upsert),
            (old.join("deleted"), Op::Delete),
        ]);
        flush(&mut store, &mut stale_children, &mut deferred, &db, 86400).unwrap();
        assert!(stale_children.is_empty());
        let mut rename = HashMap::from([(old, Op::MovedFrom), (new, Op::MovedTo)]);
        flush(&mut store, &mut rename, &mut deferred, &db, 86400).unwrap();
        assert_filesystem(&store, &dir);
        drop(store);
        cleanup(&dir, &db);
    }
}

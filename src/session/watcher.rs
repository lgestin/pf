use crate::error::Result;
use crate::paths;
use crate::session::ssh::{RealSsh, SshControl};
use crate::session::{
    apply, reconcile, store, AttachStatus, DesiredSession, ForwardObs, SessionState, SessionStatus,
};
use crate::watcher::{backoff_delay, jitter_nanos, next_retry_count, HEALTHY_UPTIME_SECS};
use chrono::Utc;
use std::fs::OpenOptions;
use std::os::unix::process::CommandExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often the watcher re-reads desired state. SIGUSR1 wakes it sooner; this
/// is the fallback so a missed signal costs half a second, not correctness.
pub fn poll_interval() -> Duration {
    Duration::from_millis(500)
}

/// Ref-counted lifecycle: no forwards means no reason for the master to exist.
pub fn should_exit(desired: &DesiredSession) -> bool {
    desired.forwards.is_empty()
}

/// Why the watcher's loop ended — and therefore what it may delete on the way
/// out.
///
/// The desired forward set is the only thing in the run directory a person
/// actually typed. Erasing it is right when leaving *was* their decision, and
/// wrong otherwise: a reboot SIGTERMs every process on the machine, and reading
/// that as "cancel my forwards" is how an overnight shutdown used to leave an
/// empty run directory with no record of what had been running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    /// The desired set emptied out, or was deleted outright. The session is
    /// over because someone said so.
    Retired,
    /// SIGTERM or SIGINT from outside — in practice, the OS shutting down.
    Signalled,
    /// The connection is gone and the watcher stopped trying: auto-reconnect
    /// was off, retries ran out, or it could not function at all.
    GaveUp,
}

impl ExitReason {
    /// Whether the desired forward set dies with the watcher.
    fn discards_intent(self) -> bool {
        matches!(self, ExitReason::Retired)
    }

    /// Whether the final observed state is worth keeping.
    ///
    /// Only a give-up says something nothing else records. The watcher writes
    /// `Failed` and exits; if teardown then deletes that file the machine does
    /// not show as failed, it silently disappears from `pf list` and the TUI.
    fn keeps_state(self) -> bool {
        matches!(self, ExitReason::GaveUp)
    }

    fn label(self) -> &'static str {
        match self {
            ExitReason::Retired => "retired",
            ExitReason::Signalled => "interrupted",
            ExitReason::GaveUp => "gave up",
        }
    }
}

/// Apply a departing watcher's file policy.
///
/// Split out of `run` so the rule that a reboot must not erase intent is
/// testable without an ssh connection.
pub fn tear_down_files(run: &std::path::Path, host: &str, reason: ExitReason) -> Result<()> {
    // The socket and the lock only mean something while this process is alive.
    store::clear_transient_in(run, host)?;

    if reason.discards_intent() {
        store::remove_session_in(run, host)
    } else if reason.keeps_state() {
        Ok(())
    } else {
        store::remove_state_in(run, host)
    }
}

pub fn needs_master(ssh: &dyn SshControl, host: &str) -> bool {
    !ssh.check(host)
}

/// Reset observed state after losing the master. Errors are cleared too: a
/// stale one is misleading, and it would suppress the retry that the reconnect
/// is specifically meant to trigger.
pub fn mark_all_pending(observed: &mut [ForwardObs]) {
    for f in observed.iter_mut() {
        f.status = AttachStatus::Pending;
        f.attached_at = None;
        f.error = None;
    }
}

macro_rules! slog {
    ($host:expr, $($arg:tt)*) => {
        eprintln!(
            "{} [{}] {}",
            chrono::Local::now().format("%m-%d %H:%M:%S"),
            $host,
            format_args!($($arg)*)
        )
    };
}

/// Write the desired set into observed state as failed, with the reason.
///
/// A session that gives up before it ever connects has nothing in its observed
/// list, so the `Failed` state it leaves behind would describe no forwards at
/// all — `pf list` would print an empty table and the give-up would be just as
/// invisible as deleting the file. What the user wants to see is the forwards
/// that are not running and why.
pub fn mark_all_failed(desired: &DesiredSession, observed: &mut Vec<ForwardObs>, error: &str) {
    observed.retain(|o| desired.forwards.iter().any(|d| d.name == o.name));
    for d in &desired.forwards {
        match observed.iter_mut().find(|o| o.name == d.name) {
            Some(o) => {
                o.status = AttachStatus::Failed;
                o.attached_at = None;
                o.error = Some(error.to_string());
            }
            None => observed.push(ForwardObs {
                name: d.name.clone(),
                local_port: d.local_port,
                remote_host: d.remote_host.clone(),
                remote_port: d.remote_port,
                status: AttachStatus::Failed,
                attached_at: None,
                error: Some(error.to_string()),
            }),
        }
    }
}

/// Log a terminal failure and record it in observed state before leaving.
///
/// Always paired, because a `Failed` status that is never written is a machine
/// that disappears instead of one that explains itself.
macro_rules! give_up {
    ($run:expr, $state:expr, $desired:expr, $host:expr, $($arg:tt)*) => {{
        let why = format!($($arg)*);
        slog!($host, "{why}");
        $state.status = SessionStatus::Failed;
        mark_all_failed($desired, &mut $state.forwards, &why);
        let _ = store::save_state_in($run, &$state);
        ExitReason::GaveUp
    }};
}

/// Spawn a detached `pf watcher --host <host>`.
pub fn spawn(host: &str) -> Result<u32> {
    paths::ensure_dirs()?;

    let exe = std::env::current_exe()
        .map_err(|e| crate::error::PfError::Other(format!("Cannot find own executable: {e}")))?;
    let key = paths::sanitize_host(host);
    let log_path = paths::session_log_file(&key)?;

    let log_file = OpenOptions::new().create(true).append(true).open(&log_path)?;
    let log_err = log_file.try_clone()?;

    let mut cmd = std::process::Command::new(exe);
    cmd.args(["watcher", "--host", host])
        .stdin(std::process::Stdio::null())
        .stdout(log_file)
        .stderr(log_err);

    unsafe {
        cmd.pre_exec(|| {
            nix::unistd::setsid().map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            Ok(())
        });
    }

    let child = cmd.spawn()?;
    let pid = child.id();

    // Give the watcher a moment to write its first state file.
    std::thread::sleep(Duration::from_millis(300));

    Ok(pid)
}

/// The daemon entry point (`pf watcher --host <host>`).
pub fn run(host: String) {
    let term = Arc::new(AtomicBool::new(false));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&term));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&term));

    // SIGUSR1 is the "desired state changed, reconcile now" nudge.
    let wake = Arc::new(AtomicBool::new(false));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGUSR1, Arc::clone(&wake));

    let run_dir = match paths::run_dir() {
        Ok(d) => d,
        Err(e) => {
            slog!(host, "Cannot resolve run dir: {e}");
            return;
        }
    };
    let key = paths::sanitize_host(&host);
    let ssh = RealSsh::new(run_dir.clone());

    let initial = match store::load_desired_in(&run_dir, &host) {
        Ok(Some(d)) => d,
        Ok(None) => {
            slog!(host, "No desired state; nothing to do");
            return;
        }
        Err(e) => {
            slog!(host, "Cannot read desired state: {e}");
            return;
        }
    };

    let mut state = SessionState::new(
        host.clone(),
        std::process::id(),
        initial.auto_reconnect,
        initial.retry,
    );
    let _ = store::save_state_in(&run_dir, &state);

    let mut master: Option<std::process::Child> = None;
    let mut master_started = Instant::now();
    let mut retries = 0u32;

    let reason = loop {
        if term.load(Ordering::Relaxed) {
            slog!(host, "Received shutdown signal");
            break ExitReason::Signalled;
        }

        let desired = match store::load_desired_in(&run_dir, &host) {
            Ok(Some(d)) => d,
            Ok(None) => {
                slog!(host, "Desired state removed; shutting down");
                break ExitReason::Retired;
            }
            Err(e) => {
                slog!(host, "Cannot read desired state: {e}");
                std::thread::sleep(poll_interval());
                continue;
            }
        };

        if should_exit(&desired) {
            slog!(host, "No forwards remain; shutting down session");
            break ExitReason::Retired;
        }

        state.auto_reconnect = desired.auto_reconnect;
        state.retry = desired.retry;

        // Reap a dead master before deciding anything.
        if let Some(child) = master.as_mut() {
            if let Ok(Some(exit)) = child.try_wait() {
                let uptime = master_started.elapsed().as_secs();
                slog!(host, "Master exited with {exit} after {uptime}s");
                master = None;
                state.master_pid = None;
                state.connected_at = None;
                mark_all_pending(&mut state.forwards);

                if !desired.auto_reconnect {
                    break give_up!(
                        &run_dir,
                        state,
                        &desired,
                        host,
                        "Auto-reconnect disabled, exiting"
                    );
                }

                retries = next_retry_count(retries, uptime);
                if desired.retry.max_retries > 0 && retries > desired.retry.max_retries {
                    break give_up!(
                        &run_dir,
                        state,
                        &desired,
                        host,
                        "Max retries ({}) exceeded, giving up",
                        desired.retry.max_retries
                    );
                }

                let delay = backoff_delay(&desired.retry, retries, jitter_nanos());
                state.status = SessionStatus::Reconnecting;
                state.reconnect_count += 1;
                let _ = store::save_state_in(&run_dir, &state);
                slog!(host, "Reconnecting in {delay}s (attempt {retries})...");

                for _ in 0..(delay * 10) {
                    if term.load(Ordering::Relaxed) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                continue;
            }
        }

        if master.is_none() && needs_master(&ssh, &host) {
            slog!(host, "Starting SSH master");
            state.status = SessionStatus::Connecting;
            let _ = store::save_state_in(&run_dir, &state);

            let log_path = match paths::session_log_file(&key) {
                Ok(p) => p,
                Err(e) => {
                    break give_up!(
                        &run_dir,
                        state,
                        &desired,
                        host,
                        "Cannot resolve log path: {e}"
                    )
                }
            };
            let log_file = match OpenOptions::new().create(true).append(true).open(&log_path) {
                Ok(f) => f,
                Err(e) => break give_up!(&run_dir, state, &desired, host, "Cannot open log: {e}"),
            };
            let log_err = match log_file.try_clone() {
                Ok(f) => f,
                Err(e) => {
                    break give_up!(
                        &run_dir,
                        state,
                        &desired,
                        host,
                        "Cannot clone log handle: {e}"
                    )
                }
            };

            match ssh
                .master_command(&host)
                .stdin(std::process::Stdio::null())
                .stdout(log_file)
                .stderr(log_err)
                .spawn()
            {
                Ok(child) => {
                    state.master_pid = Some(child.id());
                    master = Some(child);
                    master_started = Instant::now();
                    slog!(host, "Master started (pid {:?})", state.master_pid);
                }
                Err(e) => {
                    slog!(host, "Failed to spawn master: {e}");
                    state.status = SessionStatus::Failed;
                    let _ = store::save_state_in(&run_dir, &state);
                    std::thread::sleep(poll_interval());
                    continue;
                }
            }

            // Wait for the control socket to accept requests before attaching.
            // Honouring `term` here matters: a host that never resolves keeps
            // this loop busy for ten seconds, and at shutdown the OS follows
            // its SIGTERM with a SIGKILL long before that.
            for _ in 0..100 {
                if ssh.check(&host) || term.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            if !ssh.check(&host) {
                slog!(host, "Master did not become ready");
                continue;
            }

            state.status = SessionStatus::Connected;
            state.connected_at = Some(Utc::now());
            if master_started.elapsed().as_secs() >= HEALTHY_UPTIME_SECS {
                retries = 0;
            }
        }

        if ssh.check(&host) {
            state.status = SessionStatus::Connected;
            let actions = reconcile(&desired, &state.forwards);
            if !actions.is_empty() {
                for line in apply(&actions, &host, &ssh, &mut state.forwards) {
                    slog!(host, "{line}");
                }
            }
        }

        let _ = store::save_state_in(&run_dir, &state);

        // Sleep in slices so SIGUSR1 and SIGTERM are noticed promptly.
        wake.store(false, Ordering::Relaxed);
        let slices = poll_interval().as_millis() / 50;
        for _ in 0..slices {
            if term.load(Ordering::Relaxed) || wake.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    };

    // Teardown. The ssh connection always goes; which files go depends on why
    // we are leaving.
    ssh.exit(&host);
    if let Some(mut child) = master {
        let _ = child.kill();
        let _ = child.wait();
    }
    let _ = tear_down_files(&run_dir, &host, reason);
    slog!(host, "Watcher exiting ({})", reason.label());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::ssh::FakeSsh;
    use crate::session::{AttachStatus, DesiredForward, DesiredSession, ForwardObs};
    use crate::watcher::RetryPolicy;

    fn with_forwards(names: &[&str]) -> DesiredSession {
        let mut s = DesiredSession::new("gpu-01".to_string(), true, RetryPolicy::default());
        for (i, n) in names.iter().enumerate() {
            s.upsert(DesiredForward {
                name: n.to_string(),
                local_port: 8000 + i as u16,
                remote_host: "localhost".to_string(),
                remote_port: 80,
            });
        }
        s
    }

    #[test]
    fn an_empty_desired_set_means_the_session_should_end() {
        assert!(should_exit(&with_forwards(&[])));
        assert!(!should_exit(&with_forwards(&["a"])));
    }

    #[test]
    fn a_missing_master_needs_starting() {
        let ssh = FakeSsh::new();
        ssh.connected.set(false);
        assert!(needs_master(&ssh, "gpu-01"));

        ssh.connected.set(true);
        assert!(!needs_master(&ssh, "gpu-01"));
    }

    #[test]
    fn losing_the_master_marks_every_forward_pending() {
        let mut obs = vec![
            ForwardObs {
                name: "a".to_string(),
                local_port: 1,
                remote_host: "localhost".to_string(),
                remote_port: 1,
                status: AttachStatus::Forwarded,
                attached_at: Some(chrono::Utc::now()),
                error: None,
            },
            ForwardObs {
                name: "b".to_string(),
                local_port: 2,
                remote_host: "localhost".to_string(),
                remote_port: 2,
                status: AttachStatus::Failed,
                attached_at: None,
                error: Some("boom".to_string()),
            },
        ];

        mark_all_pending(&mut obs);

        assert!(obs.iter().all(|f| f.status == AttachStatus::Pending));
        assert!(obs.iter().all(|f| f.attached_at.is_none()));
        // A stale error from the previous connection would be misleading, and
        // it would also suppress the retry the reconnect is meant to trigger.
        assert!(obs.iter().all(|f| f.error.is_none()));
    }

    #[test]
    fn giving_up_names_the_forwards_that_are_not_running() {
        // A session that never connects has an empty observed list. Keeping its
        // `Failed` state file only helps if that file says which forwards it is
        // talking about.
        let desired = with_forwards(&["jupyter", "tensorboard"]);
        let mut observed = Vec::new();

        mark_all_failed(&desired, &mut observed, "Max retries (3) exceeded");

        assert_eq!(observed.len(), 2);
        assert!(observed.iter().all(|f| f.status == AttachStatus::Failed));
        assert!(observed
            .iter()
            .all(|f| f.error.as_deref() == Some("Max retries (3) exceeded")));
        let names: Vec<&str> = observed.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["jupyter", "tensorboard"]);
    }

    #[test]
    fn giving_up_overwrites_what_was_observed_and_drops_what_is_gone() {
        let desired = with_forwards(&["jupyter"]);
        let mut observed = vec![
            ForwardObs {
                name: "jupyter".to_string(),
                local_port: 8000,
                remote_host: "localhost".to_string(),
                remote_port: 80,
                status: AttachStatus::Forwarded,
                attached_at: Some(chrono::Utc::now()),
                error: None,
            },
            // No longer desired, so it has no business in the final snapshot.
            ForwardObs {
                name: "stale".to_string(),
                local_port: 9000,
                remote_host: "localhost".to_string(),
                remote_port: 90,
                status: AttachStatus::Forwarded,
                attached_at: None,
                error: None,
            },
        ];

        mark_all_failed(&desired, &mut observed, "Auto-reconnect disabled");

        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].name, "jupyter");
        assert_eq!(observed[0].status, AttachStatus::Failed);
        assert!(observed[0].attached_at.is_none());
        assert_eq!(
            observed[0].error.as_deref(),
            Some("Auto-reconnect disabled")
        );
    }

    /// Lay down the four files a running session owns.
    fn seed_session(run: &std::path::Path, host: &str) {
        store::update_desired_in(run, host, |s| {
            s.upsert(DesiredForward {
                name: "jupyter".to_string(),
                local_port: 8888,
                remote_host: "localhost".to_string(),
                remote_port: 8888,
            });
        })
        .unwrap();

        let mut state = SessionState::new(host.to_string(), 4242, true, RetryPolicy::default());
        state.status = SessionStatus::Failed;
        store::save_state_in(run, &state).unwrap();

        let key = paths::sanitize_host(host);
        std::fs::write(paths::socket_file_in(run, &key), "").unwrap();
        std::fs::write(paths::lock_file_in(run, &key), "").unwrap();
    }

    fn exists(run: &std::path::Path, host: &str) -> (bool, bool, bool, bool) {
        let key = paths::sanitize_host(host);
        (
            paths::desired_file_in(run, &key).exists(),
            paths::state_file_in(run, &key).exists(),
            paths::socket_file_in(run, &key).exists(),
            paths::lock_file_in(run, &key).exists(),
        )
    }

    #[test]
    fn retiring_a_session_takes_its_intent_with_it() {
        let d = tempfile::tempdir().unwrap();
        seed_session(d.path(), "gpu-01");

        tear_down_files(d.path(), "gpu-01", ExitReason::Retired).unwrap();

        assert_eq!(
            exists(d.path(), "gpu-01"),
            (false, false, false, false),
            "an emptied desired set means the session is over; nothing should survive"
        );
    }

    #[test]
    fn a_reboot_does_not_erase_the_forwards_you_had() {
        // Regression: macOS SIGTERMs every process at shutdown. Treating that
        // as "cancel my forwards" is what left an overnight reboot with an
        // empty run directory and no way to know what had been running.
        let d = tempfile::tempdir().unwrap();
        seed_session(d.path(), "gpu-01");

        tear_down_files(d.path(), "gpu-01", ExitReason::Signalled).unwrap();

        let (desired, state, sock, lock) = exists(d.path(), "gpu-01");
        assert!(desired, "intent must outlive a signal it did not ask for");
        assert!(
            !state,
            "observed state is a snapshot of a process that is gone"
        );
        assert!(
            !sock && !lock,
            "socket and lock are meaningless without a watcher"
        );

        let restored = store::load_desired_in(d.path(), "gpu-01").unwrap().unwrap();
        assert_eq!(restored.forwards.len(), 1);
        assert_eq!(restored.forwards[0].name, "jupyter");
    }

    #[test]
    fn giving_up_leaves_the_failure_where_it_can_be_seen() {
        // The watcher writes `Failed` and exits. If teardown then deletes that
        // file, the machine does not show as failed — it just disappears.
        let d = tempfile::tempdir().unwrap();
        seed_session(d.path(), "gpu-01");

        tear_down_files(d.path(), "gpu-01", ExitReason::GaveUp).unwrap();

        let (desired, state, sock, lock) = exists(d.path(), "gpu-01");
        assert!(desired, "giving up is not a decision to drop the forwards");
        assert!(
            state,
            "the whole point is that `pf list` can still show why"
        );
        assert!(!sock && !lock);

        let kept = store::load_state_in(d.path(), "gpu-01").unwrap().unwrap();
        assert_eq!(kept.status, SessionStatus::Failed);
    }

    #[test]
    fn tearing_down_a_host_that_owns_nothing_is_not_an_error() {
        let d = tempfile::tempdir().unwrap();
        for reason in [
            ExitReason::Retired,
            ExitReason::Signalled,
            ExitReason::GaveUp,
        ] {
            assert!(tear_down_files(d.path(), "never-existed", reason).is_ok());
        }
    }
}

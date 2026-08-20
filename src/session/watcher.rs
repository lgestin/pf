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

    loop {
        if term.load(Ordering::Relaxed) {
            slog!(host, "Received shutdown signal");
            break;
        }

        let desired = match store::load_desired_in(&run_dir, &host) {
            Ok(Some(d)) => d,
            Ok(None) => {
                slog!(host, "Desired state removed; shutting down");
                break;
            }
            Err(e) => {
                slog!(host, "Cannot read desired state: {e}");
                std::thread::sleep(poll_interval());
                continue;
            }
        };

        if should_exit(&desired) {
            slog!(host, "No forwards remain; shutting down session");
            break;
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
                    slog!(host, "Auto-reconnect disabled, exiting");
                    state.status = SessionStatus::Failed;
                    let _ = store::save_state_in(&run_dir, &state);
                    break;
                }

                retries = next_retry_count(retries, uptime);
                if desired.retry.max_retries > 0 && retries > desired.retry.max_retries {
                    slog!(
                        host,
                        "Max retries ({}) exceeded, giving up",
                        desired.retry.max_retries
                    );
                    state.status = SessionStatus::Failed;
                    let _ = store::save_state_in(&run_dir, &state);
                    break;
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
                Err(_) => break,
            };
            let log_file = match OpenOptions::new().create(true).append(true).open(&log_path) {
                Ok(f) => f,
                Err(e) => {
                    slog!(host, "Cannot open log: {e}");
                    break;
                }
            };
            let log_err = match log_file.try_clone() {
                Ok(f) => f,
                Err(e) => {
                    slog!(host, "Cannot clone log handle: {e}");
                    break;
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
            for _ in 0..100 {
                if ssh.check(&host) {
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
    }

    // Teardown.
    ssh.exit(&host);
    if let Some(mut child) = master {
        let _ = child.kill();
        let _ = child.wait();
    }
    let _ = store::remove_session_in(&run_dir, &host);
    slog!(host, "Watcher exiting");
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
                status: AttachStatus::Attached,
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
}

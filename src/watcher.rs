use crate::error::Result;
use crate::paths;
use crate::process;
use crate::state::{ForwardState, ForwardStatus};
use crate::tunnel::TunnelParams;
use chrono::Utc;
use std::fs::OpenOptions;
use std::os::unix::process::CommandExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub const DEFAULT_INITIAL_DELAY: u64 = 5;
pub const DEFAULT_MAX_DELAY: u64 = 300;

/// Write a timestamped watcher line to stderr, which is the forward's log file.
/// Timestamps make it possible to read how long a tunnel actually stayed up.
macro_rules! wlog {
    ($name:expr, $($arg:tt)*) => {
        eprintln!(
            "{} [{}] {}",
            chrono::Local::now().format("%m-%d %H:%M:%S"),
            $name,
            format_args!($($arg)*)
        )
    };
}

/// Uptime that counts as the tunnel having genuinely worked. Above ssh's
/// `ConnectTimeout=10` and banner exchange timeouts, so a fast failure can't
/// masquerade as success and reset the backoff.
const HEALTHY_UPTIME_SECS: u64 = 60;

/// Retry pacing for a watcher's reconnect loop.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Max reconnect attempts (0 = unlimited).
    pub max_retries: u32,
    /// Delay before the first retry, in seconds.
    pub initial_delay: u64,
    /// Upper bound on the backoff delay, in seconds.
    pub max_delay: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 0,
            initial_delay: DEFAULT_INITIAL_DELAY,
            max_delay: DEFAULT_MAX_DELAY,
        }
    }
}

/// Exponential curve, capped at `max_delay`. Jitter applied separately.
fn capped_delay(policy: &RetryPolicy, retries: u32) -> u64 {
    if retries == 0 {
        return 0;
    }
    // Clamp before shifting: retry counts in the tens of thousands are real.
    let exp = (retries - 1).min(32);
    policy
        .initial_delay
        .saturating_mul(1u64 << exp)
        .min(policy.max_delay)
}

/// Spread `delay` over `[delay / 2, delay]`, so watchers that failed together
/// (one expired Access token drops every tunnel to a host) don't retry in
/// lockstep and pile onto cloudflared's token lock.
fn apply_jitter(delay: u64, jitter_nanos: u32) -> u64 {
    let half = delay / 2;
    if half == 0 {
        return delay;
    }
    half + u64::from(jitter_nanos) % (half + 1)
}

fn backoff_delay(policy: &RetryPolicy, retries: u32, jitter_nanos: u32) -> u64 {
    apply_jitter(capped_delay(policy, retries), jitter_nanos)
}

/// Retry count for the next attempt, given how long the SSH process that just
/// died had been up. A tunnel that stayed up resets the backoff; one that died
/// fast escalates it.
fn next_retry_count(retries: u32, uptime_secs: u64) -> u32 {
    if uptime_secs >= HEALTHY_UPTIME_SECS {
        1
    } else {
        retries.saturating_add(1)
    }
}

fn jitter_nanos() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
}

/// Spawn a watcher daemon by re-execing `pf watcher ...` as a detached process.
pub fn spawn_watcher(
    name: &str,
    host: &str,
    local_port: u16,
    remote_port: u16,
    remote_host: &str,
    reconnect: bool,
    policy: RetryPolicy,
) -> Result<u32> {
    process::check_name_available(name)?;

    if process::is_port_in_use(local_port) {
        return Err(crate::error::PfError::PortInUse(local_port));
    }

    paths::ensure_dirs()?;

    let exe = std::env::current_exe().map_err(|e| {
        crate::error::PfError::Other(format!("Cannot find own executable: {e}"))
    })?;

    let log_path = paths::log_file(name)?;

    let mut cmd = std::process::Command::new(exe);
    cmd.args([
        "watcher",
        "--name",
        name,
        "--host",
        host,
        "--local-port",
        &local_port.to_string(),
        "--remote-port",
        &remote_port.to_string(),
        "--remote-host",
        remote_host,
        "--max-retries",
        &policy.max_retries.to_string(),
        "--retry-delay",
        &policy.initial_delay.to_string(),
        "--max-retry-delay",
        &policy.max_delay.to_string(),
    ]);
    if reconnect {
        cmd.arg("--reconnect");
    }

    // Detach the watcher: redirect stdio to log, use setsid via pre_exec
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let log_err = log_file.try_clone()?;

    cmd.stdin(std::process::Stdio::null())
        .stdout(log_file)
        .stderr(log_err);

    // Use setsid to fully detach on unix
    unsafe {
        cmd.pre_exec(|| {
            nix::unistd::setsid().map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            Ok(())
        });
    }

    let child = cmd.spawn()?;
    let pid = child.id();

    // Give the watcher a moment to write its state file
    std::thread::sleep(std::time::Duration::from_millis(300));

    Ok(pid)
}

/// The actual watcher daemon entry point (called via `pf watcher`).
pub fn run_watcher(
    name: String,
    host: String,
    local_port: u16,
    remote_port: u16,
    remote_host: String,
    reconnect: bool,
    policy: RetryPolicy,
) {
    // Set up signal handling for graceful shutdown
    let term = Arc::new(AtomicBool::new(false));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&term));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&term));

    let watcher_pid = std::process::id();

    // Write initial state
    let mut state = ForwardState {
        name: name.clone(),
        host: host.clone(),
        local_port,
        remote_port,
        remote_host: remote_host.clone(),
        watcher_pid,
        ssh_pid: None,
        status: ForwardStatus::Running,
        started_at: Utc::now(),
        reconnect_count: 0,
        auto_reconnect: reconnect,
        max_retries: policy.max_retries,
        retry_delay: policy.initial_delay,
        max_retry_delay: policy.max_delay,
    };
    if let Err(e) = state.save() {
        wlog!("pf watcher", "Failed to save state: {e}");
        return;
    }

    let params = TunnelParams {
        host: host.clone(),
        local_port,
        remote_port,
        remote_host: remote_host.clone(),
    };

    let mut retries = 0u32;

    loop {
        if term.load(Ordering::Relaxed) {
            // Received shutdown signal
            wlog!(name, "Received shutdown signal");
            break;
        }

        wlog!(name, "Starting SSH tunnel ({}:{} via {})", local_port, remote_port, host);

        let log_path = match paths::log_file(&name) {
            Ok(p) => p,
            Err(_) => break,
        };
        let log_file = match OpenOptions::new().create(true).append(true).open(&log_path) {
            Ok(f) => f,
            Err(e) => {
                wlog!(name, "Failed to open log: {e}");
                break;
            }
        };

        let mut child = match params.spawn(log_file) {
            Ok(c) => c,
            Err(e) => {
                wlog!(name, "Failed to spawn SSH: {e}");
                state.status = ForwardStatus::Failed;
                let _ = state.save();
                break;
            }
        };

        let ssh_pid = child.id();
        let started = Instant::now();
        state.ssh_pid = Some(ssh_pid);
        state.status = ForwardStatus::Running;
        let _ = state.save();

        wlog!(name, "SSH tunnel started (pid {})", ssh_pid);

        // Wait for SSH to exit, checking for shutdown signal periodically
        loop {
            if term.load(Ordering::Relaxed) {
                wlog!(name, "Shutting down SSH (pid {})", ssh_pid);
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            match child.try_wait() {
                Ok(Some(exit)) => {
                    wlog!(name, "SSH exited with {} after {}s", exit, started.elapsed().as_secs());
                    break;
                }
                Ok(None) => {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                Err(e) => {
                    wlog!(name, "Error waiting for SSH: {e}");
                    break;
                }
            }
        }

        if term.load(Ordering::Relaxed) {
            break;
        }

        // SSH died unexpectedly
        state.ssh_pid = None;
        let uptime = started.elapsed().as_secs();

        if !reconnect {
            wlog!(name, "Auto-reconnect disabled, exiting");
            state.status = ForwardStatus::Failed;
            let _ = state.save();
            break;
        }

        if uptime >= HEALTHY_UPTIME_SECS && retries > 0 {
            wlog!(name, "Was up {}s, resetting backoff", uptime);
        }
        retries = next_retry_count(retries, uptime);
        if policy.max_retries > 0 && retries > policy.max_retries {
            wlog!(name, "Max retries ({}) exceeded, giving up", policy.max_retries);
            state.status = ForwardStatus::Failed;
            let _ = state.save();
            break;
        }

        let delay = backoff_delay(&policy, retries, jitter_nanos());

        state.status = ForwardStatus::Reconnecting;
        state.reconnect_count += 1;
        let _ = state.save();

        wlog!(
            name,
            "Reconnecting in {}s (attempt {}{})...",
            delay,
            retries,
            if policy.max_retries > 0 {
                format!("/{}", policy.max_retries)
            } else {
                String::new()
            }
        );

        // Wait for the backoff delay, but check for shutdown signal
        for _ in 0..(delay * 10) {
            if term.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    // Clean up
    state.ssh_pid = None;
    state.status = ForwardStatus::Stopped;
    let _ = state.save();
    let _ = ForwardState::remove(&name);
    wlog!(name, "Watcher exiting");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> RetryPolicy {
        RetryPolicy {
            max_retries: 0,
            initial_delay: 5,
            max_delay: 300,
        }
    }

    #[test]
    fn escalates_exponentially_then_saturates_at_cap() {
        let p = policy();
        let seq: Vec<u64> = (1..=9).map(|r| capped_delay(&p, r)).collect();
        assert_eq!(seq, vec![5, 10, 20, 40, 80, 160, 300, 300, 300]);
    }

    #[test]
    fn zero_retries_means_no_delay() {
        assert_eq!(capped_delay(&policy(), 0), 0);
    }

    #[test]
    fn survives_the_retry_counts_seen_in_real_logs() {
        // A failing tunnel reached 17,155 attempts before this was fixed.
        let p = policy();
        assert_eq!(capped_delay(&p, 17_155), 300);
        assert_eq!(capped_delay(&p, u32::MAX), 300);
    }

    #[test]
    fn absurd_initial_delay_saturates_instead_of_overflowing() {
        let p = RetryPolicy {
            max_retries: 0,
            initial_delay: u64::MAX,
            max_delay: 300,
        };
        assert_eq!(capped_delay(&p, 5), 300);
    }

    #[test]
    fn max_delay_below_initial_delay_is_respected() {
        let p = RetryPolicy {
            max_retries: 0,
            initial_delay: 60,
            max_delay: 10,
        };
        assert_eq!(capped_delay(&p, 1), 10);
    }

    #[test]
    fn jitter_stays_within_half_the_delay_and_the_delay() {
        for delay in [2u64, 5, 10, 300, 900] {
            for nanos in [0u32, 1, 7, 12_345, 499_999_999, u32::MAX] {
                let got = apply_jitter(delay, nanos);
                assert!(
                    got >= delay / 2 && got <= delay,
                    "apply_jitter({delay}, {nanos}) = {got}, outside [{}, {delay}]",
                    delay / 2
                );
            }
        }
    }

    #[test]
    fn jitter_actually_varies() {
        let spread: std::collections::HashSet<u64> =
            (0..1000).map(|n| apply_jitter(300, n * 7919)).collect();
        assert!(spread.len() > 50, "jitter barely varied: {} values", spread.len());
    }

    #[test]
    fn tiny_delays_are_left_alone() {
        assert_eq!(apply_jitter(0, 12345), 0);
        assert_eq!(apply_jitter(1, 12345), 1);
    }

    #[test]
    fn fast_failures_escalate() {
        assert_eq!(next_retry_count(0, 0), 1);
        assert_eq!(next_retry_count(1, 3), 2);
        assert_eq!(next_retry_count(9, HEALTHY_UPTIME_SECS - 1), 10);
    }

    #[test]
    fn a_tunnel_that_stayed_up_resets_the_backoff() {
        assert_eq!(next_retry_count(50, HEALTHY_UPTIME_SECS), 1);
        assert_eq!(next_retry_count(17_155, 3600), 1);
    }

    #[test]
    fn ssh_connect_timeout_cannot_look_like_success() {
        // ConnectTimeout=10 plus banner exchange must stay well under the
        // healthy threshold, or a failing tunnel would reset forever.
        for uptime in [0u64, 10, 20, 30, 45, 59] {
            assert_eq!(next_retry_count(4, uptime), 5, "uptime {uptime} reset the backoff");
        }
    }

    #[test]
    fn retry_count_does_not_overflow() {
        assert_eq!(next_retry_count(u32::MAX, 0), u32::MAX);
    }

    #[test]
    fn backoff_delay_never_exceeds_the_cap() {
        let p = policy();
        for r in 1..=50u32 {
            let d = backoff_delay(&p, r, jitter_nanos());
            assert!(d <= p.max_delay, "retry {r} produced {d}s");
        }
    }
}

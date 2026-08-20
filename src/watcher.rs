//! Retry pacing shared by every session watcher.
//!
//! The per-forward watcher daemon that used to live here was replaced by
//! `session::watcher`, which owns one multiplexed ssh master per host. This
//! backoff logic moved across unchanged, tests and all.

use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_INITIAL_DELAY: u64 = 5;
pub const DEFAULT_MAX_DELAY: u64 = 300;

/// Uptime that counts as the tunnel having genuinely worked. Above ssh's
/// `ConnectTimeout=10` and banner exchange timeouts, so a fast failure can't
/// masquerade as success and reset the backoff.
pub(crate) const HEALTHY_UPTIME_SECS: u64 = 60;

/// Retry pacing for a watcher's reconnect loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

pub(crate) fn backoff_delay(policy: &RetryPolicy, retries: u32, jitter_nanos: u32) -> u64 {
    apply_jitter(capped_delay(policy, retries), jitter_nanos)
}

/// Retry count for the next attempt, given how long the SSH process that just
/// died had been up. A tunnel that stayed up resets the backoff; one that died
/// fast escalates it.
pub(crate) fn next_retry_count(retries: u32, uptime_secs: u64) -> u32 {
    if uptime_secs >= HEALTHY_UPTIME_SECS {
        1
    } else {
        retries.saturating_add(1)
    }
}

pub(crate) fn jitter_nanos() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
}

/// Spawn a watcher daemon by re-execing `pf watcher ...` as a detached process.
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

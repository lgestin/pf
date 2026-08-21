use crate::error::{PfError, Result};
use crate::paths;
use crate::session::{store, DesiredForward};
use crate::watcher::RetryPolicy;
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use std::path::Path;

pub fn is_alive(pid: u32) -> bool {
    signal::kill(Pid::from_raw(pid as i32), None).is_ok()
}

pub fn kill_process(pid: u32) -> Result<()> {
    let pid = Pid::from_raw(pid as i32);
    signal::kill(pid, Signal::SIGTERM).map_err(|e| PfError::Other(format!("Failed to kill PID {}: {}", pid, e)))?;
    Ok(())
}

/// Is `port` unavailable on **either** loopback address?
///
/// ssh's `LocalForward` binds `127.0.0.1` and `::1` as two separate specific
/// addresses, and `ssh -O forward` exits 0 if it gets *either* one. So a
/// listener on only `::1` produces a forward that ssh calls a success, `pf list`
/// calls `running`, and that silently fails for any client whose `localhost`
/// resolves to `::1`. Checking one family would let exactly that through.
///
/// Verified against OpenSSH 9.9 on 2026-08-20: with both loopback addresses
/// taken, `-O forward` exits 255 with "Port forwarding failed"; with only one
/// taken, it exits 0 having bound just the other.
pub fn is_port_in_use(port: u16) -> bool {
    use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};

    TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_err()
        || TcpListener::bind((Ipv6Addr::LOCALHOST, port)).is_err()
}

/// Which host owns a given forward name. Names are globally unique, so the
/// first match is the only match.
pub fn find_host_for_forward_in(run: &Path, name: &str) -> Result<Option<String>> {
    if !run.exists() {
        return Ok(None);
    }
    for entry in std::fs::read_dir(run)? {
        let path = entry?.path();
        let Some(file) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !file.ends_with(".desired.json") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(session) = serde_json::from_str::<crate::session::DesiredSession>(&raw) else {
            continue;
        };
        if session.forwards.iter().any(|f| f.name == name) {
            return Ok(Some(session.host));
        }
    }
    Ok(None)
}

pub fn find_host_for_forward(name: &str) -> Result<Option<String>> {
    find_host_for_forward_in(&paths::run_dir()?, name)
}

/// Is a host's session actually being supervised right now?
///
/// A desired set that no live watcher owns is inert: it records intent that
/// outlived its process — after a reboot, or after a watcher gave up — but
/// nothing is acting on it.
pub fn has_live_watcher_in(run: &Path, host: &str) -> bool {
    matches!(store::load_state_in(run, host), Ok(Some(s)) if is_alive(s.watcher_pid))
}

pub fn has_live_watcher(host: &str) -> Result<bool> {
    Ok(has_live_watcher_in(&paths::run_dir()?, host))
}

pub fn check_name_available_in(run: &Path, name: &str) -> Result<()> {
    match find_host_for_forward_in(run, name)? {
        // "Taken" has to mean taken by something running. Intent now survives a
        // reboot, so after one every host has an orphaned desired set — and
        // refusing those names would make yesterday's forwards unstartable.
        Some(host) if has_live_watcher_in(run, &host) => {
            Err(PfError::AlreadyRunning(name.to_string()))
        }
        _ => Ok(()),
    }
}

pub fn check_name_available(name: &str) -> Result<()> {
    check_name_available_in(&paths::run_dir()?, name)
}

/// Nudge a host's watcher to reconcile immediately instead of waiting out its
/// poll interval. Best-effort: a missed signal costs half a second.
pub fn signal_watcher(host: &str) -> Result<()> {
    if let Some(state) = store::load_state(host)? {
        if is_alive(state.watcher_pid) {
            let _ = signal::kill(Pid::from_raw(state.watcher_pid as i32), Signal::SIGUSR1);
        }
    }
    Ok(())
}

/// Add a forward to its host's desired set, starting a watcher if the host has
/// none. Ref-counted: the first forward brings the session up.
#[allow(clippy::too_many_arguments)]
pub fn start_forward(
    host: &str,
    name: &str,
    local_port: u16,
    remote_host: &str,
    remote_port: u16,
    reconnect: bool,
    policy: RetryPolicy,
) -> Result<()> {
    check_name_available(name)?;

    if is_port_in_use(local_port) {
        return Err(PfError::PortInUse(local_port));
    }

    let run = paths::run_dir()?;
    // A host whose watcher is gone still has its desired set on disk. Spawning
    // a watcher hands it back that whole set, so this brings up the forwards
    // that were interrupted alongside the one being asked for.
    let had_session = has_live_watcher_in(&run, host);

    store::update_desired_in(&run, host, |s| {
        s.host = host.to_string();
        s.auto_reconnect = reconnect;
        s.retry = policy;
        s.upsert(DesiredForward {
            name: name.to_string(),
            local_port,
            remote_host: remote_host.to_string(),
            remote_port,
        });
    })?;

    if had_session {
        signal_watcher(host)?;
    } else {
        crate::session::watcher::spawn(host)?;
    }

    Ok(())
}

/// Remove one forward. Its session shuts itself down if it was the last one.
pub fn stop_forward(name: &str) -> Result<()> {
    let host = find_host_for_forward(name)?.ok_or_else(|| PfError::NotFound(name.to_string()))?;

    store::update_desired_in(&paths::run_dir()?, &host, |s| {
        s.remove(name);
    })?;

    signal_watcher(&host)?;
    Ok(())
}

/// Stop every forward on a host, taking the session down with it.
pub fn stop_host(host: &str) -> Result<()> {
    let state = store::load_state(host)?.ok_or_else(|| PfError::NotFound(host.to_string()))?;

    store::update_desired_in(&paths::run_dir()?, host, |s| {
        s.forwards.clear();
    })?;

    if is_alive(state.watcher_pid) {
        kill_process(state.watcher_pid)?;
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    store::remove_session(host)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{store, DesiredForward};

    fn seed(run: &std::path::Path, host: &str, names: &[(&str, u16)]) {
        store::update_desired_in(run, host, |s| {
            s.host = host.to_string();
            for (n, p) in names {
                s.upsert(DesiredForward {
                    name: n.to_string(),
                    local_port: *p,
                    remote_host: "localhost".to_string(),
                    remote_port: *p,
                });
            }
        })
        .unwrap();
    }

    #[test]
    fn a_forward_name_resolves_to_its_host() {
        let d = tempfile::tempdir().unwrap();
        seed(d.path(), "gpu-01", &[("jupyter", 8888)]);
        seed(d.path(), "gpu-02", &[("db", 5432)]);

        assert_eq!(
            find_host_for_forward_in(d.path(), "db").unwrap().as_deref(),
            Some("gpu-02")
        );
        assert_eq!(find_host_for_forward_in(d.path(), "nope").unwrap(), None);
    }

    #[test]
    fn an_unused_name_is_available() {
        let d = tempfile::tempdir().unwrap();
        seed(d.path(), "gpu-01", &[("jupyter", 8888)]);
        assert!(check_name_available_in(d.path(), "tensorboard").is_ok());
    }

    #[test]
    fn a_port_taken_on_either_loopback_family_counts_as_in_use() {
        use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};

        // Free on both families.
        let probe = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let free_port = probe.local_addr().unwrap().port();
        drop(probe);
        assert!(!is_port_in_use(free_port));

        // Taken on IPv4 only.
        let v4 = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let v4_port = v4.local_addr().unwrap().port();
        assert!(is_port_in_use(v4_port), "IPv4-only conflict missed");
        drop(v4);

        // Taken on IPv6 only. This is the case a 127.0.0.1-only check misses:
        // ssh would bind IPv4, exit 0, and report a half-working forward as
        // healthy.
        let v6 = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
        let v6_port = v6.local_addr().unwrap().port();
        assert!(is_port_in_use(v6_port), "IPv6-only conflict missed");
        drop(v6);
    }

    /// Claim a host's session for a watcher with the given pid. Using our own
    /// pid is the only way to name a process that is reliably alive.
    fn seed_watcher(run: &std::path::Path, host: &str, pid: u32) {
        let state = crate::session::SessionState::new(
            host.to_string(),
            pid,
            true,
            crate::watcher::RetryPolicy::default(),
        );
        store::save_state_in(run, &state).unwrap();
    }

    #[test]
    fn a_name_already_used_on_another_host_is_rejected() {
        // Names stay globally unique — `pf stop <name>` has to be unambiguous.
        let d = tempfile::tempdir().unwrap();
        seed(d.path(), "gpu-01", &[("jupyter", 8888)]);
        seed_watcher(d.path(), "gpu-01", std::process::id());

        let err = check_name_available_in(d.path(), "jupyter").unwrap_err();
        assert!(
            matches!(err, crate::error::PfError::AlreadyRunning(ref n) if n == "jupyter"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn a_name_held_only_by_a_dead_session_is_free_again() {
        // After a reboot every desired file is an orphan. If those names stayed
        // claimed, the forwards you had yesterday would be unstartable today.
        let d = tempfile::tempdir().unwrap();
        seed(d.path(), "gpu-01", &[("jupyter", 8888)]);

        // No state file at all: intent that outlived its watcher.
        assert!(check_name_available_in(d.path(), "jupyter").is_ok());

        // A state file naming a pid that is gone is equally not a conflict.
        seed_watcher(d.path(), "gpu-01", dead_pid());
        assert!(check_name_available_in(d.path(), "jupyter").is_ok());
    }

    #[test]
    fn a_session_counts_as_live_only_while_its_watcher_is() {
        let d = tempfile::tempdir().unwrap();
        seed(d.path(), "gpu-01", &[("jupyter", 8888)]);

        assert!(!has_live_watcher_in(d.path(), "gpu-01"), "no state file");

        seed_watcher(d.path(), "gpu-01", dead_pid());
        assert!(!has_live_watcher_in(d.path(), "gpu-01"), "dead pid");

        seed_watcher(d.path(), "gpu-01", std::process::id());
        assert!(has_live_watcher_in(d.path(), "gpu-01"));
    }

    /// A pid that has certainly exited: spawn something trivial and reap it.
    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    #[test]
    fn a_forward_name_still_resolves_to_a_host_whose_watcher_died() {
        // `stop` and `logs` have to reach orphaned sessions — that is the only
        // way to clear one by hand.
        let d = tempfile::tempdir().unwrap();
        seed(d.path(), "gpu-01", &[("jupyter", 8888)]);

        assert_eq!(
            find_host_for_forward_in(d.path(), "jupyter")
                .unwrap()
                .as_deref(),
            Some("gpu-01")
        );
    }
}

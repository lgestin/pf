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

pub fn check_name_available_in(run: &Path, name: &str) -> Result<()> {
    match find_host_for_forward_in(run, name)? {
        Some(_) => Err(PfError::AlreadyRunning(name.to_string())),
        None => Ok(()),
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

    let had_session = store::load_state(host)?
        .map(|s| is_alive(s.watcher_pid))
        .unwrap_or(false);

    store::update_desired_in(&paths::run_dir()?, host, |s| {
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

    #[test]
    fn a_name_already_used_on_another_host_is_rejected() {
        // Names stay globally unique — `pf stop <name>` has to be unambiguous.
        let d = tempfile::tempdir().unwrap();
        seed(d.path(), "gpu-01", &[("jupyter", 8888)]);

        let err = check_name_available_in(d.path(), "jupyter").unwrap_err();
        assert!(
            matches!(err, crate::error::PfError::AlreadyRunning(ref n) if n == "jupyter"),
            "unexpected error: {err}"
        );
    }
}

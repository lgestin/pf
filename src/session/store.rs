use crate::error::{PfError, Result};
use crate::paths;
use crate::session::{DesiredSession, SessionState};
use crate::watcher::RetryPolicy;
use nix::fcntl::{Flock, FlockArg};
use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

/// A fresh session with the default retry policy.
pub fn default_session(host: &str) -> DesiredSession {
    DesiredSession::new(host.to_string(), true, RetryPolicy::default())
}

/// Write via a sibling temp file plus rename, so a reader never observes a
/// half-written file and a crash mid-write cannot destroy the previous one.
fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let tmp = path.with_extension(format!(
        "{}.tmp{}",
        path.extension().and_then(|e| e.to_str()).unwrap_or(""),
        std::process::id()
    ));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &PathBuf) -> Result<Option<T>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)?;
    Ok(Some(serde_json::from_str(&raw)?))
}

// Every entry point below takes a RAW ssh host and sanitizes it here. Keeping
// normalization at this one boundary means no caller can accidentally key a
// file by the raw host and silently fail to find it later.

pub fn load_desired_in(run: &Path, host: &str) -> Result<Option<DesiredSession>> {
    read_json(&paths::desired_file_in(run, &paths::sanitize_host(host)))
}

pub fn load_state_in(run: &Path, host: &str) -> Result<Option<SessionState>> {
    read_json(&paths::state_file_in(run, &paths::sanitize_host(host)))
}

/// Read-modify-write the desired file under an exclusive `flock`.
///
/// `flock` rather than a pid-stamped lockfile because the kernel releases it
/// when the holder dies, so a killed `pf` cannot wedge a host forever.
pub fn update_desired_in<F>(run: &Path, host: &str, f: F) -> Result<DesiredSession>
where
    F: FnOnce(&mut DesiredSession),
{
    std::fs::create_dir_all(run)?;

    let key = paths::sanitize_host(host);
    let lock_path = paths::lock_file_in(run, &key);
    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    let _guard = Flock::lock(lock_file, FlockArg::LockExclusive)
        .map_err(|(_, e)| PfError::Lock(host.to_string(), e.to_string()))?;

    let mut session = load_desired_in(run, host)?.unwrap_or_else(|| default_session(host));
    f(&mut session);

    write_atomic(
        &paths::desired_file_in(run, &key),
        &serde_json::to_string_pretty(&session)?,
    )?;

    Ok(session)
}

pub fn save_state_in(run: &Path, state: &SessionState) -> Result<()> {
    std::fs::create_dir_all(run)?;
    write_atomic(
        &paths::state_file_in(run, &paths::sanitize_host(&state.host)),
        &serde_json::to_string_pretty(state)?,
    )
}

/// Every readable session state file. Unparseable files are skipped rather
/// than failing the whole listing, matching `ForwardState::list_all`.
pub fn list_states_in(run: &Path) -> Result<Vec<SessionState>> {
    if !run.exists() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(run)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".state.json") {
            continue;
        }
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Ok(state) = serde_json::from_str::<SessionState>(&raw) {
                out.push(state);
            }
        }
    }
    out.sort_by(|a, b| a.host.cmp(&b.host));
    Ok(out)
}

/// Every host the run directory knows about, whether or not a watcher owns it.
///
/// The host is read back out of the file rather than off the filename, because
/// the filename is a sanitized key that is deliberately lossy — feeding it back
/// to a command as a host name would not always work.
pub fn list_hosts_in(run: &Path) -> Result<Vec<String>> {
    if !run.exists() {
        return Ok(vec![]);
    }
    let mut hosts = BTreeSet::new();
    for entry in std::fs::read_dir(run)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        if name.ends_with(".desired.json") {
            if let Ok(d) = serde_json::from_str::<DesiredSession>(&raw) {
                hosts.insert(d.host);
            }
        } else if name.ends_with(".state.json") {
            if let Ok(s) = serde_json::from_str::<SessionState>(&raw) {
                hosts.insert(s.host);
            }
        }
    }
    Ok(hosts.into_iter().collect())
}

fn remove_files(paths: &[PathBuf]) -> Result<()> {
    for path in paths {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}

/// Remove everything a session owns, its desired forward set included.
pub fn remove_session_in(run: &Path, host: &str) -> Result<()> {
    let key = paths::sanitize_host(host);
    remove_files(&[
        paths::desired_file_in(run, &key),
        paths::state_file_in(run, &key),
        paths::socket_file_in(run, &key),
        paths::lock_file_in(run, &key),
    ])
}

/// Remove only the files that mean something while a watcher is alive: the
/// control socket and the lock. Both are recreated on demand, so leaving them
/// behind is just litter that a later `ssh -O check` could misread.
pub fn clear_transient_in(run: &Path, host: &str) -> Result<()> {
    let key = paths::sanitize_host(host);
    remove_files(&[
        paths::socket_file_in(run, &key),
        paths::lock_file_in(run, &key),
    ])
}

/// Remove a host's observed state, leaving its desired forward set intact.
pub fn remove_state_in(run: &Path, host: &str) -> Result<()> {
    remove_files(&[paths::state_file_in(run, &paths::sanitize_host(host))])
}

// Wrappers that resolve the real run directory. Only these three: every other
// caller already holds a run directory and uses the `_in` form directly.

pub fn load_state(host: &str) -> Result<Option<SessionState>> {
    load_state_in(&paths::run_dir()?, host)
}

pub fn list_states() -> Result<Vec<SessionState>> {
    list_states_in(&paths::run_dir()?)
}

pub fn remove_session(host: &str) -> Result<()> {
    remove_session_in(&paths::run_dir()?, host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{DesiredForward, SessionState};
    use crate::watcher::RetryPolicy;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn loading_a_missing_session_yields_none() {
        let d = tmp();
        assert!(load_desired_in(d.path(), "gpu-01").unwrap().is_none());
        assert!(load_state_in(d.path(), "gpu-01").unwrap().is_none());
    }

    #[test]
    fn update_creates_then_mutates_the_desired_file() {
        let d = tmp();

        let created = update_desired_in(d.path(), "gpu-01", |s| {
            s.upsert(DesiredForward {
                name: "jupyter".to_string(),
                local_port: 8888,
                remote_host: "localhost".to_string(),
                remote_port: 8888,
            });
        })
        .unwrap();
        assert_eq!(created.forwards.len(), 1);

        let updated = update_desired_in(d.path(), "gpu-01", |s| {
            s.upsert(DesiredForward {
                name: "tensorboard".to_string(),
                local_port: 6006,
                remote_host: "localhost".to_string(),
                remote_port: 6006,
            });
        })
        .unwrap();
        assert_eq!(updated.forwards.len(), 2);
        // upsert keeps them sorted by name
        assert_eq!(updated.forwards[0].name, "jupyter");
        assert_eq!(updated.forwards[1].name, "tensorboard");

        let reloaded = load_desired_in(d.path(), "gpu-01").unwrap().unwrap();
        assert_eq!(reloaded, updated);
    }

    #[test]
    fn writes_leave_no_temporary_files_behind() {
        let d = tmp();
        update_desired_in(d.path(), "gpu-01", |_| {}).unwrap();
        save_state_in(
            d.path(),
            &SessionState::new("gpu-01".to_string(), 1, true, RetryPolicy::default()),
        )
        .unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");
    }

    #[test]
    fn list_states_reads_every_session_and_ignores_other_files() {
        let d = tmp();
        for host in ["gpu-01", "gpu-02"] {
            save_state_in(
                d.path(),
                &SessionState::new(host.to_string(), 1, true, RetryPolicy::default()),
            )
            .unwrap();
        }
        // Files that must not be parsed as session state:
        std::fs::write(d.path().join("legacy.json"), "{}").unwrap();
        std::fs::write(d.path().join("gpu-01.desired.json"), "{}").unwrap();
        std::fs::write(d.path().join("gpu-01.sock"), "").unwrap();

        let mut hosts: Vec<String> = list_states_in(d.path())
            .unwrap()
            .into_iter()
            .map(|s| s.host)
            .collect();
        hosts.sort();
        assert_eq!(hosts, vec!["gpu-01", "gpu-02"]);
    }

    #[test]
    fn removing_a_session_deletes_desired_state_and_socket() {
        let d = tmp();
        update_desired_in(d.path(), "gpu-01", |_| {}).unwrap();
        save_state_in(
            d.path(),
            &SessionState::new("gpu-01".to_string(), 1, true, RetryPolicy::default()),
        )
        .unwrap();
        std::fs::write(d.path().join("gpu-01.sock"), "").unwrap();

        remove_session_in(d.path(), "gpu-01").unwrap();

        assert!(load_desired_in(d.path(), "gpu-01").unwrap().is_none());
        assert!(load_state_in(d.path(), "gpu-01").unwrap().is_none());
        assert!(!d.path().join("gpu-01.sock").exists());
    }

    #[test]
    fn removing_a_session_that_never_existed_is_not_an_error() {
        let d = tmp();
        remove_session_in(d.path(), "never-was").unwrap();
    }

    #[test]
    fn clearing_transients_keeps_intent_and_observed_state() {
        let d = tmp();
        update_desired_in(d.path(), "gpu-01", |_| {}).unwrap();
        save_state_in(
            d.path(),
            &SessionState::new("gpu-01".to_string(), 1, true, RetryPolicy::default()),
        )
        .unwrap();
        std::fs::write(d.path().join("gpu-01.sock"), "").unwrap();

        clear_transient_in(d.path(), "gpu-01").unwrap();

        assert!(!d.path().join("gpu-01.sock").exists());
        assert!(!d.path().join("gpu-01.lock").exists());
        assert!(load_desired_in(d.path(), "gpu-01").unwrap().is_some());
        assert!(load_state_in(d.path(), "gpu-01").unwrap().is_some());
    }

    #[test]
    fn removing_state_leaves_the_desired_set_alone() {
        let d = tmp();
        update_desired_in(d.path(), "gpu-01", |_| {}).unwrap();
        save_state_in(
            d.path(),
            &SessionState::new("gpu-01".to_string(), 1, true, RetryPolicy::default()),
        )
        .unwrap();

        remove_state_in(d.path(), "gpu-01").unwrap();

        assert!(load_state_in(d.path(), "gpu-01").unwrap().is_none());
        assert!(load_desired_in(d.path(), "gpu-01").unwrap().is_some());
    }

    #[test]
    fn listing_hosts_covers_desired_only_and_state_only_sessions() {
        let d = tmp();
        // Intent that outlived its watcher — no state file at all.
        update_desired_in(d.path(), "orphan", |_| {}).unwrap();
        // A live session, both files.
        update_desired_in(d.path(), "gpu-01", |_| {}).unwrap();
        save_state_in(
            d.path(),
            &SessionState::new("gpu-01".to_string(), 1, true, RetryPolicy::default()),
        )
        .unwrap();
        // State with no intent, which is what a half-cleaned host looks like.
        save_state_in(
            d.path(),
            &SessionState::new("stateonly".to_string(), 1, true, RetryPolicy::default()),
        )
        .unwrap();
        std::fs::write(d.path().join("noise.txt"), "ignore me").unwrap();

        assert_eq!(
            list_hosts_in(d.path()).unwrap(),
            vec!["gpu-01", "orphan", "stateonly"],
            "clean has to see hosts that only have one of the two files"
        );
    }

    #[test]
    fn listing_hosts_reports_the_raw_host_not_the_filename_key() {
        // The filename is a sanitized key that does not round-trip, so reading
        // the host off the filename would hand `clean` a name it cannot use.
        let d = tmp();
        update_desired_in(d.path(), "weird host/name", |_| {}).unwrap();

        assert_eq!(list_hosts_in(d.path()).unwrap(), vec!["weird host/name"]);
    }

    #[test]
    fn a_host_needing_sanitization_round_trips_through_every_entry_point() {
        // "gpu-01" sanitizes to itself, so it cannot catch a mismatch between
        // the key a write uses and the key a read looks for. This host can.
        let d = tmp();
        let raw = "weird host/name";

        update_desired_in(d.path(), raw, |s| {
            s.host = raw.to_string();
            s.upsert(DesiredForward {
                name: "jupyter".to_string(),
                local_port: 8888,
                remote_host: "localhost".to_string(),
                remote_port: 8888,
            });
        })
        .unwrap();

        let mut state = SessionState::new(raw.to_string(), 1, true, RetryPolicy::default());
        state.reconnect_count = 5;
        save_state_in(d.path(), &state).unwrap();

        // Reads take the raw host, exactly as writes did.
        let desired = load_desired_in(d.path(), raw).unwrap().expect("desired lost");
        assert_eq!(desired.forwards.len(), 1);
        let loaded = load_state_in(d.path(), raw).unwrap().expect("state lost");
        assert_eq!(loaded.reconnect_count, 5);
        // The struct keeps the raw host for display, even though the file is keyed.
        assert_eq!(loaded.host, raw);

        // And listing finds it, proving the file really is on disk under the key.
        assert_eq!(list_states_in(d.path()).unwrap().len(), 1);

        remove_session_in(d.path(), raw).unwrap();
        assert!(load_state_in(d.path(), raw).unwrap().is_none());
        assert!(load_desired_in(d.path(), raw).unwrap().is_none());
    }
}

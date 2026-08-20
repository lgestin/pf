use crate::error::Result;
use crate::paths;
use crate::session::{store, AttachStatus, ForwardObs, SessionState, SessionStatus};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ForwardStatus {
    Running,
    Reconnecting,
    Failed,
    Stopped,
}

impl std::fmt::Display for ForwardStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ForwardStatus::Running => write!(f, "running"),
            ForwardStatus::Reconnecting => write!(f, "reconnecting"),
            ForwardStatus::Failed => write!(f, "failed"),
            ForwardStatus::Stopped => write!(f, "stopped"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardState {
    pub name: String,
    pub host: String,
    pub local_port: u16,
    pub remote_port: u16,
    pub remote_host: String,
    pub watcher_pid: u32,
    pub ssh_pid: Option<u32>,
    pub status: ForwardStatus,
    pub started_at: DateTime<Utc>,
    pub reconnect_count: u32,
    pub auto_reconnect: bool,
    pub max_retries: u32,
    pub retry_delay: u64,
    #[serde(default = "default_max_retry_delay")]
    pub max_retry_delay: u64,
}

fn default_max_retry_delay() -> u64 {
    crate::watcher::DEFAULT_MAX_DELAY
}

/// Flatten one session into the legacy per-forward view.
///
/// This is what keeps `pf list`, `pf list --json`, `display.rs`, and the shell
/// completions working unchanged: they all still see a flat `Vec<ForwardState>`.
pub fn project(session: &SessionState) -> Vec<ForwardState> {
    session
        .forwards
        .iter()
        .map(|f: &ForwardObs| ForwardState {
            name: f.name.clone(),
            host: session.host.clone(),
            local_port: f.local_port,
            remote_port: f.remote_port,
            remote_host: f.remote_host.clone(),
            watcher_pid: session.watcher_pid,
            // The shared master is this forward's ssh process now.
            ssh_pid: session.master_pid,
            status: match (session.status, f.status) {
                (SessionStatus::Failed, _) => ForwardStatus::Failed,
                (_, AttachStatus::Failed) => ForwardStatus::Failed,
                (SessionStatus::Connected, AttachStatus::Attached) => ForwardStatus::Running,
                _ => ForwardStatus::Reconnecting,
            },
            started_at: session.started_at,
            reconnect_count: session.reconnect_count,
            auto_reconnect: session.auto_reconnect,
            max_retries: session.retry.max_retries,
            retry_delay: session.retry.initial_delay,
            max_retry_delay: session.retry.max_delay,
        })
        .collect()
}

impl ForwardState {
    pub fn load(name: &str) -> Result<Self> {
        Self::list_all()?
            .into_iter()
            .find(|f| f.name == name)
            .ok_or_else(|| crate::error::PfError::NotFound(name.to_string()))
    }

    pub fn list_all_in(run: &Path) -> Result<Vec<Self>> {
        let mut out: Vec<Self> = store::list_states_in(run)?.iter().flat_map(project).collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn list_all() -> Result<Vec<Self>> {
        Self::list_all_in(&paths::run_dir()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{AttachStatus, ForwardObs, SessionState, SessionStatus};
    use crate::watcher::RetryPolicy;

    fn session(status: SessionStatus, forwards: Vec<ForwardObs>) -> SessionState {
        let mut s = SessionState::new("gpu-01".to_string(), 4242, true, RetryPolicy::default());
        s.status = status;
        s.master_pid = Some(4243);
        s.reconnect_count = 7;
        s.forwards = forwards;
        s
    }

    fn obs(name: &str, port: u16, status: AttachStatus) -> ForwardObs {
        ForwardObs {
            name: name.to_string(),
            local_port: port,
            remote_host: "localhost".to_string(),
            remote_port: port,
            status,
            attached_at: None,
            error: None,
        }
    }

    #[test]
    fn a_connected_attached_forward_projects_as_running() {
        let s = session(
            SessionStatus::Connected,
            vec![obs("a", 1, AttachStatus::Attached)],
        );
        let flat = project(&s);

        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].name, "a");
        assert_eq!(flat[0].host, "gpu-01");
        assert_eq!(flat[0].status, ForwardStatus::Running);
        // The shared master is what the legacy `ssh_pid` field now means.
        assert_eq!(flat[0].ssh_pid, Some(4243));
        assert_eq!(flat[0].watcher_pid, 4242);
        assert_eq!(flat[0].reconnect_count, 7);
    }

    #[test]
    fn a_reconnecting_session_projects_all_its_forwards_as_reconnecting() {
        let s = session(
            SessionStatus::Reconnecting,
            vec![
                obs("a", 1, AttachStatus::Pending),
                obs("b", 2, AttachStatus::Pending),
            ],
        );
        let flat = project(&s);
        assert!(flat.iter().all(|f| f.status == ForwardStatus::Reconnecting));
    }

    #[test]
    fn a_failed_attach_projects_as_failed_even_on_a_healthy_session() {
        let s = session(
            SessionStatus::Connected,
            vec![
                obs("a", 1, AttachStatus::Attached),
                obs("b", 2, AttachStatus::Failed),
            ],
        );
        let flat = project(&s);

        let a = flat.iter().find(|f| f.name == "a").unwrap();
        let b = flat.iter().find(|f| f.name == "b").unwrap();
        assert_eq!(a.status, ForwardStatus::Running);
        assert_eq!(b.status, ForwardStatus::Failed);
    }

    #[test]
    fn list_all_flattens_every_session_sorted_by_name() {
        let d = tempfile::tempdir().unwrap();
        let mut one = session(
            SessionStatus::Connected,
            vec![obs("zeta", 1, AttachStatus::Attached)],
        );
        one.host = "gpu-01".to_string();
        let mut two = session(
            SessionStatus::Connected,
            vec![obs("alpha", 2, AttachStatus::Attached)],
        );
        two.host = "gpu-02".to_string();

        crate::session::store::save_state_in(d.path(), &one).unwrap();
        crate::session::store::save_state_in(d.path(), &two).unwrap();

        let names: Vec<String> = ForwardState::list_all_in(d.path())
            .unwrap()
            .into_iter()
            .map(|f| f.name)
            .collect();
        assert_eq!(names, vec!["alpha", "zeta"], "list_all must stay name-sorted");
    }
}

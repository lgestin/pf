use crate::watcher::RetryPolicy;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What the user asked for. Written only by the CLI and TUI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesiredSession {
    pub host: String,
    pub auto_reconnect: bool,
    pub retry: RetryPolicy,
    /// Kept sorted by name so diffs against observed state are stable.
    pub forwards: Vec<DesiredForward>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesiredForward {
    pub name: String,
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
}

impl DesiredSession {
    pub fn new(host: String, auto_reconnect: bool, retry: RetryPolicy) -> Self {
        Self {
            host,
            auto_reconnect,
            retry,
            forwards: Vec::new(),
        }
    }

    /// Insert or replace by name, keeping `forwards` sorted.
    pub fn upsert(&mut self, f: DesiredForward) {
        self.forwards.retain(|x| x.name != f.name);
        self.forwards.push(f);
        self.forwards.sort_by(|a, b| a.name.cmp(&b.name));
    }

    /// Returns true if a forward was actually removed.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.forwards.len();
        self.forwards.retain(|x| x.name != name);
        self.forwards.len() != before
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    Connecting,
    Connected,
    Reconnecting,
    Failed,
}

/// Whether a forward is currently established on the live master.
///
/// Deliberately separate from `state::ForwardStatus`, which is frozen as the
/// `pf list --json` contract and `display.rs`'s color map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AttachStatus {
    Forwarded,
    Pending,
    Failed,
}

/// What is actually true. Written only by the watcher.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub host: String,
    pub watcher_pid: u32,
    pub master_pid: Option<u32>,
    pub status: SessionStatus,
    /// Watcher start. Survives reconnects.
    pub started_at: DateTime<Utc>,
    /// Current master. Resets on every reconnect.
    pub connected_at: Option<DateTime<Utc>>,
    pub reconnect_count: u32,
    /// Echoed from desired so the flat projection needs only this file.
    pub auto_reconnect: bool,
    pub retry: RetryPolicy,
    pub forwards: Vec<ForwardObs>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardObs {
    pub name: String,
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
    pub status: AttachStatus,
    pub attached_at: Option<DateTime<Utc>>,
    /// ssh's stderr when an attach failed.
    pub error: Option<String>,
}

impl SessionState {
    pub fn new(host: String, watcher_pid: u32, auto_reconnect: bool, retry: RetryPolicy) -> Self {
        Self {
            host,
            watcher_pid,
            master_pid: None,
            status: SessionStatus::Connecting,
            started_at: Utc::now(),
            connected_at: None,
            reconnect_count: 0,
            auto_reconnect,
            retry,
            forwards: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desired_session_round_trips_through_json() {
        let d = DesiredSession {
            host: "gpu-01".to_string(),
            auto_reconnect: true,
            retry: crate::watcher::RetryPolicy::default(),
            forwards: vec![DesiredForward {
                name: "jupyter".to_string(),
                local_port: 8888,
                remote_host: "localhost".to_string(),
                remote_port: 8888,
            }],
        };

        let json = serde_json::to_string(&d).unwrap();
        let back: DesiredSession = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn session_state_round_trips_through_json() {
        let s = SessionState {
            host: "gpu-01".to_string(),
            watcher_pid: 42,
            master_pid: Some(43),
            status: SessionStatus::Connected,
            started_at: chrono::Utc::now(),
            connected_at: None,
            reconnect_count: 3,
            auto_reconnect: true,
            retry: crate::watcher::RetryPolicy::default(),
            forwards: vec![ForwardObs {
                name: "jupyter".to_string(),
                local_port: 8888,
                remote_host: "localhost".to_string(),
                remote_port: 8888,
                status: AttachStatus::Failed,
                attached_at: None,
                error: Some("bind: Address already in use".to_string()),
            }],
        };

        let json = serde_json::to_string(&s).unwrap();
        let back: SessionState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.host, s.host);
        assert_eq!(back.status, s.status);
        assert_eq!(back.forwards[0].status, AttachStatus::Failed);
        assert_eq!(
            back.forwards[0].error.as_deref(),
            Some("bind: Address already in use")
        );
    }

    #[test]
    fn upsert_replaces_by_name_and_keeps_the_list_sorted() {
        let mut s = DesiredSession::new("gpu-01".to_string(), true, Default::default());
        for (name, port) in [("zeta", 3u16), ("alpha", 1), ("mid", 2)] {
            s.upsert(DesiredForward {
                name: name.to_string(),
                local_port: port,
                remote_host: "localhost".to_string(),
                remote_port: port,
            });
        }
        assert_eq!(
            s.forwards.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "mid", "zeta"]
        );

        // Re-upserting a name replaces rather than duplicating.
        s.upsert(DesiredForward {
            name: "mid".to_string(),
            local_port: 99,
            remote_host: "localhost".to_string(),
            remote_port: 99,
        });
        assert_eq!(s.forwards.len(), 3);
        assert_eq!(s.forwards[1].local_port, 99);

        assert!(s.remove("mid"));
        assert!(!s.remove("mid"), "removing twice should report no change");
        assert_eq!(s.forwards.len(), 2);
    }
}

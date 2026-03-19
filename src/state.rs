use crate::error::Result;
use crate::paths;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
}

impl ForwardState {
    pub fn save(&self) -> Result<()> {
        let path = paths::state_file(&self.name)?;
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    pub fn load(name: &str) -> Result<Self> {
        let path = paths::state_file(name)?;
        if !path.exists() {
            return Err(crate::error::PfError::NotFound(name.to_string()));
        }
        let json = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&json)?)
    }

    pub fn remove(name: &str) -> Result<()> {
        let path = paths::state_file(name)?;
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    pub fn list_all() -> Result<Vec<Self>> {
        let run_dir = paths::run_dir()?;
        if !run_dir.exists() {
            return Ok(vec![]);
        }
        let mut states = Vec::new();
        for entry in std::fs::read_dir(run_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                match std::fs::read_to_string(&path) {
                    Ok(json) => {
                        if let Ok(state) = serde_json::from_str::<ForwardState>(&json) {
                            states.push(state);
                        }
                    }
                    Err(_) => continue,
                }
            }
        }
        states.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(states)
    }
}

use crate::error::{PfError, Result};
use crate::paths;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub host: String,
    pub local_port: u16,
    pub remote_port: u16,
    #[serde(default = "default_remote_host")]
    pub remote_host: String,
}

fn default_remote_host() -> String {
    "localhost".to_string()
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = paths::config_path()?;
        if !path.exists() {
            return Ok(Config::default());
        }
        let content = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
    }

    pub fn save(&self) -> Result<()> {
        let path = paths::config_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    pub fn add_profile(&mut self, name: String, profile: Profile) -> Result<()> {
        if self.profiles.contains_key(&name) {
            return Err(PfError::ProfileExists(name));
        }
        self.profiles.insert(name, profile);
        self.save()
    }

    pub fn remove_profile(&mut self, name: &str) -> Result<()> {
        if self.profiles.remove(name).is_none() {
            return Err(PfError::ProfileNotFound(name.to_string()));
        }
        self.save()
    }

    pub fn get_profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.get(name)
    }
}

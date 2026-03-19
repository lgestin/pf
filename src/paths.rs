use crate::error::{PfError, Result};
use std::path::PathBuf;

pub fn base_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| PfError::Other("Cannot find home directory".into()))?;
    Ok(home.join(".pf"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(base_dir()?.join("config.toml"))
}

pub fn run_dir() -> Result<PathBuf> {
    Ok(base_dir()?.join("run"))
}

pub fn log_dir() -> Result<PathBuf> {
    Ok(base_dir()?.join("logs"))
}

pub fn state_file(name: &str) -> Result<PathBuf> {
    Ok(run_dir()?.join(format!("{name}.json")))
}

pub fn log_file(name: &str) -> Result<PathBuf> {
    Ok(log_dir()?.join(format!("{name}.log")))
}

pub fn ensure_dirs() -> Result<()> {
    std::fs::create_dir_all(run_dir()?)?;
    std::fs::create_dir_all(log_dir()?)?;
    Ok(())
}

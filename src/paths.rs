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

pub fn desired_file_in(run: &std::path::Path, host: &str) -> PathBuf {
    run.join(format!("{host}.desired.json"))
}

pub fn state_file_in(run: &std::path::Path, host: &str) -> PathBuf {
    run.join(format!("{host}.state.json"))
}

pub fn lock_file_in(run: &std::path::Path, host: &str) -> PathBuf {
    run.join(format!("{host}.lock"))
}

pub fn socket_file_in(run: &std::path::Path, host: &str) -> PathBuf {
    run.join(format!("{host}.sock"))
}

pub fn session_log_file(host: &str) -> Result<PathBuf> {
    Ok(log_dir()?.join(format!("{host}.log")))
}

pub fn ensure_dirs() -> Result<()> {
    std::fs::create_dir_all(run_dir()?)?;
    std::fs::create_dir_all(log_dir()?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_files_are_distinct_and_correctly_suffixed() {
        let run = std::path::Path::new("/tmp/pf-run");

        assert_eq!(desired_file_in(run, "gpu-01"), run.join("gpu-01.desired.json"));
        assert_eq!(state_file_in(run, "gpu-01"), run.join("gpu-01.state.json"));
        assert_eq!(lock_file_in(run, "gpu-01"), run.join("gpu-01.lock"));
        assert_eq!(socket_file_in(run, "gpu-01"), run.join("gpu-01.sock"));
    }

    #[test]
    fn a_legacy_state_file_cannot_collide_with_a_session_state_file() {
        // v0.1.5 wrote `<name>.json`; sessions write `<host>.state.json`.
        // A forward literally named "gpu-01.state" must not shadow the session.
        let run = std::path::Path::new("/tmp/pf-run");
        assert_ne!(
            state_file_in(run, "gpu-01"),
            run.join("gpu-01.json"),
            "session state must not use the legacy filename"
        );
    }
}

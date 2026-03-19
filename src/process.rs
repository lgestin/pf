use crate::error::{PfError, Result};
use crate::state::{ForwardState, ForwardStatus};
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;

pub fn is_alive(pid: u32) -> bool {
    signal::kill(Pid::from_raw(pid as i32), None).is_ok()
}

pub fn kill_process(pid: u32) -> Result<()> {
    let pid = Pid::from_raw(pid as i32);
    signal::kill(pid, Signal::SIGTERM).map_err(|e| PfError::Other(format!("Failed to kill PID {}: {}", pid, e)))?;
    Ok(())
}

pub fn stop_forward(name: &str) -> Result<()> {
    let state = ForwardState::load(name)?;

    if is_alive(state.watcher_pid) {
        kill_process(state.watcher_pid)?;
        // Give the watcher time to clean up
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    // If SSH is still alive (watcher didn't clean up), kill it directly
    if let Some(ssh_pid) = state.ssh_pid {
        if is_alive(ssh_pid) {
            let _ = kill_process(ssh_pid);
        }
    }

    ForwardState::remove(name)?;
    Ok(())
}

pub fn is_port_in_use(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_err()
}

pub fn check_name_available(name: &str) -> Result<()> {
    let state = ForwardState::load(name);
    if let Ok(state) = state {
        if state.status != ForwardStatus::Failed && state.status != ForwardStatus::Stopped && is_alive(state.watcher_pid) {
            return Err(PfError::AlreadyRunning(name.to_string()));
        }
        // Stale state file, clean it up
        ForwardState::remove(name)?;
    }
    Ok(())
}

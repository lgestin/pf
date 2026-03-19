use crate::error::Result;
use crate::paths;
use crate::process;
use crate::state::{ForwardState, ForwardStatus};
use crate::tunnel::TunnelParams;
use chrono::Utc;
use std::fs::OpenOptions;
use std::os::unix::process::CommandExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Spawn a watcher daemon by re-execing `pf watcher ...` as a detached process.
pub fn spawn_watcher(
    name: &str,
    host: &str,
    local_port: u16,
    remote_port: u16,
    remote_host: &str,
    reconnect: bool,
    max_retries: u32,
    retry_delay: u64,
) -> Result<u32> {
    process::check_name_available(name)?;

    if process::is_port_in_use(local_port) {
        return Err(crate::error::PfError::PortInUse(local_port));
    }

    paths::ensure_dirs()?;

    let exe = std::env::current_exe().map_err(|e| {
        crate::error::PfError::Other(format!("Cannot find own executable: {e}"))
    })?;

    let log_path = paths::log_file(name)?;

    let mut cmd = std::process::Command::new(exe);
    cmd.args([
        "watcher",
        "--name",
        name,
        "--host",
        host,
        "--local-port",
        &local_port.to_string(),
        "--remote-port",
        &remote_port.to_string(),
        "--remote-host",
        remote_host,
        "--max-retries",
        &max_retries.to_string(),
        "--retry-delay",
        &retry_delay.to_string(),
    ]);
    if reconnect {
        cmd.arg("--reconnect");
    }

    // Detach the watcher: redirect stdio to log, use setsid via pre_exec
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let log_err = log_file.try_clone()?;

    cmd.stdin(std::process::Stdio::null())
        .stdout(log_file)
        .stderr(log_err);

    // Use setsid to fully detach on unix
    unsafe {
        cmd.pre_exec(|| {
            nix::unistd::setsid().map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            Ok(())
        });
    }

    let child = cmd.spawn()?;
    let pid = child.id();

    // Give the watcher a moment to write its state file
    std::thread::sleep(std::time::Duration::from_millis(300));

    Ok(pid)
}

/// The actual watcher daemon entry point (called via `pf watcher`).
pub fn run_watcher(
    name: String,
    host: String,
    local_port: u16,
    remote_port: u16,
    remote_host: String,
    reconnect: bool,
    max_retries: u32,
    retry_delay: u64,
) {
    // Set up signal handling for graceful shutdown
    let term = Arc::new(AtomicBool::new(false));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&term));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&term));

    let watcher_pid = std::process::id();

    // Write initial state
    let mut state = ForwardState {
        name: name.clone(),
        host: host.clone(),
        local_port,
        remote_port,
        remote_host: remote_host.clone(),
        watcher_pid,
        ssh_pid: None,
        status: ForwardStatus::Running,
        started_at: Utc::now(),
        reconnect_count: 0,
        auto_reconnect: reconnect,
        max_retries,
        retry_delay,
    };
    if let Err(e) = state.save() {
        eprintln!("[pf watcher] Failed to save state: {e}");
        return;
    }

    let params = TunnelParams {
        host: host.clone(),
        local_port,
        remote_port,
        remote_host: remote_host.clone(),
    };

    let mut retries = 0u32;

    loop {
        if term.load(Ordering::Relaxed) {
            // Received shutdown signal
            eprintln!("[{}] Received shutdown signal", name);
            break;
        }

        eprintln!("[{}] Starting SSH tunnel ({}:{} via {})", name, local_port, remote_port, host);

        let log_path = match paths::log_file(&name) {
            Ok(p) => p,
            Err(_) => break,
        };
        let log_file = match OpenOptions::new().create(true).append(true).open(&log_path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[{}] Failed to open log: {e}", name);
                break;
            }
        };

        let mut child = match params.spawn(log_file) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[{}] Failed to spawn SSH: {e}", name);
                state.status = ForwardStatus::Failed;
                let _ = state.save();
                break;
            }
        };

        let ssh_pid = child.id();
        state.ssh_pid = Some(ssh_pid);
        state.status = ForwardStatus::Running;
        let _ = state.save();

        eprintln!("[{}] SSH tunnel started (pid {})", name, ssh_pid);

        // Wait for SSH to exit, checking for shutdown signal periodically
        loop {
            if term.load(Ordering::Relaxed) {
                eprintln!("[{}] Shutting down SSH (pid {})", name, ssh_pid);
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            match child.try_wait() {
                Ok(Some(exit)) => {
                    eprintln!("[{}] SSH exited with {}", name, exit);
                    break;
                }
                Ok(None) => {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                Err(e) => {
                    eprintln!("[{}] Error waiting for SSH: {e}", name);
                    break;
                }
            }
        }

        if term.load(Ordering::Relaxed) {
            break;
        }

        // SSH died unexpectedly
        state.ssh_pid = None;

        if !reconnect {
            eprintln!("[{}] Auto-reconnect disabled, exiting", name);
            state.status = ForwardStatus::Failed;
            let _ = state.save();
            break;
        }

        retries += 1;
        if max_retries > 0 && retries > max_retries {
            eprintln!("[{}] Max retries ({}) exceeded, giving up", name, max_retries);
            state.status = ForwardStatus::Failed;
            let _ = state.save();
            break;
        }

        state.status = ForwardStatus::Reconnecting;
        state.reconnect_count += 1;
        let _ = state.save();

        eprintln!(
            "[{}] Reconnecting in {}s (attempt {}{})...",
            name,
            retry_delay,
            retries,
            if max_retries > 0 {
                format!("/{}", max_retries)
            } else {
                String::new()
            }
        );

        // Wait for retry_delay, but check for shutdown signal
        for _ in 0..(retry_delay * 10) {
            if term.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    // Clean up
    state.ssh_pid = None;
    state.status = ForwardStatus::Stopped;
    let _ = state.save();
    let _ = ForwardState::remove(&name);
    eprintln!("[{}] Watcher exiting", name);
}

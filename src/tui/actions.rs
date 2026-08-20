use crate::process;
use crate::watcher;

pub fn start_profile(
    name: &str,
    host: &str,
    local_port: u16,
    remote_port: u16,
) -> Result<String, String> {
    match process::start_forward(
        host,
        name,
        local_port,
        "localhost",
        remote_port,
        true,
        watcher::RetryPolicy::default(),
    ) {
        Ok(()) => Ok(format!("Started {name}")),
        Err(e) => Err(format!("Failed to start {name}: {e}")),
    }
}

pub fn start_adhoc(
    host: &str,
    local_port: u16,
    remote_port: u16,
    name: Option<&str>,
) -> Result<String, String> {
    let fwd_name = name.unwrap_or("").to_string();
    let fwd_name = if fwd_name.is_empty() {
        format!("{}-{}", host, local_port)
    } else {
        fwd_name
    };

    match process::start_forward(
        host,
        &fwd_name,
        local_port,
        "localhost",
        remote_port,
        true,
        watcher::RetryPolicy::default(),
    ) {
        Ok(()) => Ok(format!("Started {fwd_name}")),
        Err(e) => Err(format!("Failed to start {fwd_name}: {e}")),
    }
}

pub fn stop_forward(name: &str) -> Result<String, String> {
    match process::stop_forward(name) {
        Ok(()) => Ok(format!("Stopped {name}")),
        Err(e) => Err(format!("Failed to stop {name}: {e}")),
    }
}

/// Stop every forward on a host, taking its session down with them.
pub fn stop_host(host: &str) -> Result<String, String> {
    match process::stop_host(host) {
        Ok(()) => Ok(format!("Stopped all forwards on {host}")),
        Err(e) => Err(format!("Failed to stop {host}: {e}")),
    }
}

/// Drop the master and let the watcher reconnect it, re-attaching every
/// forward through the reconciler's normal path.
pub fn restart_host(host: &str) -> Result<String, String> {
    let state = match crate::session::store::load_state(host) {
        Ok(Some(s)) => s,
        Ok(None) => return Err(format!("{host} is not connected")),
        Err(e) => return Err(format!("Failed to read {host}: {e}")),
    };

    let Some(master_pid) = state.master_pid else {
        return Err(format!("{host} has no live master"));
    };

    // Killing the master is enough: the watcher notices, backs off, reconnects,
    // and reconciles the desired set back onto the new connection.
    match process::kill_process(master_pid) {
        Ok(()) => Ok(format!("Reconnecting {host}")),
        Err(e) => Err(format!("Failed to reconnect {host}: {e}")),
    }
}

pub fn restart_forward(name: &str) -> Result<String, String> {
    // Load state before stopping so we can restart with same params
    let state = match crate::state::ForwardState::load(name) {
        Ok(s) => s,
        Err(e) => return Err(format!("Failed to load state for {name}: {e}")),
    };

    if let Err(e) = process::stop_forward(name) {
        return Err(format!("Failed to stop {name}: {e}"));
    }

    // Brief pause for cleanup
    std::thread::sleep(std::time::Duration::from_millis(500));

    match process::start_forward(
        &state.host,
        &state.name,
        state.local_port,
        &state.remote_host,
        state.remote_port,
        state.auto_reconnect,
        watcher::RetryPolicy {
            max_retries: state.max_retries,
            initial_delay: state.retry_delay,
            max_delay: state.max_retry_delay,
        },
    ) {
        Ok(()) => Ok(format!("Restarted {name}")),
        Err(e) => Err(format!("Failed to restart {name}: {e}")),
    }
}

use crate::process;
use crate::watcher;

pub fn start_profile(name: &str, host: &str, local_port: u16, remote_port: u16) -> Result<String, String> {
    match watcher::spawn_watcher(
        name,
        host,
        local_port,
        remote_port,
        "localhost",
        true,
        0,
        5,
    ) {
        Ok(_pid) => Ok(format!("Started {name}")),
        Err(e) => Err(format!("Failed to start {name}: {e}")),
    }
}

pub fn start_adhoc(
    host: &str,
    local_port: u16,
    remote_port: u16,
    name: Option<&str>,
) -> Result<String, String> {
    let fwd_name = name.unwrap_or_else(|| "").to_string();
    let fwd_name = if fwd_name.is_empty() {
        format!("{}-{}", host, local_port)
    } else {
        fwd_name
    };

    match watcher::spawn_watcher(
        &fwd_name,
        host,
        local_port,
        remote_port,
        "localhost",
        true,
        0,
        5,
    ) {
        Ok(_pid) => Ok(format!("Started {fwd_name}")),
        Err(e) => Err(format!("Failed to start {fwd_name}: {e}")),
    }
}

pub fn stop_forward(name: &str) -> Result<String, String> {
    match process::stop_forward(name) {
        Ok(()) => Ok(format!("Stopped {name}")),
        Err(e) => Err(format!("Failed to stop {name}: {e}")),
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

    match watcher::spawn_watcher(
        &state.name,
        &state.host,
        state.local_port,
        state.remote_port,
        &state.remote_host,
        state.auto_reconnect,
        state.max_retries,
        state.retry_delay,
    ) {
        Ok(_pid) => Ok(format!("Restarted {name}")),
        Err(e) => Err(format!("Failed to restart {name}: {e}")),
    }
}

use crate::process;
use crate::state::{ForwardState, ForwardStatus};
use chrono::Utc;
use colored::Colorize;
use comfy_table::{Cell, Color, Table};

pub fn print_forwards_table(states: &[ForwardState]) {
    if states.is_empty() {
        println!("No active forwards.");
        return;
    }

    let mut table = Table::new();
    table.set_header(vec!["Name", "Host", "Ports", "Status", "Uptime", "Reconnects", "PIDs"]);

    for state in states {
        let alive = process::is_alive(state.watcher_pid);
        let effective_status = if alive {
            &state.status
        } else {
            &ForwardStatus::Failed
        };

        let status_color = match effective_status {
            ForwardStatus::Running => Color::Green,
            ForwardStatus::Reconnecting => Color::Yellow,
            ForwardStatus::Failed => Color::Red,
            ForwardStatus::Stopped => Color::Grey,
        };

        let uptime = if alive {
            let dur = Utc::now() - state.started_at;
            format_duration(dur.num_seconds())
        } else {
            "-".to_string()
        };

        let ports = format!("{}:{}", state.local_port, state.remote_port);
        let pids = if alive {
            match state.ssh_pid {
                Some(ssh) => format!("w:{} s:{}", state.watcher_pid, ssh),
                None => format!("w:{}", state.watcher_pid),
            }
        } else {
            "-".to_string()
        };

        table.add_row(vec![
            Cell::new(&state.name),
            Cell::new(&state.host),
            Cell::new(&ports),
            Cell::new(effective_status.to_string()).fg(status_color),
            Cell::new(&uptime),
            Cell::new(state.reconnect_count),
            Cell::new(&pids),
        ]);
    }

    println!("{table}");
}

pub fn print_forwards_json(states: &[ForwardState]) {
    let json = serde_json::to_string_pretty(&states).unwrap_or_else(|_| "[]".to_string());
    println!("{json}");
}

pub fn print_started(name: &str, host: &str, local_port: u16, remote_port: u16) {
    println!(
        "{} {} ({}:{} via {})",
        "[started]".green(),
        name,
        local_port,
        remote_port,
        host
    );
}

pub fn print_stopped(name: &str) {
    println!("{} {}", "[stopped]".yellow(), name);
}

fn format_duration(total_secs: i64) -> String {
    if total_secs < 0 {
        return "-".to_string();
    }
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;
    if hours > 0 {
        format!("{hours}h{mins:02}m")
    } else if mins > 0 {
        format!("{mins}m{secs:02}s")
    } else {
        format!("{secs}s")
    }
}

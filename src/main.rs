mod cli;
mod config;
mod display;
mod error;
mod paths;
mod process;
pub mod ssh_hosts;
mod state;
mod tunnel;
mod tui;
mod watcher;

use clap::{CommandFactory, Parser};
use cli::{Cli, Command, ConfigAction};
use colored::Colorize;
use config::{Config, Profile};
use error::{PfError, Result};
use state::ForwardState;

fn main() {
    let cli = Cli::parse();

    if let Err(e) = run(cli) {
        eprintln!("{} {e}", "error:".red().bold());
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    paths::ensure_dirs()?;

    match cli.command {
        Command::Start {
            name_or_host,
            ports,
            name,
            no_reconnect,
            max_retries,
            retry_delay,
        } => cmd_start(
            name_or_host,
            ports,
            name,
            !no_reconnect,
            max_retries,
            retry_delay,
        ),

        Command::Stop { name, all } => cmd_stop(name, all),
        Command::List { json } => cmd_list(json),
        Command::Restart { name, all } => cmd_restart(name, all),
        Command::Logs { name, follow } => cmd_logs(&name, follow),

        Command::Config { action } => match action {
            ConfigAction::Add { name, host, ports } => cmd_config_add(name, host, ports),
            ConfigAction::Remove { name } => cmd_config_remove(name),
            ConfigAction::List => cmd_config_list(),
        },

        Command::Clean => cmd_clean(),
        Command::Tui => tui::run(),
        Command::Hosts => cmd_hosts(),
        Command::Completions { shell } => cmd_completions(shell),
        Command::Complete { subcommand, prefix } => cmd_complete(&subcommand, &prefix),

        Command::Watcher {
            name,
            host,
            local_port,
            remote_port,
            remote_host,
            reconnect,
            max_retries,
            retry_delay,
        } => {
            watcher::run_watcher(
                name,
                host,
                local_port,
                remote_port,
                remote_host.unwrap_or_else(|| "localhost".to_string()),
                reconnect,
                max_retries,
                retry_delay,
            );
            Ok(())
        }
    }
}

fn parse_ports(ports: &str) -> Result<(u16, u16)> {
    let parts: Vec<&str> = ports.split(':').collect();
    if parts.len() != 2 {
        return Err(PfError::InvalidPortMapping(ports.to_string()));
    }
    let local: u16 = parts[0]
        .parse()
        .map_err(|_| PfError::InvalidPortMapping(ports.to_string()))?;
    let remote: u16 = parts[1]
        .parse()
        .map_err(|_| PfError::InvalidPortMapping(ports.to_string()))?;
    Ok((local, remote))
}

fn cmd_start(
    name_or_host: String,
    ports: Option<String>,
    name: Option<String>,
    reconnect: bool,
    max_retries: u32,
    retry_delay: u64,
) -> Result<()> {
    // Check if name_or_host matches a saved profile
    let config = Config::load()?;

    if let Some(profile) = config.get_profile(&name_or_host) {
        // Profile-based start — ports arg is ignored if profile provides them
        let (local_port, remote_port) = if let Some(ref p) = ports {
            parse_ports(p)?
        } else {
            (profile.local_port, profile.remote_port)
        };
        let host = profile.host.clone();
        let remote_host = profile.remote_host.clone();
        let fwd_name = name_or_host;

        watcher::spawn_watcher(
            &fwd_name,
            &host,
            local_port,
            remote_port,
            &remote_host,
            reconnect,
            max_retries,
            retry_delay,
        )?;

        display::print_started(&fwd_name, &host, local_port, remote_port);
        Ok(())
    } else {
        // Ad-hoc start: name_or_host is the SSH host, ports is required
        let ports_str = ports.ok_or_else(|| {
            PfError::Other(format!(
                "No profile named '{}' found and no ports specified.\n\
                 Usage: pf start <HOST> <LOCAL:REMOTE> or pf start <PROFILE>",
                name_or_host
            ))
        })?;
        let (local_port, remote_port) = parse_ports(&ports_str)?;

        let fwd_name = name.unwrap_or_else(|| format!("{}-{}", name_or_host, local_port));

        watcher::spawn_watcher(
            &fwd_name,
            &name_or_host,
            local_port,
            remote_port,
            "localhost",
            reconnect,
            max_retries,
            retry_delay,
        )?;

        display::print_started(&fwd_name, &name_or_host, local_port, remote_port);
        Ok(())
    }
}

fn cmd_stop(name: Option<String>, all: bool) -> Result<()> {
    if all {
        let states = ForwardState::list_all()?;
        if states.is_empty() {
            println!("No active forwards.");
            return Ok(());
        }
        for state in &states {
            match process::stop_forward(&state.name) {
                Ok(()) => display::print_stopped(&state.name),
                Err(e) => eprintln!("{} {}: {e}", "error:".red().bold(), state.name),
            }
        }
        return Ok(());
    }

    let name = name.ok_or_else(|| PfError::Other("Name required (or use --all)".into()))?;
    process::stop_forward(&name)?;
    display::print_stopped(&name);
    Ok(())
}

fn cmd_list(json: bool) -> Result<()> {
    let states = ForwardState::list_all()?;
    if json {
        display::print_forwards_json(&states);
    } else {
        display::print_forwards_table(&states);
    }
    Ok(())
}

fn cmd_restart(name: Option<String>, all: bool) -> Result<()> {
    if all {
        let states = ForwardState::list_all()?;
        if states.is_empty() {
            println!("No active forwards.");
            return Ok(());
        }
        for s in &states {
            restart_one(&s.name)?;
        }
        return Ok(());
    }

    let name = name.ok_or_else(|| PfError::Other("Name required (or use --all)".into()))?;
    restart_one(&name)
}

fn restart_one(name: &str) -> Result<()> {
    let state = ForwardState::load(name)?;
    process::stop_forward(name)?;
    std::thread::sleep(std::time::Duration::from_millis(500));
    watcher::spawn_watcher(
        &state.name,
        &state.host,
        state.local_port,
        state.remote_port,
        &state.remote_host,
        state.auto_reconnect,
        state.max_retries,
        state.retry_delay,
    )?;
    display::print_started(&state.name, &state.host, state.local_port, state.remote_port);
    Ok(())
}

fn cmd_logs(name: &str, follow: bool) -> Result<()> {
    let log_path = paths::log_file(name)?;
    if !log_path.exists() {
        return Err(PfError::NotFound(format!("No logs for '{name}'")));
    }

    if follow {
        // Use tail -f via a child process
        let status = std::process::Command::new("tail")
            .args(["-f", log_path.to_str().unwrap()])
            .status()?;
        if !status.success() {
            return Err(PfError::Other("tail exited with error".into()));
        }
    } else {
        let content = std::fs::read_to_string(&log_path)?;
        print!("{content}");
    }

    Ok(())
}

fn cmd_config_add(name: String, host: String, ports: String) -> Result<()> {
    let (local_port, remote_port) = parse_ports(&ports)?;
    let mut config = Config::load()?;
    config.add_profile(
        name.clone(),
        Profile {
            host: host.clone(),
            local_port,
            remote_port,
            remote_host: "localhost".to_string(),
        },
    )?;
    println!(
        "{} profile '{}' ({}:{}:{} via {})",
        "[saved]".green(),
        name,
        local_port,
        "localhost",
        remote_port,
        host
    );
    Ok(())
}

fn cmd_config_remove(name: String) -> Result<()> {
    let mut config = Config::load()?;
    config.remove_profile(&name)?;
    println!("{} profile '{}'", "[removed]".yellow(), name);
    Ok(())
}

fn cmd_config_list() -> Result<()> {
    let config = Config::load()?;
    if config.profiles.is_empty() {
        println!("No saved profiles.");
        return Ok(());
    }

    let mut table = comfy_table::Table::new();
    table.set_header(vec!["Name", "Host", "Local Port", "Remote Port"]);
    for (name, profile) in &config.profiles {
        table.add_row(vec![
            name.as_str(),
            &profile.host,
            &profile.local_port.to_string(),
            &profile.remote_port.to_string(),
        ]);
    }
    println!("{table}");
    Ok(())
}

fn cmd_clean() -> Result<()> {
    let states = ForwardState::list_all()?;
    let mut cleaned = 0;
    for state in &states {
        if !process::is_alive(state.watcher_pid) {
            ForwardState::remove(&state.name)?;
            println!("Cleaned stale state for '{}'", state.name);
            cleaned += 1;
        }
    }
    if cleaned == 0 {
        println!("No stale state files found.");
    } else {
        println!("Cleaned {cleaned} stale state file(s).");
    }
    Ok(())
}

fn cmd_hosts() -> Result<()> {
    let hosts = ssh_hosts::parse_ssh_hosts();
    if hosts.is_empty() {
        println!("No hosts found in SSH config.");
    } else {
        for host in &hosts {
            println!("{host}");
        }
    }
    Ok(())
}

fn cmd_completions(shell: cli::ShellType) -> Result<()> {
    let mut cmd = Cli::command();
    let shell = match shell {
        cli::ShellType::Bash => clap_complete::Shell::Bash,
        cli::ShellType::Zsh => clap_complete::Shell::Zsh,
        cli::ShellType::Fish => clap_complete::Shell::Fish,
    };
    clap_complete::generate(shell, &mut cmd, "pf", &mut std::io::stdout());

    // Print a note about dynamic completions
    eprintln!();
    eprintln!("# For dynamic SSH host + profile completion, also add:");
    eprintln!("# eval \"$(pf completions {})\"", match shell {
        clap_complete::Shell::Bash => "bash",
        clap_complete::Shell::Zsh => "zsh",
        clap_complete::Shell::Fish => "fish",
        _ => "bash",
    });
    eprintln!("# Then source the custom completion wrapper below:");
    eprintln!();
    match shell {
        clap_complete::Shell::Zsh => {
            println!();
            println!("# Dynamic completions for pf start (SSH hosts + profiles)");
            println!("# Add this after the generated completions above:");
            println!("_pf_start_complete() {{");
            println!("  local -a completions");
            println!("  completions=(${{(@f)$(pf complete --subcommand start --prefix \"$words[CURRENT]\" 2>/dev/null)}})");
            println!("  compadd -a completions");
            println!("}}");
        }
        clap_complete::Shell::Bash => {
            println!();
            println!("# Dynamic completions for pf start (SSH hosts + profiles)");
            println!("_pf_dynamic() {{");
            println!("  local cur=${{COMP_WORDS[COMP_CWORD]}}");
            println!("  local sub=${{COMP_WORDS[1]}}");
            println!("  if [[ \"$sub\" == \"start\" || \"$sub\" == \"stop\" || \"$sub\" == \"restart\" || \"$sub\" == \"logs\" ]]; then");
            println!("    COMPREPLY=($(pf complete --subcommand \"$sub\" --prefix \"$cur\" 2>/dev/null))");
            println!("  fi");
            println!("}}");
            println!("complete -F _pf_dynamic pf");
        }
        clap_complete::Shell::Fish => {
            println!();
            println!("# Dynamic completions for pf start (SSH hosts + profiles)");
            println!("complete -c pf -n '__fish_seen_subcommand_from start' -f -a '(pf complete --subcommand start 2>/dev/null)'");
            println!("complete -c pf -n '__fish_seen_subcommand_from stop restart logs' -f -a '(pf complete --subcommand stop 2>/dev/null)'");
        }
        _ => {}
    }
    Ok(())
}

fn cmd_complete(subcommand: &str, prefix: &str) -> Result<()> {
    match subcommand {
        "start" => {
            // Complete with SSH hosts + profile names
            let hosts = ssh_hosts::hosts_matching(prefix);
            let config = Config::load().unwrap_or_default();
            let profiles: Vec<String> = config
                .profiles
                .keys()
                .filter(|k| k.starts_with(prefix))
                .cloned()
                .collect();

            // Print profiles first (they're named shortcuts), then SSH hosts
            for p in &profiles {
                println!("{p}");
            }
            for h in &hosts {
                // Skip if already listed as a profile
                if !profiles.contains(h) {
                    println!("{h}");
                }
            }
        }
        "stop" | "restart" | "logs" => {
            // Complete with running forward names
            let states = ForwardState::list_all().unwrap_or_default();
            for state in &states {
                if state.name.starts_with(prefix) {
                    println!("{}", state.name);
                }
            }
        }
        _ => {}
    }
    Ok(())
}

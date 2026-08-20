pub mod actions;
pub mod state;
pub mod tree;
pub mod ui;

use crate::error::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use state::{AppState, ConfirmAction, Mode};
use std::io;
use std::time::{Duration, Instant};
use tree::Sel;

pub fn run() -> Result<()> {
    // Setup terminal
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = AppState::new();
    app.refresh_profiles();
    app.refresh();

    let tick_rate = Duration::from_secs(1);
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| ui::render(f, &mut app))?;

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    break;
                }
                if app.mode != Mode::Filter {
                    app.status_message = None;
                }
                handle_key(&mut app, key.code);
            }
        }

        if last_tick.elapsed() >= tick_rate {
            // Profiles feed the machine list in `configured` mode, so pick up
            // ones added from the CLI while the TUI is open.
            app.refresh_profiles();
            app.refresh();
            if app.mode == Mode::Logs {
                let name = app.log_name.clone();
                app.load_logs(&name);
            }
            last_tick = Instant::now();
        }

        if app.should_quit {
            break;
        }
    }

    // Restore terminal
    terminal::disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

fn handle_key(app: &mut AppState, key: KeyCode) {
    match &app.mode {
        Mode::Normal => handle_normal_key(app, key),
        Mode::Logs => handle_logs_key(app, key),
        Mode::NewForward => handle_new_forward_key(app, key),
        Mode::ProfilePicker => handle_profile_picker_key(app, key),
        Mode::Filter => handle_filter_key(app, key),
        Mode::Confirm(_) => handle_confirm_key(app, key),
    }
}

fn handle_normal_key(app: &mut AppState, key: KeyCode) {
    match key {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('j') | KeyCode::Down => app.select_next(),
        KeyCode::Char('k') | KeyCode::Up => app.select_prev(),

        // Folding
        KeyCode::Enter | KeyCode::Char(' ') => app.toggle_expand(),
        KeyCode::Right => app.expand_selected(),
        KeyCode::Left => app.collapse_selected(),
        KeyCode::Char('Z') => app.collapse_all(),

        KeyCode::Char('/') => {
            app.filter.clear();
            app.mode = Mode::Filter;
        }
        // The filter survives leaving Filter mode; Esc is the way back out.
        KeyCode::Esc => {
            if !app.filter.is_empty() {
                app.filter.clear();
                app.refresh();
            }
        }
        KeyCode::Char('m') => app.cycle_machine_source(),

        // Add a forward under the selected machine. The host is already known,
        // which is why the form has no Host field.
        KeyCode::Char('a') | KeyCode::Char('n') => {
            if let Some(sel) = app.selected_sel() {
                app.open_new_forward_form(sel.host().to_string());
            }
        }

        // Connect to a machine the list does not contain — an IP, a user@host,
        // or anything hidden behind a wildcard in ~/.ssh/config.
        KeyCode::Char('A') | KeyCode::Char('N') => app.open_new_machine_form(),

        KeyCode::Char('x') | KeyCode::Char('d') => match app.selected_sel() {
            Some(Sel::Forward(_, name)) => {
                app.mode = Mode::Confirm(ConfirmAction::StopForward(name));
            }
            Some(Sel::Machine(host)) => {
                let count = app
                    .selected_machine()
                    .map(|m| m.forwards.len())
                    .unwrap_or(0);
                if count > 0 {
                    app.mode = Mode::Confirm(ConfirmAction::StopHost(host, count));
                } else {
                    app.status_message = Some(format!("{host} has no forwards"));
                }
            }
            None => {}
        },

        KeyCode::Char('r') => match app.selected_sel() {
            Some(Sel::Forward(_, name)) => {
                app.mode = Mode::Confirm(ConfirmAction::RestartForward(name));
            }
            Some(Sel::Machine(host)) => {
                if app.selected_machine().is_some_and(|m| m.is_live()) {
                    app.mode = Mode::Confirm(ConfirmAction::RestartHost(host));
                } else {
                    app.status_message = Some(format!("{host} is not connected"));
                }
            }
            None => {}
        },

        // One log per machine, so either row kind opens the same one.
        KeyCode::Char('l') => {
            if let Some(sel) = app.selected_sel() {
                let host = sel.host().to_string();
                app.load_logs(&host);
                app.mode = Mode::Logs;
            }
        }

        KeyCode::Char('s') => {
            app.refresh_profiles();
            if app.profiles.is_empty() {
                app.status_message = Some("No saved profiles".to_string());
            } else {
                app.select_profile(0);
                app.mode = Mode::ProfilePicker;
            }
        }

        KeyCode::Char('o') => {
            if let Some(Sel::Forward(host, name)) = app.selected_sel() {
                if let Some(machine) = app.machines.iter().find(|m| m.host == host) {
                    if let Some(f) = machine.forwards.iter().find(|f| f.name == name) {
                        let url = format!("http://localhost:{}", f.local_port);
                        let cmd = if cfg!(target_os = "macos") {
                            "open"
                        } else {
                            "xdg-open"
                        };
                        let _ = std::process::Command::new(cmd).arg(&url).spawn();
                        app.status_message = Some(format!("Opened {url}"));
                    }
                }
            }
        }
        _ => {}
    }
}

fn handle_filter_key(app: &mut AppState, key: KeyCode) {
    match key {
        KeyCode::Esc => {
            app.filter.clear();
            app.mode = Mode::Normal;
            app.refresh();
        }
        KeyCode::Enter => {
            app.mode = Mode::Normal;
        }
        KeyCode::Backspace => {
            app.filter.pop();
            app.refresh();
        }
        KeyCode::Char(c) => {
            app.filter.push(c);
            app.refresh();
        }
        _ => {}
    }
}

fn handle_logs_key(app: &mut AppState, key: KeyCode) {
    match key {
        KeyCode::Esc => app.mode = Mode::Normal,
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('j') | KeyCode::Down => {
            app.log_scroll = (app.log_scroll + 1).min(app.log_lines.len().saturating_sub(1));
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.log_scroll = app.log_scroll.saturating_sub(1);
        }
        _ => {}
    }
}

fn handle_new_forward_key(app: &mut AppState, key: KeyCode) {
    match key {
        KeyCode::Esc => app.mode = Mode::Normal,
        KeyCode::Tab => app.next_field(),
        KeyCode::BackTab => app.prev_field(),
        KeyCode::Backspace => {
            app.current_input().pop();
        }
        KeyCode::Char(c) => {
            app.current_input().push(c);
        }
        KeyCode::Enter => {
            if !app.on_last_field() {
                app.next_field();
                return;
            }
            submit_new_forward(app);
        }
        _ => {}
    }
}

fn submit_new_forward(app: &mut AppState) {
    let host = app.input_host.trim().to_string();
    if host.is_empty() {
        app.status_message = Some("Host is required".to_string());
        app.input_field = state::InputField::Host;
        return;
    }

    let local: u16 = match app.input_local_port.trim().parse() {
        Ok(p) => p,
        Err(_) => {
            app.status_message = Some("Local port must be a number".to_string());
            return;
        }
    };
    // Same port on both sides is the common case, so an empty remote port
    // mirrors the local one rather than being an error.
    let remote_raw = app.input_remote_port.trim();
    let remote: u16 = if remote_raw.is_empty() {
        local
    } else {
        match remote_raw.parse() {
            Ok(p) => p,
            Err(_) => {
                app.status_message = Some("Remote port must be a number".to_string());
                return;
            }
        }
    };

    let name = app.input_name.trim().to_string();
    let name = if name.is_empty() {
        format!("{host}-{local}")
    } else {
        name
    };

    match actions::start_adhoc(&host, local, remote, Some(&name)) {
        Ok(msg) => {
            app.status_message = Some(msg);
            app.mode = Mode::Normal;
            app.refresh();
            app.select_forward(&host, &name);
        }
        Err(msg) => app.status_message = Some(msg),
    }
}

fn handle_profile_picker_key(app: &mut AppState, key: KeyCode) {
    match key {
        KeyCode::Esc => app.mode = Mode::Normal,
        KeyCode::Char('j') | KeyCode::Down => app.select_next_profile(),
        KeyCode::Char('k') | KeyCode::Up => app.select_prev_profile(),
        KeyCode::Enter => {
            if let Some((name, profile)) = app.profiles.get(app.profile_selected()) {
                let name = name.clone();
                let host = profile.host.clone();
                let lp = profile.local_port;
                let rp = profile.remote_port;
                match actions::start_profile(&name, &host, lp, rp) {
                    Ok(msg) => app.status_message = Some(msg),
                    Err(msg) => app.status_message = Some(msg),
                }
                app.mode = Mode::Normal;
                app.refresh();
                // A profile targets its own host, which may not be the machine
                // the cursor was on, so follow the forward we just made.
                app.select_forward(&host, &name);
            }
        }
        _ => {}
    }
}

fn handle_confirm_key(app: &mut AppState, key: KeyCode) {
    match key {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            let Mode::Confirm(action) = app.mode.clone() else {
                return;
            };
            let msg = match action {
                ConfirmAction::StopForward(name) => actions::stop_forward(&name),
                ConfirmAction::StopHost(host, _) => actions::stop_host(&host),
                ConfirmAction::RestartForward(name) => actions::restart_forward(&name),
                ConfirmAction::RestartHost(host) => actions::restart_host(&host),
            };
            app.status_message = Some(match msg {
                Ok(m) => m,
                Err(m) => m,
            });
            app.mode = Mode::Normal;
            app.refresh();
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            app.mode = Mode::Normal;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use tree::MachineListMode;

    /// A tree of idle hosts, bypassing `refresh()` so nothing on-disk leaks in.
    fn app_with_hosts(hosts: &[&str]) -> AppState {
        let mut app = AppState::new();
        app.ssh_hosts = hosts.iter().map(|h| h.to_string()).collect();
        app.machine_source = MachineListMode::AllHosts;
        app.machines = tree::build_machines(
            Vec::new(),
            &BTreeSet::new(),
            &app.ssh_hosts,
            MachineListMode::AllHosts,
            "",
        );
        app.rows = tree::flatten(&app.machines, &app.expanded);
        app.table_state.select(Some(0));
        app
    }

    #[test]
    fn esc_in_normal_mode_clears_an_applied_filter() {
        // The empty state promises "Esc clears the filter" — that has to hold
        // after Enter applied it and returned to Normal mode, not only while
        // still typing in the filter prompt.
        let mut app = app_with_hosts(&["gpu-01", "nas"]);
        app.filter = "gpu".to_string();

        handle_key(&mut app, KeyCode::Esc);

        assert!(app.filter.is_empty(), "Esc should clear the applied filter");
        assert_eq!(app.mode, Mode::Normal);
    }
}

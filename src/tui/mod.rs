pub mod actions;
pub mod state;
pub mod ui;

use crate::error::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use state::{AppState, ConfirmAction, Mode};
use std::io;
use std::time::{Duration, Instant};

pub fn run() -> Result<()> {
    // Setup terminal
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = AppState::new();
    app.refresh_forwards();

    let tick_rate = Duration::from_secs(1);
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| ui::render(f, &app))?;

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                app.status_message = None;
                handle_key(&mut app, key.code);
            }
        }

        if last_tick.elapsed() >= tick_rate {
            app.refresh_forwards();
            // Refresh logs if viewing
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
        Mode::Confirm(_) => handle_confirm_key(app, key),
    }
}

fn handle_normal_key(app: &mut AppState, key: KeyCode) {
    match key {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('j') | KeyCode::Down => {
            if !app.forwards.is_empty() {
                app.selected = (app.selected + 1) % app.forwards.len();
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if !app.forwards.is_empty() {
                app.selected = app.selected.checked_sub(1).unwrap_or(app.forwards.len() - 1);
            }
        }
        KeyCode::Char('s') => {
            app.refresh_profiles();
            if app.profiles.is_empty() {
                app.status_message = Some("No saved profiles. Use 'pf config add' first.".to_string());
            } else {
                app.profile_selected = 0;
                app.mode = Mode::ProfilePicker;
            }
        }
        KeyCode::Char('n') => {
            app.clear_input_form();
            app.mode = Mode::NewForward;
        }
        KeyCode::Char('x') | KeyCode::Char('d') => {
            if let Some(name) = app.selected_name() {
                app.mode = Mode::Confirm(ConfirmAction::Stop(name));
            }
        }
        KeyCode::Char('r') => {
            if let Some(name) = app.selected_name() {
                app.mode = Mode::Confirm(ConfirmAction::Restart(name));
            }
        }
        KeyCode::Enter | KeyCode::Char('l') => {
            if let Some(name) = app.selected_name() {
                app.load_logs(&name);
                app.mode = Mode::Logs;
            }
        }
        _ => {}
    }
}

fn handle_logs_key(app: &mut AppState, key: KeyCode) {
    match key {
        KeyCode::Esc => app.mode = Mode::Normal,
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('j') | KeyCode::Down => {
            if app.log_scroll < app.log_lines.len().saturating_sub(1) {
                app.log_scroll += 1;
            }
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
        KeyCode::Tab => {
            if app.input_field == state::InputField::Host && !app.host_suggestions.is_empty() {
                // Cycle through SSH host suggestions
                app.cycle_host_suggestion();
            } else {
                app.input_field = app.input_field.next();
                if app.input_field == state::InputField::Host {
                    app.update_host_suggestions();
                }
            }
        }
        KeyCode::BackTab => {
            app.input_field = app.input_field.prev();
        }
        KeyCode::Backspace => {
            app.current_input().pop();
            if app.input_field == state::InputField::Host {
                app.update_host_suggestions();
            }
        }
        KeyCode::Char(c) => {
            app.current_input().push(c);
            if app.input_field == state::InputField::Host {
                app.update_host_suggestions();
            }
        }
        KeyCode::Enter => {
            let host = app.input_host.clone();
            let name = if app.input_name.is_empty() {
                None
            } else {
                Some(app.input_name.as_str())
            };
            let local_port: u16 = match app.input_local_port.parse() {
                Ok(p) => p,
                Err(_) => {
                    app.status_message = Some("Invalid local port".to_string());
                    return;
                }
            };
            let remote_port: u16 = match app.input_remote_port.parse() {
                Ok(p) => p,
                Err(_) => {
                    app.status_message = Some("Invalid remote port".to_string());
                    return;
                }
            };
            if host.is_empty() {
                app.status_message = Some("Host is required".to_string());
                return;
            }
            match actions::start_adhoc(&host, local_port, remote_port, name) {
                Ok(msg) => app.status_message = Some(msg),
                Err(msg) => app.status_message = Some(msg),
            }
            app.mode = Mode::Normal;
            app.refresh_forwards();
        }
        _ => {}
    }
}

fn handle_profile_picker_key(app: &mut AppState, key: KeyCode) {
    match key {
        KeyCode::Esc => app.mode = Mode::Normal,
        KeyCode::Char('j') | KeyCode::Down => {
            if !app.profiles.is_empty() {
                app.profile_selected = (app.profile_selected + 1) % app.profiles.len();
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if !app.profiles.is_empty() {
                app.profile_selected = app
                    .profile_selected
                    .checked_sub(1)
                    .unwrap_or(app.profiles.len() - 1);
            }
        }
        KeyCode::Enter => {
            if let Some((name, profile)) = app.profiles.get(app.profile_selected) {
                let name = name.clone();
                let host = profile.host.clone();
                let lp = profile.local_port;
                let rp = profile.remote_port;
                match actions::start_profile(&name, &host, lp, rp) {
                    Ok(msg) => app.status_message = Some(msg),
                    Err(msg) => app.status_message = Some(msg),
                }
                app.mode = Mode::Normal;
                app.refresh_forwards();
            }
        }
        _ => {}
    }
}

fn handle_confirm_key(app: &mut AppState, key: KeyCode) {
    match key {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            let action = if let Mode::Confirm(action) = &app.mode {
                action.clone()
            } else {
                return;
            };
            match action {
                ConfirmAction::Stop(name) => match actions::stop_forward(&name) {
                    Ok(msg) => app.status_message = Some(msg),
                    Err(msg) => app.status_message = Some(msg),
                },
                ConfirmAction::Restart(name) => match actions::restart_forward(&name) {
                    Ok(msg) => app.status_message = Some(msg),
                    Err(msg) => app.status_message = Some(msg),
                },
            }
            app.mode = Mode::Normal;
            app.refresh_forwards();
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            app.mode = Mode::Normal;
        }
        _ => {}
    }
}

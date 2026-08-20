pub mod actions;
pub mod state;
pub mod tree;
pub mod ui;

use crate::error::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
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

        app.poll_actions();

        // With work in flight, wake often enough that its result shows the
        // moment it lands rather than at the next tick.
        let mut timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if app.pending_actions > 0 {
            timeout = timeout.min(Duration::from_millis(100));
        }
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
                handle_key(&mut app, key);
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

fn handle_key(app: &mut AppState, key: KeyEvent) {
    match &app.mode {
        Mode::Normal => handle_normal_key(app, key),
        Mode::Logs => handle_logs_key(app, key.code),
        Mode::NewForward => handle_new_forward_key(app, key.code),
        Mode::ProfilePicker => handle_profile_picker_key(app, key.code),
        Mode::Filter => handle_filter_key(app, key.code),
        Mode::Confirm(_) => handle_confirm_key(app, key.code),
        // Any key closes help — and is swallowed, so a key you were reading
        // about does not fire the moment you dismiss the overlay.
        Mode::Help => app.mode = Mode::Normal,
    }
}

fn handle_normal_key(app: &mut AppState, key: KeyEvent) {
    // Half a page, and never zero, so ctrl-d/u still move on a tiny terminal.
    let half_page = (app.tree_visible / 2).max(1) as isize;

    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('d') => app.select_by(half_page),
            KeyCode::Char('u') => app.select_by(-half_page),
            _ => {}
        }
        return;
    }

    match key.code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('j') | KeyCode::Down => app.select_next(),
        KeyCode::Char('k') | KeyCode::Up => app.select_prev(),
        KeyCode::Char('g') | KeyCode::Home => app.select_first(),
        KeyCode::Char('G') | KeyCode::End => app.select_last(),
        KeyCode::PageDown => app.select_by(app.tree_visible.max(1) as isize),
        KeyCode::PageUp => app.select_by(-(app.tree_visible.max(1) as isize)),

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
        KeyCode::Char('?') => app.mode = Mode::Help,

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
                app.open_logs(&host);
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

        KeyCode::Char('o') => match selected_forward_url(app) {
            Some(url) => {
                let cmd = if cfg!(target_os = "macos") {
                    "open"
                } else {
                    "xdg-open"
                };
                let _ = std::process::Command::new(cmd).arg(&url).spawn();
                app.status_message = Some(format!("Opened {url}"));
            }
            None => {
                app.status_message = Some("Select a forward to open its URL".to_string());
            }
        },

        // The URL as text, for the curl command or the terminal on the other
        // monitor — the other half of `o`.
        KeyCode::Char('y') => match selected_forward_url(app) {
            Some(url) => {
                app.status_message = Some(match (app.clipboard)(&url) {
                    Ok(()) => format!("Copied {url}"),
                    Err(e) => format!("Failed to copy: {e}"),
                });
            }
            None => {
                app.status_message = Some("Select a forward to copy its URL".to_string());
            }
        },
        _ => {}
    }
}

/// `http://localhost:<port>` for the forward under the cursor, if the cursor
/// is on one.
fn selected_forward_url(app: &AppState) -> Option<String> {
    let Sel::Forward(host, name) = app.selected_sel()? else {
        return None;
    };
    let machine = app.machines.iter().find(|m| m.host == host)?;
    let f = machine.forwards.iter().find(|f| f.name == name)?;
    Some(format!("http://localhost:{}", f.local_port))
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
        // The arrows walk the narrowed list without leaving the prompt, so
        // filter-then-pick is one motion. Letters stay filter text — j/k
        // included — which is why only the arrow keys carry motion here.
        KeyCode::Down => app.select_next(),
        KeyCode::Up => app.select_prev(),
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
        KeyCode::Char('j') | KeyCode::Down => app.log_scroll_down(1),
        KeyCode::Char('k') | KeyCode::Up => app.log_scroll_up(1),
        KeyCode::PageDown => app.log_scroll_down(app.log_visible.max(1)),
        KeyCode::PageUp => app.log_scroll_up(app.log_visible.max(1)),
        KeyCode::Char('g') | KeyCode::Home => app.log_to_top(),
        KeyCode::Char('G') | KeyCode::End => app.log_to_bottom(),
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

    app.mode = Mode::Normal;
    app.run_action_following(
        &format!("Starting {name}…"),
        Some((host.clone(), name.clone())),
        move || actions::start_adhoc(&host, local, remote, Some(&name)),
    );
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
                app.mode = Mode::Normal;
                // A profile targets its own host, which may not be the machine
                // the cursor was on, so follow the forward we just made.
                let follow = Some((host.clone(), name.clone()));
                app.run_action_following(&format!("Starting {name}…"), follow, move || {
                    actions::start_profile(&name, &host, lp, rp)
                });
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
            match action {
                ConfirmAction::StopForward(name) => {
                    app.run_action(&format!("Stopping {name}…"), move || {
                        actions::stop_forward(&name)
                    });
                }
                ConfirmAction::StopHost(host, _) => {
                    app.run_action(&format!("Stopping {host}…"), move || {
                        actions::stop_host(&host)
                    });
                }
                ConfirmAction::RestartForward(name) => {
                    app.run_action(&format!("Restarting {name}…"), move || {
                        actions::restart_forward(&name)
                    });
                }
                ConfirmAction::RestartHost(host) => {
                    app.run_action(&format!("Reconnecting {host}…"), move || {
                        actions::restart_host(&host)
                    });
                }
            }
            app.mode = Mode::Normal;
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

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn app_with_n_hosts(n: usize) -> AppState {
        let hosts: Vec<String> = (0..n).map(|i| format!("host-{i:02}")).collect();
        let refs: Vec<&str> = hosts.iter().map(|h| h.as_str()).collect();
        app_with_hosts(&refs)
    }

    #[test]
    fn g_and_shift_g_jump_to_the_ends_of_the_list() {
        let mut app = app_with_n_hosts(30);
        app.select(5);

        handle_key(&mut app, key(KeyCode::Char('G')));
        assert_eq!(app.selected(), 29, "G should jump to the last row");

        handle_key(&mut app, key(KeyCode::Char('g')));
        assert_eq!(app.selected(), 0, "g should jump to the first row");
    }

    #[test]
    fn page_keys_move_by_the_viewport_and_clamp_at_the_ends() {
        let mut app = app_with_n_hosts(30);
        app.tree_visible = 10;

        handle_key(&mut app, key(KeyCode::PageDown));
        assert_eq!(app.selected(), 10, "PageDown should move a full viewport");

        // Near the end, paging pins to the last row rather than wrapping —
        // wrap-around on a page jump is disorienting in a long list.
        app.select(25);
        handle_key(&mut app, key(KeyCode::PageDown));
        assert_eq!(app.selected(), 29, "PageDown should clamp, not wrap");

        app.select(3);
        handle_key(&mut app, key(KeyCode::PageUp));
        assert_eq!(app.selected(), 0, "PageUp should clamp at the top");
    }

    #[test]
    fn ctrl_d_and_ctrl_u_move_half_a_viewport() {
        let mut app = app_with_n_hosts(30);
        app.tree_visible = 10;

        handle_key(&mut app, ctrl('d'));
        assert_eq!(app.selected(), 5, "ctrl-d should move half a viewport");

        handle_key(&mut app, ctrl('u'));
        assert_eq!(app.selected(), 0, "ctrl-u should move back up");
    }

    /// One live machine `gpu-01` with one forward `jupyter` on 8888, with the
    /// forward row selected.
    fn app_on_a_forward() -> AppState {
        use crate::session::{AttachStatus, ForwardObs, SessionState, SessionStatus};
        use crate::watcher::RetryPolicy;

        let mut s = SessionState::new(
            "gpu-01".to_string(),
            std::process::id(),
            true,
            RetryPolicy::default(),
        );
        s.status = SessionStatus::Connected;
        s.forwards = vec![ForwardObs {
            name: "jupyter".to_string(),
            local_port: 8888,
            remote_host: "localhost".to_string(),
            remote_port: 8888,
            status: AttachStatus::Forwarded,
            attached_at: None,
            error: None,
        }];

        let mut app = app_with_hosts(&[]);
        app.machines = tree::build_machines(
            vec![s],
            &BTreeSet::new(),
            &[],
            MachineListMode::AllHosts,
            "",
        );
        app.expanded = tree::default_expanded(&app.machines);
        app.rows = tree::flatten(&app.machines, &app.expanded);
        app.select(1); // the forward row
        app
    }

    /// Poll until the background action lands, or a deadline passes.
    fn wait_for_actions(app: &mut AppState) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.pending_actions > 0 && Instant::now() < deadline {
            app.poll_actions();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(app.pending_actions, 0, "action never reported back");
    }

    #[test]
    fn run_action_reports_back_without_blocking_the_caller() {
        let mut app = app_with_hosts(&["nas"]);

        app.run_action("Stopping x…", || Ok("Stopped x".to_string()));
        assert_eq!(app.pending_actions, 1, "the action should be pending, not done");
        assert_eq!(app.pending_label.as_deref(), Some("Stopping x…"));

        wait_for_actions(&mut app);
        assert_eq!(app.status_message.as_deref(), Some("Stopped x"));
        assert_eq!(app.pending_label, None, "the label should clear with the work");
    }

    #[test]
    fn a_slow_confirm_does_not_freeze_the_ui_thread() {
        let mut app = app_on_a_forward();
        // restart_forward sleeps 500ms between stop and start; on the UI
        // thread that was half a second of frozen screen.
        app.mode = Mode::Confirm(ConfirmAction::RestartForward("jupyter".to_string()));

        let t0 = Instant::now();
        handle_key(&mut app, key(KeyCode::Char('y')));

        assert!(
            t0.elapsed() < Duration::from_millis(200),
            "confirming an action blocked the UI thread"
        );
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.pending_actions, 1);
        assert!(
            app.pending_label.as_deref().is_some_and(|l| l.contains("jupyter")),
            "the bar should say what is in flight: {:?}",
            app.pending_label
        );

        wait_for_actions(&mut app);
    }

    #[test]
    fn a_finished_start_selects_the_forward_it_made() {
        let mut app = app_with_hosts(&["nas"]);

        app.run_action_following(
            "Starting db…",
            Some(("newhost".to_string(), "db".to_string())),
            || Ok("Started db".to_string()),
        );
        wait_for_actions(&mut app);

        // The follow ran: the target machine was unfolded to show the forward.
        assert!(
            app.expanded.contains("newhost"),
            "completion should follow the forward it created"
        );
    }

    #[test]
    fn question_mark_opens_help_and_any_key_closes_it() {
        let mut app = app_with_hosts(&["nas"]);

        handle_key(&mut app, key(KeyCode::Char('?')));
        assert_eq!(app.mode, Mode::Help);

        // Any key dismisses it — and is swallowed, not executed: `x` here
        // must not open a stop-confirm dialog.
        handle_key(&mut app, key(KeyCode::Char('x')));
        assert_eq!(app.mode, Mode::Normal, "any key should close help");
    }

    #[test]
    fn y_copies_the_forwards_local_url() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let mut app = app_on_a_forward();
        let copied: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let sink = copied.clone();
        app.clipboard = Box::new(move |text| {
            *sink.borrow_mut() = Some(text.to_string());
            Ok(())
        });

        handle_key(&mut app, key(KeyCode::Char('y')));

        assert_eq!(
            copied.borrow().as_deref(),
            Some("http://localhost:8888"),
            "y should put the local URL on the clipboard"
        );
        assert_eq!(
            app.status_message.as_deref(),
            Some("Copied http://localhost:8888")
        );
    }

    #[test]
    fn y_on_a_machine_row_explains_itself() {
        let mut app = app_on_a_forward();
        app.clipboard = Box::new(|_| panic!("nothing should be copied"));
        app.select(0); // the machine row

        handle_key(&mut app, key(KeyCode::Char('y')));

        assert_eq!(
            app.status_message.as_deref(),
            Some("Select a forward to copy its URL")
        );
    }

    fn numbered_lines(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("line {i}")).collect()
    }

    /// An app sitting in the log viewer with `lines` loaded and a viewport
    /// `visible` rows tall.
    fn log_app(lines: usize, visible: usize) -> AppState {
        let mut app = app_with_hosts(&["gpu-01"]);
        app.mode = Mode::Logs;
        app.log_visible = visible;
        app.set_log_lines(numbered_lines(lines));
        app
    }

    #[test]
    fn logs_follow_the_tail_until_you_scroll_up() {
        let mut app = log_app(100, 10);
        assert_eq!(app.log_scroll, 90, "opening the log should show the tail");

        // The 1s tick reloads the file; new lines keep the tail in view.
        app.set_log_lines(numbered_lines(105));
        assert_eq!(app.log_scroll, 95, "a growing log should stay pinned to the tail");
    }

    #[test]
    fn reloading_logs_keeps_your_place_once_you_scroll_up() {
        let mut app = log_app(100, 10);

        handle_key(&mut app, key(KeyCode::Char('k')));
        assert_eq!(app.log_scroll, 89, "k should scroll up one line");

        // You are reading an old error; the tick reload must not yank you
        // back to the bottom.
        app.set_log_lines(numbered_lines(105));
        assert_eq!(app.log_scroll, 89, "a reload stole the scroll position");
    }

    #[test]
    fn scrolling_back_to_the_bottom_resumes_following() {
        let mut app = log_app(100, 10);

        handle_key(&mut app, key(KeyCode::Char('k')));
        handle_key(&mut app, key(KeyCode::Char('j')));

        app.set_log_lines(numbered_lines(105));
        assert_eq!(app.log_scroll, 95, "returning to the tail should resume following");
    }

    #[test]
    fn g_and_shift_g_jump_within_the_log() {
        let mut app = log_app(100, 10);

        handle_key(&mut app, key(KeyCode::Char('g')));
        assert_eq!(app.log_scroll, 0, "g should jump to the top");
        app.set_log_lines(numbered_lines(105));
        assert_eq!(app.log_scroll, 0, "reading the top must survive a reload");

        handle_key(&mut app, key(KeyCode::Char('G')));
        assert_eq!(app.log_scroll, 95, "G should jump back to the tail");
    }

    #[test]
    fn page_keys_page_through_the_log() {
        let mut app = log_app(100, 10);

        handle_key(&mut app, key(KeyCode::PageUp));
        assert_eq!(app.log_scroll, 80, "PageUp should move a viewport up");

        handle_key(&mut app, key(KeyCode::PageDown));
        assert_eq!(app.log_scroll, 90, "PageDown should move a viewport down");
    }

    #[test]
    fn arrows_browse_the_list_while_the_filter_is_open() {
        // `/` narrows the list as you type; the arrows walk the matches
        // without leaving the prompt, so filter-then-pick is one motion.
        let mut app = app_with_hosts(&["gpu-01", "gpu-02", "nas"]);
        app.mode = Mode::Filter;

        handle_key(&mut app, key(KeyCode::Down));
        assert_eq!(app.selected(), 1, "Down should move the selection");

        handle_key(&mut app, key(KeyCode::Up));
        assert_eq!(app.selected(), 0, "Up should move it back");

        // j/k stay literal — they are filter text, not motion.
        handle_key(&mut app, key(KeyCode::Char('j')));
        assert_eq!(app.filter, "j", "typed letters must keep filtering");
    }

    #[test]
    fn esc_in_normal_mode_clears_an_applied_filter() {
        // The empty state promises "Esc clears the filter" — that has to hold
        // after Enter applied it and returned to Normal mode, not only while
        // still typing in the filter prompt.
        let mut app = app_with_hosts(&["gpu-01", "nas"]);
        app.filter = "gpu".to_string();

        handle_key(&mut app, KeyEvent::from(KeyCode::Esc));

        assert!(app.filter.is_empty(), "Esc should clear the applied filter");
        assert_eq!(app.mode, Mode::Normal);
    }
}

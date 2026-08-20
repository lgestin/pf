use super::state::{AppState, ConfirmAction, InputField, Mode};
use super::tree::{MachineRow, Row};
use crate::process;
use crate::session::{AttachStatus, SessionStatus};
use chrono::{DateTime, Utc};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, HighlightSpacing, List, ListItem, Paragraph, Row as TRow,
    Scrollbar, ScrollbarOrientation, ScrollbarState, Table, Wrap,
};
use ratatui::Frame;

pub fn render(f: &mut Frame, app: &mut AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(8),
            Constraint::Length(if matches!(app.mode, Mode::Normal | Mode::Filter) {
                0
            } else {
                15
            }),
            Constraint::Length(1),
        ])
        .split(f.area());

    render_tree(f, app, chunks[0]);

    let mode = app.mode.clone();
    match &mode {
        Mode::Logs => render_log_panel(f, app, chunks[1]),
        Mode::NewForward => render_new_forward_form(f, app, chunks[1]),
        Mode::Normal | Mode::Filter | Mode::ProfilePicker | Mode::Confirm(_) => {}
    }

    render_status_bar(f, app, chunks[2]);

    if matches!(mode, Mode::ProfilePicker) {
        render_profile_picker(f, app);
    } else if let Mode::Confirm(action) = &mode {
        render_confirm_dialog(f, action);
    }
}

/// Uptime for a machine row: how long the *current* master has held.
fn session_uptime(machine: &MachineRow) -> String {
    match machine.session.as_ref().and_then(|s| s.connected_at) {
        Some(at) => format_since(at),
        None => "-".to_string(),
    }
}

fn format_since(at: DateTime<Utc>) -> String {
    format_duration((Utc::now() - at).num_seconds())
}

fn machine_row(machine: &MachineRow, expanded: bool) -> TRow<'static> {
    let marker = if machine.forwards.is_empty() {
        " "
    } else if expanded {
        "▾"
    } else {
        "▸"
    };

    // Forward count rides in column 0 so a collapsed machine still shows its
    // uptime — folding must not cost information.
    let count = if machine.forwards.is_empty() {
        String::new()
    } else {
        format!(" ({})", machine.forwards.len())
    };
    let label = format!("{marker} {}{count}", machine.host);

    let (status, color) = match machine.session.as_ref().map(|s| s.status) {
        Some(SessionStatus::Connected) => ("connected", Color::Green),
        Some(SessionStatus::Connecting) => ("connecting", Color::Yellow),
        Some(SessionStatus::Reconnecting) => ("reconnecting", Color::Yellow),
        Some(SessionStatus::Failed) => ("failed", Color::Red),
        None => ("idle", Color::DarkGray),
    };

    let alive = machine
        .session
        .as_ref()
        .is_some_and(|s| process::is_alive(s.watcher_pid));
    let (status, color) = if machine.session.is_some() && !alive {
        ("failed", Color::Red)
    } else {
        (status, color)
    };

    let reconnects = match machine.session.as_ref() {
        Some(s) if s.reconnect_count > 0 => format!("↻{}", s.reconnect_count),
        Some(_) => "↻0".to_string(),
        None => "-".to_string(),
    };

    TRow::new(vec![
        Cell::from(label).style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from(status).style(Style::default().fg(color)),
        Cell::from(session_uptime(machine)),
        Cell::from(reconnects).style(Style::default().fg(Color::DarkGray)),
    ])
}

fn forward_row(machine: &MachineRow, fi: usize) -> TRow<'static> {
    let f = &machine.forwards[fi];
    let label = format!("    {} → {}:{}", f.local_port, f.remote_host, f.remote_port);

    let (status, color) = match f.status {
        AttachStatus::Attached => ("attached", Color::Green),
        AttachStatus::Pending => ("pending", Color::Yellow),
        AttachStatus::Failed => ("failed", Color::Red),
    };

    let uptime = match f.attached_at {
        Some(at) => format_since(at),
        None => "-".to_string(),
    };

    // A failed attach has an error worth surfacing; it replaces the uptime,
    // which would be "-" anyway.
    let detail = if f.status == AttachStatus::Failed {
        f.error
            .as_deref()
            .map(short_error)
            .unwrap_or_else(|| uptime.clone())
    } else {
        uptime
    };

    TRow::new(vec![
        Cell::from(label).style(Style::default().fg(Color::Gray)),
        Cell::from(status).style(Style::default().fg(color)),
        Cell::from(detail),
        Cell::from(""),
    ])
}

/// ssh's stderr can be several lines; the table has one narrow cell.
fn short_error(err: &str) -> String {
    let first = err.lines().next().unwrap_or(err).trim();
    if first.len() > 24 {
        format!("{}…", &first[..23])
    } else {
        first.to_string()
    }
}

fn render_tree(f: &mut Frame, app: &mut AppState, area: Rect) {
    let header = TRow::new(vec![
        Cell::from("Machine / Forward"),
        Cell::from("Status"),
        Cell::from("Uptime"),
        Cell::from("Reconn"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let rows: Vec<TRow> = app
        .rows
        .iter()
        .filter_map(|row| match row {
            Row::Machine(mi) => {
                let machine = app.machines.get(*mi)?;
                Some(machine_row(machine, app.expanded.contains(&machine.host)))
            }
            Row::Forward(mi, fi) => {
                let machine = app.machines.get(*mi)?;
                (*fi < machine.forwards.len()).then(|| forward_row(machine, *fi))
            }
        })
        .collect();

    let title = if app.filter.is_empty() {
        format!(" Machines ({}) ", app.machine_source.label())
    } else {
        format!(" Machines — filter: {} ", app.filter)
    };

    let table = Table::new(
        rows,
        [
            Constraint::Fill(1),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Length(7),
        ],
    )
    .header(header)
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("› ")
    .highlight_spacing(HighlightSpacing::Always)
    .block(Block::default().title(title).borders(Borders::ALL));

    f.render_stateful_widget(table, area, &mut app.table_state);

    let visible = area.height.saturating_sub(3) as usize;
    if app.rows.len() > visible {
        let mut sb = ScrollbarState::new(app.rows.len()).position(app.table_state.offset());
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None),
            area,
            &mut sb,
        );
    }
}

fn render_log_panel(f: &mut Frame, app: &AppState, area: Rect) {
    let visible_lines = area.height.saturating_sub(2) as usize;
    let end = (app.log_scroll + visible_lines).min(app.log_lines.len());
    let start = app.log_scroll.min(end);

    let lines: Vec<Line> = app.log_lines[start..end]
        .iter()
        .map(|l| Line::from(l.as_str()))
        .collect();

    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .title(format!(" Session log: {} ", app.log_name))
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: false });

    f.render_widget(para, area);
}

fn render_new_forward_form(f: &mut Frame, app: &AppState, area: Rect) {
    let block = Block::default()
        .title(format!(" New forward on {} ", app.input_host))
        .borders(Borders::ALL);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let fields = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(inner);

    let entries = [
        (InputField::LocalPort, &app.input_local_port),
        (InputField::RemotePort, &app.input_remote_port),
        (InputField::Name, &app.input_name),
    ];

    for (i, (field, value)) in entries.iter().enumerate() {
        let active = *field == app.input_field;
        let style = if active {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let cursor = if active { "_" } else { "" };
        let line = Line::from(vec![
            Span::styled(format!("{:>12}: ", field.label()), style),
            Span::raw(format!("{value}{cursor}")),
        ]);
        f.render_widget(Paragraph::new(line), fields[i]);
    }

    let hint = if app.input_field == InputField::Name {
        "  Tab: next field | Enter: submit | Esc: cancel"
    } else {
        "  Tab: next field | Enter: next field | Esc: cancel"
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(Color::DarkGray),
        ))),
        fields[3],
    );
}

fn render_profile_picker(f: &mut Frame, app: &mut AppState) {
    let area = centered_rect(50, 60, f.area());
    f.render_widget(Clear, area);

    let items: Vec<ListItem> = app
        .profiles
        .iter()
        .map(|(name, profile)| {
            ListItem::new(format!(
                "{name}: {} ({}:{})",
                profile.host, profile.local_port, profile.remote_port
            ))
        })
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ")
        .highlight_spacing(HighlightSpacing::Always)
        .block(
            Block::default()
                .title(" Select Profile ")
                .borders(Borders::ALL),
        );

    f.render_stateful_widget(list, area, &mut app.profile_state);
}

fn render_confirm_dialog(f: &mut Frame, action: &ConfirmAction) {
    let area = centered_rect(46, 20, f.area());
    f.render_widget(Clear, area);

    let msg = match action {
        ConfirmAction::StopForward(name) => format!("Stop '{name}'? (y/n)"),
        ConfirmAction::StopHost(host, n) => {
            format!("Stop all {n} forward(s) on '{host}'? (y/n)")
        }
        ConfirmAction::RestartForward(name) => format!("Restart '{name}'? (y/n)"),
        ConfirmAction::RestartHost(host) => {
            format!("Reconnect '{host}'? All its forwards drop briefly. (y/n)")
        }
    };

    let para = Paragraph::new(msg)
        .wrap(Wrap { trim: true })
        .block(Block::default().title(" Confirm ").borders(Borders::ALL));
    f.render_widget(para, area);
}

fn render_status_bar(f: &mut Frame, app: &AppState, area: Rect) {
    let hint = match &app.mode {
        Mode::Normal => {
            "j/k:nav  ↵:fold  a:add  x:stop  r:restart  l:logs  /:filter  m:mode  s:profile  q:quit"
        }
        Mode::Logs => "j/k:scroll  Esc:back  q:quit",
        Mode::NewForward => "Tab:next  Enter:next/submit  Esc:cancel",
        Mode::ProfilePicker => "j/k:nav  Enter:start  Esc:cancel",
        Mode::Filter => "type to filter  Enter:apply  Esc:clear",
        Mode::Confirm(_) => "y:confirm  n/Esc:cancel",
    };

    let text = if app.mode == Mode::Filter {
        format!("/{}_  |  {hint}", app.filter)
    } else if let Some(msg) = &app.status_message {
        format!("{msg}  |  {hint}")
    } else {
        hint.to_string()
    };

    let bar = Paragraph::new(Line::from(Span::styled(
        text,
        Style::default().fg(Color::Cyan),
    )));
    f.render_widget(bar, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{ForwardObs, SessionState};
    use crate::tui::tree::{self, MachineListMode};
    use crate::watcher::RetryPolicy;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::BTreeSet;

    fn obs(name: &str, port: u16, status: AttachStatus) -> ForwardObs {
        ForwardObs {
            name: name.to_string(),
            local_port: port,
            remote_host: "localhost".to_string(),
            remote_port: port,
            status,
            attached_at: Some(Utc::now()),
            error: None,
        }
    }

    fn live(host: &str, forwards: Vec<ForwardObs>) -> SessionState {
        let mut s = SessionState::new(host.to_string(), std::process::id(), true, RetryPolicy::default());
        s.status = SessionStatus::Connected;
        s.connected_at = Some(Utc::now());
        s.forwards = forwards;
        s
    }

    fn app_with(sessions: Vec<SessionState>, ssh_hosts: &[&str]) -> AppState {
        let mut app = AppState::new();
        app.ssh_hosts = ssh_hosts.iter().map(|h| h.to_string()).collect();
        app.machine_source = MachineListMode::AllHosts;
        app.machines = tree::build_machines(
            sessions,
            &BTreeSet::new(),
            &app.ssh_hosts,
            MachineListMode::AllHosts,
            "",
        );
        app.expanded = tree::default_expanded(&app.machines);
        app.rows = tree::flatten(&app.machines, &app.expanded);
        app.table_state.select(Some(0));
        app
    }

    fn draw(app: &mut AppState, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn an_expanded_machine_shows_its_forwards_indented() {
        let mut app = app_with(
            vec![live("gpu-01", vec![obs("jupyter", 8888, AttachStatus::Attached)])],
            &[],
        );
        let text = draw(&mut app, 90, 12);

        assert!(text.contains("gpu-01"), "machine row missing:\n{text}");
        assert!(text.contains("▾"), "expanded marker missing:\n{text}");
        assert!(text.contains("(1)"), "forward count missing:\n{text}");
        assert!(text.contains("8888 → localhost:8888"), "forward row missing:\n{text}");
        assert!(text.contains("connected"), "session status missing:\n{text}");
        assert!(text.contains("attached"), "attach status missing:\n{text}");
    }

    #[test]
    fn a_collapsed_machine_hides_forwards_but_keeps_its_own_row() {
        let mut app = app_with(
            vec![live("gpu-01", vec![obs("jupyter", 8888, AttachStatus::Attached)])],
            &[],
        );
        app.expanded.clear();
        app.rows = tree::flatten(&app.machines, &app.expanded);

        let text = draw(&mut app, 90, 12);
        assert!(text.contains("gpu-01"), "machine row vanished:\n{text}");
        assert!(text.contains("▸"), "collapsed marker missing:\n{text}");
        assert!(!text.contains("8888 → localhost"), "forward leaked while collapsed:\n{text}");
        // Folding must not cost information: the count still shows.
        assert!(text.contains("(1)"), "forward count lost when collapsed:\n{text}");
    }

    #[test]
    fn idle_hosts_render_without_a_session() {
        let mut app = app_with(vec![], &["nas", "bastion"]);
        let text = draw(&mut app, 90, 12);
        assert!(text.contains("nas"), "idle host missing:\n{text}");
        assert!(text.contains("idle"), "idle status missing:\n{text}");
    }

    #[test]
    fn a_failed_forward_shows_its_error_instead_of_an_uptime() {
        let mut f = obs("boom", 6006, AttachStatus::Failed);
        f.attached_at = None;
        f.error = Some("bind [127.0.0.1]:6006: Address already in use".to_string());

        let mut app = app_with(vec![live("gpu-01", vec![f])], &[]);
        let text = draw(&mut app, 100, 12);
        assert!(text.contains("failed"), "failed status missing:\n{text}");
        assert!(text.contains("bind"), "error text missing:\n{text}");
    }

    #[test]
    fn selection_past_the_viewport_scrolls_into_view() {
        let sessions: Vec<SessionState> = (0..40)
            .map(|i| live(&format!("host-{i:02}"), vec![]))
            .collect();
        let mut app = app_with(sessions, &[]);
        app.select(39);

        let text = draw(&mut app, 90, 12);
        assert!(text.contains("host-39"), "selected row not rendered:\n{text}");
        assert!(!text.contains("host-00"), "viewport did not scroll:\n{text}");
    }

    #[test]
    fn the_filter_shows_in_the_title() {
        let mut app = app_with(vec![live("gpu-01", vec![])], &[]);
        app.filter = "gpu".to_string();
        let text = draw(&mut app, 90, 12);
        assert!(text.contains("filter: gpu"), "filter not shown:\n{text}");
    }

    /// Not an assertion — prints the layout so it can be eyeballed.
    /// `cargo test preview_layout -- --nocapture --ignored`
    #[test]
    #[ignore]
    fn preview_layout() {
        let mut gpu1 = live(
            "gpu-01",
            vec![
                obs("jupyter", 8888, AttachStatus::Attached),
                obs("tensorboard", 6006, AttachStatus::Attached),
            ],
        );
        gpu1.reconnect_count = 1;

        let mut boom = obs("db", 5432, AttachStatus::Failed);
        boom.attached_at = None;
        boom.error = Some("bind [127.0.0.1]:5432: Address already in use".to_string());
        let gpu2 = live("gpu-02", vec![obs("api", 3000, AttachStatus::Attached), boom]);

        let mut app = app_with(vec![gpu1, gpu2], &["bastion", "nas", "dev-box"]);
        app.expanded.remove("gpu-02");
        app.rows = tree::flatten(&app.machines, &app.expanded);
        app.select(1);

        let mut terminal = Terminal::new(TestBackend::new(76, 12)).unwrap();
        terminal.draw(|f| render(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        println!();
        for y in 0..buf.area.height {
            let line: String = (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect();
            println!("{}", line.trim_end());
        }
        println!();
    }

    #[test]
    fn the_add_form_names_its_machine_and_has_no_host_field() {
        let mut app = app_with(vec![live("gpu-01", vec![])], &[]);
        app.open_new_forward_form("gpu-01".to_string());

        let text = draw(&mut app, 90, 24);
        assert!(text.contains("New forward on gpu-01"), "host not in title:\n{text}");
        assert!(text.contains("Local Port"), "port field missing:\n{text}");
        // The machine list is the host picker now; the form must not ask again.
        assert!(!text.contains("Host:"), "form still asks for a host:\n{text}");
    }
}

use super::state::{AppState, ConfirmAction, InputField, Mode};
use crate::process;
use crate::state::ForwardStatus;
use chrono::Utc;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, Wrap};
use ratatui::Frame;

pub fn render(f: &mut Frame, app: &AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(8),
            Constraint::Length(if matches!(app.mode, Mode::Normal) { 0 } else { 15 }),
            Constraint::Length(1),
        ])
        .split(f.area());

    render_forwards_table(f, app, chunks[0]);

    match &app.mode {
        Mode::Logs => render_log_panel(f, app, chunks[1]),
        Mode::NewForward => render_new_forward_form(f, app, chunks[1]),
        Mode::Normal => {}
        Mode::ProfilePicker | Mode::Confirm(_) => {}
    }

    render_status_bar(f, app, chunks[2]);

    // Render overlays
    match &app.mode {
        Mode::ProfilePicker => render_profile_picker(f, app),
        Mode::Confirm(action) => render_confirm_dialog(f, action),
        _ => {}
    }
}

fn render_forwards_table(f: &mut Frame, app: &AppState, area: Rect) {
    let header = Row::new(vec![
        Cell::from("Name"),
        Cell::from("Host"),
        Cell::from("Ports"),
        Cell::from("Status"),
        Cell::from("Uptime"),
        Cell::from("Reconn"),
        Cell::from("PIDs"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = app
        .forwards
        .iter()
        .enumerate()
        .map(|(i, fwd)| {
            let alive = process::is_alive(fwd.watcher_pid);
            let status = if alive {
                &fwd.status
            } else {
                &ForwardStatus::Failed
            };

            let status_color = match status {
                ForwardStatus::Running => Color::Green,
                ForwardStatus::Reconnecting => Color::Yellow,
                ForwardStatus::Failed => Color::Red,
                ForwardStatus::Stopped => Color::DarkGray,
            };

            let uptime = if alive {
                let dur = Utc::now() - fwd.started_at;
                format_duration(dur.num_seconds())
            } else {
                "-".to_string()
            };

            let ports = format!("{}:{}", fwd.local_port, fwd.remote_port);
            let pids = if alive {
                match fwd.ssh_pid {
                    Some(ssh) => format!("w:{} s:{}", fwd.watcher_pid, ssh),
                    None => format!("w:{}", fwd.watcher_pid),
                }
            } else {
                "-".to_string()
            };

            let style = if i == app.selected {
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            Row::new(vec![
                Cell::from(fwd.name.clone()),
                Cell::from(fwd.host.clone()),
                Cell::from(ports),
                Cell::from(status.to_string()).style(Style::default().fg(status_color)),
                Cell::from(uptime),
                Cell::from(fwd.reconnect_count.to_string()),
                Cell::from(pids),
            ])
            .style(style)
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(15),
            Constraint::Percentage(20),
            Constraint::Percentage(12),
            Constraint::Percentage(13),
            Constraint::Percentage(12),
            Constraint::Percentage(10),
            Constraint::Percentage(18),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .title(" Forwards ")
            .borders(Borders::ALL),
    );

    f.render_widget(table, area);
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
                .title(format!(" Logs: {} ", app.log_name))
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: false });

    f.render_widget(para, area);
}

fn render_new_forward_form(f: &mut Frame, app: &AppState, area: Rect) {
    let block = Block::default()
        .title(" New Forward ")
        .borders(Borders::ALL);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Extra rows for host suggestions
    let suggestion_rows = if app.input_field == InputField::Host && !app.host_suggestions.is_empty() {
        app.host_suggestions.len().min(5) as u16
    } else {
        0
    };

    let mut constraints = vec![
        Constraint::Length(1), // Host
    ];
    if suggestion_rows > 0 {
        constraints.push(Constraint::Length(suggestion_rows));
    }
    constraints.extend([
        Constraint::Length(1), // LocalPort
        Constraint::Length(1), // RemotePort
        Constraint::Length(1), // Name
        Constraint::Length(1), // Hint
        Constraint::Min(0),
    ]);

    let fields = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    // Host field
    let host_active = app.input_field == InputField::Host;
    let host_style = if host_active {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let host_cursor = if host_active { "_" } else { "" };
    let host_line = Line::from(vec![
        Span::styled(format!("{:>12}: ", "Host"), host_style),
        Span::raw(format!("{}{host_cursor}", app.input_host)),
    ]);
    f.render_widget(Paragraph::new(host_line), fields[0]);

    // Host suggestions (if active and available)
    let offset = if suggestion_rows > 0 {
        let suggestions: Vec<Line> = app
            .host_suggestions
            .iter()
            .take(5)
            .enumerate()
            .map(|(i, h)| {
                let marker = if app.host_suggestion_idx == Some(i) { "> " } else { "  " };
                let style = if app.host_suggestion_idx == Some(i) {
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                Line::from(Span::styled(format!("{:>12}  {marker}{h}", ""), style))
            })
            .collect();
        f.render_widget(Paragraph::new(suggestions), fields[1]);
        2 // skip fields[0] (host) and fields[1] (suggestions)
    } else {
        1 // skip fields[0] (host) only
    };

    // Remaining fields
    let remaining = [
        (InputField::LocalPort, &app.input_local_port),
        (InputField::RemotePort, &app.input_remote_port),
        (InputField::Name, &app.input_name),
    ];

    for (i, (field, value)) in remaining.iter().enumerate() {
        let active = *field == app.input_field;
        let style = if active {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let cursor = if active { "_" } else { "" };
        let line = Line::from(vec![
            Span::styled(format!("{:>12}: ", field.label()), style),
            Span::raw(format!("{value}{cursor}")),
        ]);
        f.render_widget(Paragraph::new(line), fields[offset + i]);
    }

    let hint_text = if app.input_field == InputField::Host && !app.host_suggestions.is_empty() {
        "  Tab: cycle hosts | Enter: next field | Esc: cancel"
    } else if app.input_field == InputField::Name {
        "  Tab: next field | Enter: submit | Esc: cancel"
    } else {
        "  Tab: next field | Enter: next field | Esc: cancel"
    };
    let hint = Line::from(Span::styled(
        hint_text,
        Style::default().fg(Color::DarkGray),
    ));
    // offset + 3 remaining fields = hint row index
    f.render_widget(Paragraph::new(hint), fields[offset + 3]);
}

fn render_profile_picker(f: &mut Frame, app: &AppState) {
    let area = centered_rect(50, 60, f.area());
    f.render_widget(Clear, area);

    let items: Vec<ListItem> = app
        .profiles
        .iter()
        .enumerate()
        .map(|(i, (name, profile))| {
            let style = if i == app.profile_selected {
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(format!(
                "{name}: {} ({}:{})",
                profile.host, profile.local_port, profile.remote_port
            ))
            .style(style)
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .title(" Select Profile ")
            .borders(Borders::ALL),
    );
    f.render_widget(list, area);
}

fn render_confirm_dialog(f: &mut Frame, action: &ConfirmAction) {
    let area = centered_rect(40, 20, f.area());
    f.render_widget(Clear, area);

    let msg = match action {
        ConfirmAction::Stop(name) => format!("Stop '{name}'? (y/n)"),
        ConfirmAction::Restart(name) => format!("Restart '{name}'? (y/n)"),
    };

    let para = Paragraph::new(msg).block(
        Block::default()
            .title(" Confirm ")
            .borders(Borders::ALL),
    );
    f.render_widget(para, area);
}

fn render_status_bar(f: &mut Frame, app: &AppState, area: Rect) {
    let hint = match &app.mode {
        Mode::Normal => "j/k:nav  s:start profile  n:new  x:stop  r:restart  l:logs  q:quit",
        Mode::Logs => "j/k:scroll  Esc:back  q:quit",
        Mode::NewForward => "Tab:complete/next  Enter:next/submit  Esc:cancel",
        Mode::ProfilePicker => "j/k:nav  Enter:start  Esc:cancel",
        Mode::Confirm(_) => "y:confirm  n/Esc:cancel",
    };

    let text = if let Some(msg) = &app.status_message {
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

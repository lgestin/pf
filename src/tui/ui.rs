use super::state::{AppState, ConfirmAction, InputField, Mode};
use super::tree::{MachineRow, Row};
use crate::process;
use crate::session::{AttachStatus, SessionStatus};
use chrono::{DateTime, Utc};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, HighlightSpacing, List, ListItem, Paragraph,
    Row as TRow, Scrollbar, ScrollbarOrientation, ScrollbarState, Table, Wrap,
};
use ratatui::Frame;

// Palette. Named ANSI colors rather than RGB, so the tree inherits whatever
// theme the user's terminal already uses instead of fighting it.
//
// Deliberately *not* DarkGray: it maps to ANSI bright-black, which on plenty of
// themes sits a shade off the background and makes anything wearing it
// effectively invisible. Gray is ANSI 7 — recessive but always legible.
const ACCENT: Color = Color::Cyan;
const OK: Color = Color::Green;
const WARN: Color = Color::Yellow;
const BAD: Color = Color::Red;
const MUTE: Color = Color::Gray;

fn mute() -> Style {
    Style::default().fg(MUTE)
}

/// A status lamp: shape carries the state as well as color, so the tree still
/// reads on a monochrome terminal or to a colorblind eye.
fn lamp(symbol: &'static str, color: Color) -> Cell<'static> {
    Cell::from(symbol).style(Style::default().fg(color))
}

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
///
/// Blank rather than "-" when there is no session — most of an all-hosts list
/// is idle, and a column of dashes is noise on the rows that should be quietest.
fn session_uptime(machine: &MachineRow) -> String {
    match machine.session.as_ref().and_then(|s| s.connected_at) {
        Some(at) => format_since(at),
        None => String::new(),
    }
}

fn format_since(at: DateTime<Utc>) -> String {
    format_duration((Utc::now() - at).num_seconds())
}

/// The effective state of a machine, folding a dead watcher into `Failed`.
fn machine_state(machine: &MachineRow) -> Option<SessionStatus> {
    let session = machine.session.as_ref()?;
    if !process::is_alive(session.watcher_pid) {
        return Some(SessionStatus::Failed);
    }
    Some(session.status)
}

/// Lamp, word, and colour for a machine — one source of truth, so the glyph and
/// the label can never drift apart.
fn machine_look(state: Option<SessionStatus>) -> (&'static str, &'static str, Color) {
    match state {
        Some(SessionStatus::Connected) => ("●", "connected", OK),
        Some(SessionStatus::Connecting) => ("◐", "connecting", WARN),
        Some(SessionStatus::Reconnecting) => ("◐", "reconnecting", WARN),
        Some(SessionStatus::Failed) => ("✕", "failed", BAD),
        None => ("○", "", MUTE),
    }
}

fn forward_look(status: AttachStatus) -> (&'static str, &'static str, Color) {
    match status {
        AttachStatus::Forwarded => ("●", "forwarded", OK),
        AttachStatus::Pending => ("◐", "pending", WARN),
        AttachStatus::Failed => ("✕", "failed", BAD),
    }
}

fn machine_row(machine: &MachineRow, expanded: bool) -> TRow<'static> {
    let state = machine_state(machine);
    let (lamp_sym, status_text, lamp_color) = machine_look(state);

    let marker = if machine.forwards.is_empty() {
        " "
    } else if expanded {
        "▾"
    } else {
        "▸"
    };

    // Hierarchy comes from weight, not darkness: a live machine is bold, an
    // idle one is plain. Dimming hostnames made a disconnected screen
    // unreadable, since most of an all-hosts list is idle.
    let host_style = match state {
        Some(SessionStatus::Connected) => Style::default().add_modifier(Modifier::BOLD),
        Some(SessionStatus::Failed) => Style::default().fg(BAD).add_modifier(Modifier::BOLD),
        _ => Style::default(),
    };

    let mut label = vec![
        Span::styled(format!("{marker} "), mute()),
        Span::styled(machine.host.clone(), host_style),
    ];
    // Folding must not cost information, so a collapsed machine still says how
    // many forwards it holds. The middot keeps the number from reading as part
    // of the hostname.
    if !machine.forwards.is_empty() {
        label.push(Span::styled(
            format!(" ·{}", machine.forwards.len()),
            mute(),
        ));
    }

    // A reconnect count of zero is the normal case and says nothing. Render it
    // only once it becomes a signal.
    let reconnects = match machine.session.as_ref() {
        Some(s) if s.reconnect_count > 0 => Span::styled(
            format!("↻{}", s.reconnect_count),
            Style::default().fg(WARN),
        ),
        _ => Span::raw(""),
    };

    TRow::new(vec![
        lamp(lamp_sym, lamp_color),
        Cell::from(Line::from(label)),
        // The word carries the same colour as its lamp, so the state reads
        // whether the eye lands on the glyph or the label.
        Cell::from(Span::styled(status_text, Style::default().fg(lamp_color))),
        Cell::from(Span::styled(session_uptime(machine), mute())),
        Cell::from(reconnects),
    ])
}

fn forward_row(machine: &MachineRow, fi: usize) -> TRow<'static> {
    let f = &machine.forwards[fi];
    let last = fi + 1 == machine.forwards.len();

    let (lamp_sym, status_text, lamp_color) = forward_look(f.status);

    // Guides make the parent-child relation visible rather than implied by
    // whitespace, which matters once several machines are expanded at once.
    let guide = if last { "  └─ " } else { "  ├─ " };

    let port_style = if f.status == AttachStatus::Failed {
        Style::default().fg(BAD)
    } else {
        Style::default()
    };

    // Only the guide and the arrow are structure; the ports themselves are the
    // payload and stay at full readability.
    let label = vec![
        Span::styled(guide, mute()),
        Span::styled(f.local_port.to_string(), port_style),
        Span::styled(" → ", mute()),
        Span::styled(format!("{}:{}", f.remote_host, f.remote_port), Style::default()),
    ];

    // A failed forward has an error worth reading; it takes the uptime slot,
    // which would only have shown "-" anyway.
    let detail = if f.status == AttachStatus::Failed {
        match f.error.as_deref() {
            Some(e) => Span::styled(short_error(e), Style::default().fg(BAD)),
            None => Span::styled("-", mute()),
        }
    } else {
        let uptime = match f.attached_at {
            Some(at) => format_since(at),
            None => "-".to_string(),
        };
        Span::styled(uptime, mute())
    };

    TRow::new(vec![
        lamp(lamp_sym, lamp_color),
        Cell::from(Line::from(label)),
        Cell::from(Span::styled(status_text, Style::default().fg(lamp_color))),
        Cell::from(detail),
        Cell::from(""),
    ])
}

/// Compress ssh's stderr into something that fits a table cell and still says
/// what went wrong.
///
/// ssh reports the likes of `bind [127.0.0.1]:5432: Address already in use`,
/// whose front half is all mechanism. The reason lives after the last colon,
/// and the handful of reasons worth recognising get a plainer phrasing. The
/// full text is always in the session log.
fn short_error(err: &str) -> String {
    let first = err.lines().next().unwrap_or(err).trim();
    let reason = first.rsplit(": ").next().unwrap_or(first).trim();

    let lower = reason.to_lowercase();
    if lower.contains("address already in use") {
        return "port in use".to_string();
    }
    if lower.contains("permission denied") {
        return "denied".to_string();
    }
    if lower.contains("cannot assign") {
        return "bad address".to_string();
    }

    let compact = reason.to_lowercase();
    if compact.chars().count() > 12 {
        format!("{}…", compact.chars().take(11).collect::<String>())
    } else {
        compact
    }
}

/// ` pf ─ 2 of 12 connected ` — the one number worth reading from across a room.
fn tree_title(app: &AppState) -> Line<'static> {
    let connected = app
        .machines
        .iter()
        .filter(|m| machine_state(m) == Some(SessionStatus::Connected))
        .count();
    let total = app.machines.len();

    let mut spans = vec![
        Span::raw(" "),
        Span::styled("pf", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::styled("  ", mute()),
    ];

    if app.filter.is_empty() {
        spans.push(Span::styled(
            format!("{connected} of {total} connected"),
            Style::default(),
        ));
        spans.push(Span::styled(
            format!("  ·  {} ", app.machine_source.label()),
            mute(),
        ));
    } else {
        spans.push(Span::styled(format!("/{}", app.filter), Style::default().fg(ACCENT)));
        spans.push(Span::styled(format!("  ·  {total} shown "), mute()));
    }

    Line::from(spans)
}

fn render_tree(f: &mut Frame, app: &mut AppState, area: Rect) {
    let header = TRow::new(vec![
        Cell::from(""),
        Cell::from("machine"),
        Cell::from("state"),
        Cell::from("uptime"),
        Cell::from(""),
    ])
    .style(mute());

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

    let block = Block::default()
        .title(tree_title(app))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(mute());

    if rows.is_empty() {
        render_empty_state(f, app, block, area);
        return;
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(12),
            Constraint::Length(12),
            Constraint::Length(4),
        ],
    )
    .header(header)
    .column_spacing(1)
    // A plain 8-colour background rather than an indexed shade: it renders on
    // any theme, where a 256-colour dark grey assumes a dark one.
    .row_highlight_style(Style::default().bg(Color::DarkGray))
    // A solid rail reads as a cursor; a chevron reads as a bullet and fights
    // the tree guides.
    .highlight_symbol(Span::styled("▌", Style::default().fg(ACCENT)))
    .highlight_spacing(HighlightSpacing::Always)
    .block(block);

    f.render_stateful_widget(table, area, &mut app.table_state);

    let visible = area.height.saturating_sub(3) as usize;
    // Remembered so PageUp/PageDown and ctrl-d/u can move by what a "page"
    // actually is on this terminal.
    app.tree_visible = visible;
    if app.rows.len() > visible {
        let mut sb = ScrollbarState::new(app.rows.len()).position(app.table_state.offset());
        // Inset vertically so the track cannot paint over the block's corners.
        let track = area.inner(ratatui::layout::Margin {
            horizontal: 0,
            vertical: 1,
        });
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(None)
                .thumb_style(Style::default().fg(ACCENT)),
            track,
            &mut sb,
        );
    }
}

/// An empty screen is an invitation to act, so say what to press.
fn render_empty_state(f: &mut Frame, app: &AppState, block: Block<'static>, area: Rect) {
    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines = if !app.filter.is_empty() {
        vec![
            Line::from(Span::styled(
                format!("No machine matches “{}”", app.filter),
                Style::default(),
            )),
            Line::from(Span::styled("Esc clears the filter", mute())),
        ]
    } else {
        vec![
            Line::from(Span::styled("No machines to show", Style::default())),
            Line::from(Span::styled(
                "Add hosts to ~/.ssh/config, or press m to widen the list",
                mute(),
            )),
        ]
    };

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Length(lines.len() as u16),
            Constraint::Min(0),
        ])
        .split(inner);

    f.render_widget(
        Paragraph::new(lines).alignment(ratatui::layout::Alignment::Center),
        vertical[1],
    );
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
    let title = if app.input_asks_host {
        " Connect to a machine ".to_string()
    } else {
        format!(" New forward on {} ", app.input_host)
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(mute());
    let inner = block.inner(area);
    f.render_widget(block, area);

    let form = app.form_fields();
    let mut constraints: Vec<Constraint> = form.iter().map(|_| Constraint::Length(1)).collect();
    constraints.push(Constraint::Length(1)); // hint
    constraints.push(Constraint::Min(0));

    let fields = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    let entries: Vec<(InputField, &String)> = form
        .iter()
        .map(|field| {
            let value = match field {
                InputField::Host => &app.input_host,
                InputField::LocalPort => &app.input_local_port,
                InputField::RemotePort => &app.input_remote_port,
                InputField::Name => &app.input_name,
            };
            (field.clone(), value)
        })
        .collect();

    let local = app.input_local_port.trim();
    let host = app.input_host.trim();

    for (i, (field, value)) in entries.iter().enumerate() {
        let active = *field == app.input_field;
        let label_style = if active {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            mute()
        };

        // What an empty field will actually submit, and why. Shown dim, with
        // the cursor after it, so the field reads as already holding the value
        // it is going to use rather than as empty with a note beside it.
        let (ghost, note): (String, &str) = match field {
            InputField::RemotePort if !local.is_empty() => {
                (local.to_string(), "same as local")
            }
            InputField::Name if !local.is_empty() && !host.is_empty() => {
                (format!("{host}-{local}"), "generated")
            }
            InputField::Host => (String::new(), "hostname, IP, or user@host"),
            _ => (String::new(), ""),
        };

        let mut spans = vec![Span::styled(
            format!("{:>12}  ", field.label()),
            label_style,
        )];

        if value.is_empty() {
            spans.push(Span::styled(ghost.clone(), mute()));
        } else {
            spans.push(Span::raw(value.to_string()));
        }
        if active {
            spans.push(Span::raw("_"));
        }
        if value.is_empty() && !note.is_empty() {
            spans.push(Span::styled(format!("  {note}"), mute()));
        }

        f.render_widget(Paragraph::new(Line::from(spans)), fields[i]);
    }

    let hint = if app.on_last_field() {
        "tab field   ↵ start   esc cancel"
    } else {
        "tab field   ↵ next   esc cancel"
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(format!("  {hint}"), mute()))),
        fields[entries.len()],
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

/// Keys carry the accent so the eye can find one without reading the line, but
/// the descriptions stay at full foreground — this is the menu, and a menu you
/// cannot read is not a menu.
fn keys(pairs: &[(&'static str, &'static str)]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (i, (key, what)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            *key,
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(format!(" {what}")));
    }
    spans
}

fn render_status_bar(f: &mut Frame, app: &AppState, area: Rect) {
    let hint = match &app.mode {
        Mode::Normal => keys(&[
            ("j/k", "move"),
            ("↵", "fold"),
            ("a", "add"),
            ("A", "new host"),
            ("x", "stop"),
            ("r", "restart"),
            ("l", "logs"),
            ("/", "filter"),
            ("m", "list"),
            ("s", "profile"),
            ("q", "quit"),
        ]),
        Mode::Logs => keys(&[("j/k", "scroll"), ("esc", "back"), ("q", "quit")]),
        Mode::NewForward => keys(&[("tab", "field"), ("↵", "next"), ("esc", "cancel")]),
        Mode::ProfilePicker => keys(&[("j/k", "move"), ("↵", "start"), ("esc", "cancel")]),
        Mode::Filter => keys(&[("↵", "apply"), ("esc", "clear")]),
        Mode::Confirm(_) => keys(&[("y", "confirm"), ("n", "cancel")]),
    };

    let mut spans = vec![Span::raw(" ")];

    if app.mode == Mode::Filter {
        spans.push(Span::styled(
            format!("/{}_", app.filter),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled("  ", mute()));
    } else if let Some(msg) = &app.status_message {
        // Failures deserve the eye; confirmations do not.
        let style = if msg.starts_with("Failed") || msg.contains("must be") {
            Style::default().fg(BAD)
        } else {
            Style::default().fg(OK)
        };
        spans.push(Span::styled(msg.clone(), style));
        spans.push(Span::styled("  ", mute()));
    }

    spans.extend(hint);
    f.render_widget(Paragraph::new(Line::from(spans)), area);
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
            vec![live("gpu-01", vec![obs("jupyter", 8888, AttachStatus::Forwarded)])],
            &[],
        );
        let text = draw(&mut app, 90, 12);

        assert!(text.contains("gpu-01"), "machine row missing:\n{text}");
        assert!(text.contains("▾"), "expanded marker missing:\n{text}");
        assert!(text.contains("·1"), "forward count missing:\n{text}");
        assert!(text.contains("8888 → localhost:8888"), "forward row missing:\n{text}");
        assert!(text.contains("connected"), "session status missing:\n{text}");
        assert!(text.contains("forwarded"), "forward status missing:\n{text}");
        assert!(text.contains("●"), "status lamp missing:\n{text}");
        assert!(text.contains("└─"), "tree guide missing:\n{text}");
    }

    #[test]
    fn a_collapsed_machine_hides_forwards_but_keeps_its_own_row() {
        let mut app = app_with(
            vec![live("gpu-01", vec![obs("jupyter", 8888, AttachStatus::Forwarded)])],
            &[],
        );
        app.expanded.clear();
        app.rows = tree::flatten(&app.machines, &app.expanded);

        let text = draw(&mut app, 90, 12);
        assert!(text.contains("gpu-01"), "machine row vanished:\n{text}");
        assert!(text.contains("▸"), "collapsed marker missing:\n{text}");
        assert!(!text.contains("8888 → localhost"), "forward leaked while collapsed:\n{text}");
        // Folding must not cost information: the count still shows.
        assert!(text.contains("·1"), "forward count lost when collapsed:\n{text}");
    }

    #[test]
    fn idle_hosts_render_with_a_hollow_lamp_and_no_status_word() {
        let mut app = app_with(vec![], &["nas", "bastion"]);
        let text = draw(&mut app, 90, 12);

        assert!(text.contains("nas"), "idle host missing:\n{text}");
        assert!(text.contains("○"), "hollow lamp missing:\n{text}");
        // With most of a 12-host list idle, repeating "idle" on every row is
        // noise — the hollow lamp and the dimmed name carry it.
        assert!(!text.contains("idle"), "idle rows should stay quiet:\n{text}");
    }

    /// Foreground colour of the first cell of `needle`, or None if not found.
    fn fg_of(terminal: &Terminal<TestBackend>, needle: &str) -> Option<Color> {
        fg_of_from(terminal, needle, 0)
    }

    /// As `fg_of`, but skipping the first `min_y` rows — the title and header
    /// repeat words like "connected", and they are chrome, not data.
    fn fg_of_from(terminal: &Terminal<TestBackend>, needle: &str, min_y: u16) -> Option<Color> {
        let buf = terminal.backend().buffer();
        let first = needle.chars().next()?;
        for y in min_y..buf.area.height {
            for x in 0..buf.area.width {
                if buf[(x, y)].symbol().starts_with(first) {
                    let run: String = (x..buf.area.width.min(x + needle.len() as u16))
                        .map(|xx| buf[(xx, y)].symbol())
                        .collect();
                    if run == needle {
                        return Some(buf[(x, y)].fg);
                    }
                }
            }
        }
        None
    }

    #[test]
    fn the_menu_is_readable_and_its_keys_are_accented() {
        let mut app = app_with(vec![], &["nas"]);
        let mut terminal = Terminal::new(TestBackend::new(100, 10)).unwrap();
        terminal.draw(|f| render(f, &mut app)).unwrap();

        // Descriptions sit at the terminal's own foreground. Dimming them once
        // made the menu vanish on themes where bright-black ≈ background.
        let desc = fg_of(&terminal, "move").expect("menu not rendered");
        assert_eq!(desc, Color::Reset, "menu text is dimmed and can disappear");

        // The key itself is what the eye hunts for, so it keeps the accent.
        let key = fg_of(&terminal, "j/k").expect("menu key not rendered");
        assert_eq!(key, ACCENT, "menu keys lost their accent");
    }

    #[test]
    fn no_ansi_bright_black_anywhere() {
        // DarkGray is a shade off the background on plenty of themes. Nothing
        // in the tree should depend on it being legible.
        let mut f = obs("boom", 5432, AttachStatus::Failed);
        f.attached_at = None;
        f.error = Some("bind: Address already in use".to_string());
        let mut app = app_with(
            vec![live("gpu-01", vec![obs("a", 8888, AttachStatus::Forwarded), f])],
            &["nas", "bastion"],
        );

        let mut terminal = Terminal::new(TestBackend::new(100, 14)).unwrap();
        terminal.draw(|f| render(f, &mut app)).unwrap();

        let buf = terminal.backend().buffer();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                assert_ne!(
                    cell.fg,
                    Color::DarkGray,
                    "bright-black text at ({x},{y}): {:?}",
                    cell.symbol()
                );
            }
        }
    }

    #[test]
    fn state_words_carry_the_colour_of_their_lamp() {
        let mut f = obs("boom", 5432, AttachStatus::Failed);
        f.attached_at = None;
        f.error = Some("bind: Address already in use".to_string());

        let mut turing = live("turing", vec![obs("web", 8080, AttachStatus::Pending)]);
        turing.status = SessionStatus::Reconnecting;

        let mut app = app_with(
            vec![
                live("gpu-01", vec![obs("a", 8888, AttachStatus::Forwarded), f]),
                turing,
            ],
            &[],
        );
        let mut terminal = Terminal::new(TestBackend::new(100, 14)).unwrap();
        terminal.draw(|fr| render(fr, &mut app)).unwrap();

        // Row 0 is the title, row 1 the header — both say "connected".
        assert_eq!(fg_of_from(&terminal, "connected", 2), Some(OK));
        assert_eq!(fg_of_from(&terminal, "forwarded", 2), Some(OK));
        assert_eq!(fg_of_from(&terminal, "reconnecting", 2), Some(WARN));
        assert_eq!(fg_of_from(&terminal, "pending", 2), Some(WARN));
        assert_eq!(fg_of_from(&terminal, "failed", 2), Some(BAD));
    }

    #[test]
    fn a_lamp_and_its_word_can_never_disagree() {
        // Both come from one lookup, so a future edit cannot colour the glyph
        // green while the label says failed.
        for state in [
            Some(SessionStatus::Connected),
            Some(SessionStatus::Connecting),
            Some(SessionStatus::Reconnecting),
            Some(SessionStatus::Failed),
            None,
        ] {
            let (sym, word, color) = machine_look(state);
            assert!(!sym.is_empty(), "every state needs a lamp");
            if state.is_none() {
                assert!(word.is_empty(), "idle stays quiet");
            } else {
                assert!(!word.is_empty(), "{state:?} needs a word");
            }
            let _ = color;
        }
    }

    #[test]
    fn idle_hostnames_stay_readable_rather_than_dimmed() {
        // Dimming hostnames made a fully-disconnected screen — which is most of
        // pf's life — unreadable. Hierarchy comes from weight instead.
        let mut app = app_with(vec![], &["bastion", "nas"]);
        let mut terminal = Terminal::new(TestBackend::new(90, 10)).unwrap();
        terminal.draw(|f| render(f, &mut app)).unwrap();

        let fg = fg_of(&terminal, "bastion").expect("idle host not rendered");
        assert_ne!(fg, MUTE, "idle hostname is dimmed and hard to read");
    }

    #[test]
    fn forward_ports_stay_readable() {
        let mut app = app_with(
            vec![live("gpu-01", vec![obs("jupyter", 8888, AttachStatus::Forwarded)])],
            &[],
        );
        let mut terminal = Terminal::new(TestBackend::new(90, 10)).unwrap();
        terminal.draw(|f| render(f, &mut app)).unwrap();

        let fg = fg_of(&terminal, "localhost:8888").expect("forward not rendered");
        assert_ne!(fg, MUTE, "the port mapping is the payload; it must not be dim");
    }

    #[test]
    fn a_healthy_machine_does_not_display_a_zero_reconnect_count() {
        let mut app = app_with(vec![live("gpu-01", vec![])], &[]);
        let text = draw(&mut app, 90, 12);
        assert!(!text.contains("↻"), "zero reconnects should stay silent:\n{text}");
    }

    #[test]
    fn a_reconnect_count_appears_once_it_is_a_signal() {
        let mut s = live("gpu-01", vec![]);
        s.reconnect_count = 3;
        let mut app = app_with(vec![s], &[]);
        let text = draw(&mut app, 90, 12);
        assert!(text.contains("↻3"), "reconnect count missing:\n{text}");
    }

    #[test]
    fn the_title_counts_connected_machines() {
        let mut app = app_with(vec![live("gpu-01", vec![])], &["nas", "bastion"]);
        let text = draw(&mut app, 90, 12);
        assert!(text.contains("1 of 3 connected"), "title count missing:\n{text}");
    }

    #[test]
    fn ssh_errors_compress_to_something_that_fits_a_cell() {
        // The front half of ssh's message is all mechanism; the reason is what
        // the cell has room for.
        assert_eq!(
            short_error("bind [127.0.0.1]:5432: Address already in use"),
            "port in use"
        );
        assert_eq!(
            short_error("channel_setup_fwd_listener: Permission denied"),
            "denied"
        );
        assert_eq!(short_error("Cannot assign requested address"), "bad address");

        // Anything unrecognised still has to fit.
        let long = short_error("some entirely unexpected failure mode from ssh");
        assert!(long.chars().count() <= 13, "did not fit the cell: {long:?}");

        // Multi-line stderr collapses to its first line.
        assert_eq!(
            short_error("bind: Address already in use\nmore detail follows"),
            "port in use"
        );
    }

    #[test]
    fn a_failed_forward_shows_its_error_instead_of_an_uptime() {
        let mut f = obs("boom", 6006, AttachStatus::Failed);
        f.attached_at = None;
        f.error = Some("bind [127.0.0.1]:6006: Address already in use".to_string());

        let mut app = app_with(vec![live("gpu-01", vec![f])], &[]);
        let text = draw(&mut app, 100, 12);
        assert!(text.contains("failed"), "failed status missing:\n{text}");
        assert!(text.contains("✕"), "failure lamp missing:\n{text}");
        // The reason, not ssh's `bind [127.0.0.1]:6006:` preamble.
        assert!(text.contains("port in use"), "reason missing:\n{text}");
        assert!(!text.contains("127.0.0.1"), "raw ssh mechanism leaked:\n{text}");
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
        assert!(text.contains("/gpu"), "filter not shown:\n{text}");
    }

    #[test]
    fn a_filter_matching_nothing_says_so_and_says_how_to_undo_it() {
        let mut app = app_with(vec![live("gpu-01", vec![])], &[]);
        app.filter = "zzzz".to_string();
        app.machines.clear();
        app.rows.clear();

        let text = draw(&mut app, 90, 12);
        assert!(text.contains("No machine matches"), "empty state missing:\n{text}");
        assert!(text.contains("Esc"), "empty state gives no way out:\n{text}");
    }

    /// The disconnected screen — the state pf spends most of its life in, and
    /// the one that was unreadable when idle hosts were dimmed.
    /// `cargo test preview_idle -- --nocapture --ignored`
    #[test]
    #[ignore]
    fn preview_idle() {
        let mut app = app_with(
            vec![],
            &["bastion", "dev-box", "gpu-01", "gpu-02", "lovelace", "nas", "turing"],
        );
        app.select(2);

        let mut terminal = Terminal::new(TestBackend::new(76, 11)).unwrap();
        terminal.draw(|f| render(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        println!();
        for y in 0..buf.area.height {
            let line: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
            println!("{}", line.trim_end());
        }
        println!();
    }

    /// `cargo test preview_form -- --nocapture --ignored`
    #[test]
    #[ignore]
    fn preview_form() {
        for (label, setup) in [
            ("add to a known machine, on Remote Port", 0usize),
            ("add to a known machine, on Name", 1),
            ("connect to an unlisted machine", 2),
        ] {
            let mut app = app_with(vec![live("gpu-01", vec![])], &[]);
            match setup {
                0 => {
                    app.open_new_forward_form("gpu-01".to_string());
                    app.input_local_port = "8888".to_string();
                    app.input_field = InputField::RemotePort;
                }
                1 => {
                    app.open_new_forward_form("gpu-01".to_string());
                    app.input_local_port = "8888".to_string();
                    app.input_field = InputField::Name;
                }
                _ => app.open_new_machine_form(),
            }

            let mut terminal = Terminal::new(TestBackend::new(64, 26)).unwrap();
            terminal.draw(|f| render(f, &mut app)).unwrap();
            let buf = terminal.backend().buffer();
            println!("\n{label}");
            for y in 0..buf.area.height {
                let line: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
                let line = line.trim_end();
                if !line.is_empty() {
                    println!("{line}");
                }
            }
        }
        println!();
    }

    /// Not an assertion — prints the layout so it can be eyeballed.
    /// `cargo test preview_layout -- --nocapture --ignored`
    #[test]
    #[ignore]
    fn preview_layout() {
        let mut gpu1 = live(
            "gpu-01",
            vec![
                obs("jupyter", 8888, AttachStatus::Forwarded),
                obs("tensorboard", 6006, AttachStatus::Forwarded),
            ],
        );
        gpu1.reconnect_count = 1;

        let mut boom = obs("db", 5432, AttachStatus::Failed);
        boom.attached_at = None;
        boom.error = Some("bind [127.0.0.1]:5432: Address already in use".to_string());
        let gpu2 = live("gpu-02", vec![obs("api", 3000, AttachStatus::Forwarded), boom]);

        let mut reconnecting = live("turing", vec![obs("web", 8080, AttachStatus::Pending)]);
        reconnecting.status = SessionStatus::Reconnecting;
        reconnecting.connected_at = None;
        reconnecting.reconnect_count = 4;

        let mut app = app_with(
            vec![gpu1, gpu2, reconnecting],
            &["bastion", "nas", "dev-box"],
        );
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

    #[test]
    fn the_new_host_form_asks_for_a_host_in_free_text() {
        // The machine list cannot be the host picker for a host that is not in
        // it — an IP, a user@host, or anything behind a wildcard in ssh config.
        let mut app = app_with(vec![], &["nas"]);
        app.open_new_machine_form();

        let text = draw(&mut app, 90, 24);
        assert!(text.contains("Connect to a machine"), "wrong title:\n{text}");
        assert!(text.contains("Host"), "host field missing:\n{text}");
        assert!(
            text.contains("user@host"),
            "no hint that free text is expected:\n{text}"
        );
    }

    #[test]
    fn the_host_field_only_exists_in_the_new_host_form() {
        let mut app = app_with(vec![live("gpu-01", vec![])], &[]);

        app.open_new_machine_form();
        assert_eq!(app.form_fields().len(), 4, "new-host form should ask for a host");
        assert_eq!(app.input_field, InputField::Host, "should start on the host");

        app.open_new_forward_form("gpu-01".to_string());
        assert_eq!(app.form_fields().len(), 3, "adding to a known machine should not");
        assert_eq!(app.input_field, InputField::LocalPort);
    }

    #[test]
    fn tabbing_wraps_within_whichever_fields_the_form_has() {
        let mut app = app_with(vec![live("gpu-01", vec![])], &[]);

        app.open_new_forward_form("gpu-01".to_string());
        app.next_field();
        app.next_field();
        assert_eq!(app.input_field, InputField::Name);
        assert!(app.on_last_field(), "Name is last when there is no host field");
        app.next_field();
        assert_eq!(app.input_field, InputField::LocalPort, "should skip Host");

        app.open_new_machine_form();
        app.prev_field();
        assert_eq!(app.input_field, InputField::Name, "back from Host wraps to Name");
    }

    #[test]
    fn the_form_shows_what_an_empty_remote_port_will_become() {
        let mut app = app_with(vec![live("gpu-01", vec![])], &[]);
        app.open_new_forward_form("gpu-01".to_string());
        app.input_local_port = "8888".to_string();

        let text = draw(&mut app, 90, 24);
        assert!(
            text.contains("same as local"),
            "the remote-port default is invisible:\n{text}"
        );
        assert!(
            text.contains("gpu-01-8888"),
            "the generated name is invisible:\n{text}"
        );
    }

    #[test]
    fn the_cursor_sits_after_the_value_a_field_will_submit() {
        let mut app = app_with(vec![live("gpu-01", vec![])], &[]);
        app.open_new_forward_form("gpu-01".to_string());
        app.input_local_port = "8888".to_string();

        // On Remote Port, which is empty and will default to the local port.
        app.input_field = InputField::RemotePort;
        let text = draw(&mut app, 90, 24);
        assert!(
            text.contains("8888_  same as local"),
            "cursor should follow the defaulted value, not precede it:\n{text}"
        );

        // Same rule for the generated name.
        app.input_field = InputField::Name;
        let text = draw(&mut app, 90, 24);
        assert!(
            text.contains("gpu-01-8888_"),
            "cursor should follow the generated name:\n{text}"
        );

        // And for a field the user has actually typed into.
        app.input_field = InputField::RemotePort;
        app.input_remote_port = "80".to_string();
        let text = draw(&mut app, 90, 24);
        assert!(text.contains("80_"), "cursor should follow typed input:\n{text}");
        assert!(
            !text.contains("same as local"),
            "the default note should go once a value is typed:\n{text}"
        );
    }
}

use super::tree::{self, MachineListMode, MachineRow, Row, Sel};
use ratatui::widgets::{ListState, TableState};
use std::collections::{BTreeSet, HashSet};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc;

/// Write `text` to the system clipboard via the platform's own tool, so pf
/// carries no clipboard dependency.
fn system_clipboard(text: &str) -> Result<(), String> {
    let candidates: &[&[&str]] = if cfg!(target_os = "macos") {
        &[&["pbcopy"]]
    } else {
        // Wayland first; fall back to X11.
        &[&["wl-copy"], &["xclip", "-selection", "clipboard"]]
    };

    for cmd in candidates {
        let child = Command::new(cmd[0])
            .args(&cmd[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else { continue };
        if let Some(stdin) = child.stdin.as_mut() {
            if stdin.write_all(text.as_bytes()).is_err() {
                continue;
            }
        }
        if child.wait().is_ok_and(|s| s.success()) {
            return Ok(());
        }
    }
    Err("No clipboard tool found".to_string())
}

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Normal,
    Logs,
    NewForward,
    ProfilePicker,
    Filter,
    Confirm(ConfirmAction),
    /// The full key list; the one-line menu only has room for the essentials.
    Help,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConfirmAction {
    /// Stop one forward.
    StopForward(String),
    /// Stop every forward on a host, taking the session down.
    StopHost(String, usize),
    RestartForward(String),
    /// Drop the master and let it reconnect.
    RestartHost(String),
}

/// `a` on a machine row already knows the host, so the form skips that field.
/// `A` connects to a machine that is not listed, and then it is the only field
/// that matters — hence a form that can include it or not.
#[derive(Debug, Clone, PartialEq)]
pub enum InputField {
    Host,
    LocalPort,
    RemotePort,
    Name,
}

impl InputField {
    pub fn label(&self) -> &str {
        match self {
            InputField::Host => "Host",
            InputField::LocalPort => "Local Port",
            InputField::RemotePort => "Remote Port",
            InputField::Name => "Name",
        }
    }
}

pub struct AppState {
    pub mode: Mode,

    // Tree
    pub machines: Vec<MachineRow>,
    pub rows: Vec<Row>,
    pub expanded: HashSet<String>,
    pub table_state: TableState,
    /// Identity-keyed selection, so a refresh cannot fling the cursor around.
    pub sel: Option<Sel>,
    pub machine_source: MachineListMode,
    pub filter: String,
    /// Rows the tree viewport held at the last render; the paging distance.
    pub tree_visible: usize,
    /// True until the first refresh, which seeds the fold state.
    first_refresh: bool,

    pub profiles: Vec<(String, crate::config::Profile)>,
    pub profile_state: ListState,
    pub should_quit: bool,

    // Log viewer
    pub log_lines: Vec<String>,
    pub log_scroll: usize,
    pub log_name: String,
    /// Pinned to the tail, `tail -f` style, until the user scrolls up.
    pub log_follow: bool,
    /// Lines the log viewport held at the last render.
    pub log_visible: usize,

    // New forward form
    pub input_field: InputField,
    /// The machine `a` was pressed on, or free text when `A` asked for one.
    pub input_host: String,
    /// Whether the form includes the Host field at all.
    pub input_asks_host: bool,
    pub input_local_port: String,
    pub input_remote_port: String,
    pub input_name: String,

    pub ssh_hosts: Vec<String>,
    pub status_message: Option<String>,

    /// Seam for `y`: the system clipboard in production, a recorder in tests.
    pub clipboard: Box<dyn Fn(&str) -> Result<(), String>>,

    // Background actions: stops, restarts, and starts run off the UI thread
    // and report back through a channel.
    pub pending_actions: usize,
    /// What the most recent in-flight action is doing, for the status bar.
    pub pending_label: Option<String>,
    action_tx: mpsc::Sender<ActionDone>,
    action_rx: mpsc::Receiver<ActionDone>,
}

/// What a finished background action sends home: its outcome message, and
/// optionally the (host, forward) to point the cursor at.
struct ActionDone {
    message: String,
    follow: Option<(String, String)>,
}

impl AppState {
    pub fn new() -> Self {
        let ssh_hosts = crate::ssh_hosts::parse_ssh_hosts();
        let machine_source = crate::config::Config::load()
            .map(|c| c.tui.machine_source)
            .unwrap_or_default();
        let (action_tx, action_rx) = mpsc::channel();

        Self {
            mode: Mode::Normal,
            machines: Vec::new(),
            rows: Vec::new(),
            expanded: HashSet::new(),
            table_state: TableState::new().with_selected(Some(0)),
            sel: None,
            machine_source,
            filter: String::new(),
            tree_visible: 20,
            first_refresh: true,
            profiles: Vec::new(),
            profile_state: ListState::default().with_selected(Some(0)),
            should_quit: false,
            log_lines: Vec::new(),
            log_scroll: 0,
            log_name: String::new(),
            log_follow: true,
            log_visible: 20,
            input_field: InputField::LocalPort,
            input_host: String::new(),
            input_asks_host: false,
            input_local_port: String::new(),
            input_remote_port: String::new(),
            input_name: String::new(),
            ssh_hosts,
            status_message: None,
            clipboard: Box::new(system_clipboard),
            pending_actions: 0,
            pending_label: None,
            action_tx,
            action_rx,
        }
    }

    pub fn run_action<F>(&mut self, label: &str, f: F)
    where
        F: FnOnce() -> Result<String, String> + Send + 'static,
    {
        self.run_action_following(label, None, f);
    }

    /// Run `f` off the UI thread — ssh takes as long as it takes, and the
    /// screen must not freeze for it. The outcome lands via `poll_actions`.
    pub fn run_action_following<F>(
        &mut self,
        label: &str,
        follow: Option<(String, String)>,
        f: F,
    ) where
        F: FnOnce() -> Result<String, String> + Send + 'static,
    {
        self.pending_actions += 1;
        self.pending_label = Some(label.to_string());
        let tx = self.action_tx.clone();
        std::thread::spawn(move || {
            // Success and failure are both just a message for the status bar.
            let message = match f() {
                Ok(m) | Err(m) => m,
            };
            let _ = tx.send(ActionDone { message, follow });
        });
    }

    /// Drain finished background actions. Returns true if any reported back.
    pub fn poll_actions(&mut self) -> bool {
        let mut any = false;
        while let Ok(done) = self.action_rx.try_recv() {
            any = true;
            self.pending_actions = self.pending_actions.saturating_sub(1);
            self.status_message = Some(done.message);
            self.refresh();
            if let Some((host, name)) = done.follow {
                self.select_forward(&host, &name);
            }
        }
        if any && self.pending_actions == 0 {
            self.pending_label = None;
        }
        any
    }

    pub fn selected(&self) -> usize {
        self.table_state.selected().unwrap_or(0)
    }

    pub fn select(&mut self, idx: usize) {
        self.table_state.select(Some(idx));
        self.sel = tree::sel_at(&self.rows, &self.machines, idx);
    }

    pub fn select_next(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let next = (self.selected() + 1) % self.rows.len();
        self.select(next);
    }

    pub fn select_prev(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() - 1;
        let prev = self.selected().checked_sub(1).unwrap_or(last);
        self.select(prev);
    }

    /// Jump or page by `delta` rows, pinning to the ends. Wrap-around is fine
    /// one row at a time but disorienting on a page jump.
    pub fn select_by(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() as isize - 1;
        let target = (self.selected() as isize + delta).clamp(0, last);
        self.select(target as usize);
    }

    pub fn select_first(&mut self) {
        if !self.rows.is_empty() {
            self.select(0);
        }
    }

    pub fn select_last(&mut self) {
        if !self.rows.is_empty() {
            self.select(self.rows.len() - 1);
        }
    }

    pub fn selected_sel(&self) -> Option<Sel> {
        tree::sel_at(&self.rows, &self.machines, self.selected())
    }

    /// The machine under the cursor, whichever row kind is selected.
    pub fn selected_machine(&self) -> Option<&MachineRow> {
        let host = self.selected_sel()?;
        self.machines.iter().find(|m| m.host == host.host())
    }

    pub fn toggle_expand(&mut self) {
        let Some(sel) = self.selected_sel() else {
            return;
        };
        let host = sel.host().to_string();
        if self.expanded.contains(&host) {
            self.expanded.remove(&host);
        } else {
            self.expanded.insert(host);
        }
        self.rebuild_rows();
    }

    pub fn expand_selected(&mut self) {
        if let Some(sel) = self.selected_sel() {
            self.expanded.insert(sel.host().to_string());
            self.rebuild_rows();
        }
    }

    /// Collapse the machine, or jump to the parent machine from a forward row.
    pub fn collapse_selected(&mut self) {
        let Some(sel) = self.selected_sel() else {
            return;
        };
        match sel {
            Sel::Forward(host, _) => {
                self.sel = Some(Sel::Machine(host));
                self.rebuild_rows();
            }
            Sel::Machine(host) => {
                self.expanded.remove(&host);
                self.rebuild_rows();
            }
        }
    }

    pub fn collapse_all(&mut self) {
        self.expanded.clear();
        if let Some(sel) = self.selected_sel() {
            self.sel = Some(Sel::Machine(sel.host().to_string()));
        }
        self.rebuild_rows();
    }

    pub fn cycle_machine_source(&mut self) {
        self.machine_source = self.machine_source.cycle();
        if let Ok(mut config) = crate::config::Config::load() {
            config.tui.machine_source = self.machine_source;
            let _ = config.save();
        }
        self.status_message = Some(format!("Showing {}", self.machine_source.label()));
        self.refresh();
    }

    /// Re-derive the flat rows and re-find the selection by identity.
    fn rebuild_rows(&mut self) {
        self.rows = tree::flatten(&self.machines, &self.expanded);
        let idx = match &self.sel {
            Some(sel) => tree::resolve(&self.rows, &self.machines, sel),
            None => 0,
        };
        self.table_state.select(Some(idx));
        self.sel = tree::sel_at(&self.rows, &self.machines, idx);
    }

    /// Rebuild the whole tree from disk.
    pub fn refresh(&mut self) {
        let sessions = crate::session::store::list_states().unwrap_or_default();
        let profile_hosts: BTreeSet<String> = self
            .profiles
            .iter()
            .map(|(_, p)| p.host.clone())
            .collect();

        self.machines = tree::build_machines(
            sessions,
            &profile_hosts,
            &self.ssh_hosts,
            self.machine_source,
            &self.filter,
        );

        if self.first_refresh {
            self.expanded = tree::default_expanded(&self.machines);
            self.first_refresh = false;
        } else {
            // A machine that has come up since the last refresh opens itself,
            // so starting a forward shows it rather than hiding it in a fold.
            for machine in &self.machines {
                if machine.is_live() && !machine.forwards.is_empty() {
                    self.expanded.insert(machine.host.clone());
                }
            }
        }

        self.rebuild_rows();
    }

    pub fn refresh_profiles(&mut self) {
        if let Ok(config) = crate::config::Config::load() {
            self.profiles = config.profiles.into_iter().collect();
        }
    }

    pub fn profile_selected(&self) -> usize {
        self.profile_state.selected().unwrap_or(0)
    }

    pub fn select_profile(&mut self, idx: usize) {
        self.profile_state.select(Some(idx));
    }

    pub fn select_next_profile(&mut self) {
        if self.profiles.is_empty() {
            return;
        }
        let next = (self.profile_selected() + 1) % self.profiles.len();
        self.select_profile(next);
    }

    pub fn select_prev_profile(&mut self) {
        if self.profiles.is_empty() {
            return;
        }
        let last = self.profiles.len() - 1;
        let prev = self.profile_selected().checked_sub(1).unwrap_or(last);
        self.select_profile(prev);
    }

    /// Point the selection at a forward by name, wherever it landed.
    pub fn select_forward(&mut self, host: &str, name: &str) {
        self.expanded.insert(host.to_string());
        self.sel = Some(Sel::Forward(host.to_string(), name.to_string()));
        self.rebuild_rows();
    }

    fn clear_form(&mut self) {
        self.input_local_port.clear();
        self.input_remote_port.clear();
        self.input_name.clear();
        self.mode = Mode::NewForward;
    }

    /// `a` — add a forward to the machine under the cursor.
    pub fn open_new_forward_form(&mut self, host: String) {
        self.input_host = host;
        self.input_asks_host = false;
        self.input_field = InputField::LocalPort;
        self.clear_form();
    }

    /// `A` — connect to a machine that is not in the list. Free text, because
    /// the whole point is that this host is outside the set we could complete
    /// against.
    pub fn open_new_machine_form(&mut self) {
        self.input_host.clear();
        self.input_asks_host = true;
        self.input_field = InputField::Host;
        self.clear_form();
    }

    /// The fields this form actually shows, in tab order.
    pub fn form_fields(&self) -> Vec<InputField> {
        let mut fields = Vec::new();
        if self.input_asks_host {
            fields.push(InputField::Host);
        }
        fields.extend([
            InputField::LocalPort,
            InputField::RemotePort,
            InputField::Name,
        ]);
        fields
    }

    pub fn next_field(&mut self) {
        let fields = self.form_fields();
        let i = fields.iter().position(|f| *f == self.input_field).unwrap_or(0);
        self.input_field = fields[(i + 1) % fields.len()].clone();
    }

    pub fn prev_field(&mut self) {
        let fields = self.form_fields();
        let i = fields.iter().position(|f| *f == self.input_field).unwrap_or(0);
        self.input_field = fields[(i + fields.len() - 1) % fields.len()].clone();
    }

    /// True when the cursor is on the last field, so Enter submits.
    pub fn on_last_field(&self) -> bool {
        self.form_fields().last() == Some(&self.input_field)
    }

    pub fn current_input(&mut self) -> &mut String {
        match self.input_field {
            InputField::Host => &mut self.input_host,
            InputField::LocalPort => &mut self.input_local_port,
            InputField::RemotePort => &mut self.input_remote_port,
            InputField::Name => &mut self.input_name,
        }
    }

    /// The furthest the log can scroll: tail line at the bottom of the view.
    fn log_max_scroll(&self) -> usize {
        self.log_lines.len().saturating_sub(self.log_visible)
    }

    /// Replace the log content, `tail -f` style: pinned to the tail while
    /// following, and holding the reader's place once they have scrolled up —
    /// the tick reload must never steal the position.
    pub fn set_log_lines(&mut self, lines: Vec<String>) {
        self.log_lines = lines;
        if self.log_follow {
            self.log_scroll = self.log_max_scroll();
        } else {
            self.log_scroll = self.log_scroll.min(self.log_max_scroll());
        }
    }

    /// Scrolling up is what breaks tail-following.
    pub fn log_scroll_up(&mut self, n: usize) {
        self.log_follow = false;
        self.log_scroll = self.log_scroll.saturating_sub(n);
    }

    /// Reaching the bottom resumes it.
    pub fn log_scroll_down(&mut self, n: usize) {
        self.log_scroll = (self.log_scroll + n).min(self.log_max_scroll());
        if self.log_scroll == self.log_max_scroll() {
            self.log_follow = true;
        }
    }

    pub fn log_to_top(&mut self) {
        self.log_follow = false;
        self.log_scroll = 0;
    }

    pub fn log_to_bottom(&mut self) {
        self.log_follow = true;
        self.log_scroll = self.log_max_scroll();
    }

    /// `l` — enter the log viewer at the tail.
    pub fn open_logs(&mut self, host: &str) {
        self.log_follow = true;
        self.load_logs(host);
        self.mode = Mode::Logs;
    }

    pub fn load_logs(&mut self, host: &str) {
        self.log_name = host.to_string();
        let mut lines = Vec::new();
        if let Ok(path) = crate::paths::session_log_file(&crate::paths::sanitize_host(host)) {
            if let Ok(content) = std::fs::read_to_string(path) {
                lines = content.lines().map(|l| l.to_string()).collect();
            }
        }
        self.set_log_lines(lines);
    }
}

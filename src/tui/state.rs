use super::tree::{self, MachineListMode, MachineRow, Row, Sel};
use ratatui::widgets::{ListState, TableState};
use std::collections::{BTreeSet, HashSet};

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Normal,
    Logs,
    NewForward,
    ProfilePicker,
    Filter,
    Confirm(ConfirmAction),
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

/// The new-forward form no longer asks for a host: `a` is pressed on a machine
/// row, so the host is already known.
#[derive(Debug, Clone, PartialEq)]
pub enum InputField {
    LocalPort,
    RemotePort,
    Name,
}

impl InputField {
    pub fn next(&self) -> Self {
        match self {
            InputField::LocalPort => InputField::RemotePort,
            InputField::RemotePort => InputField::Name,
            InputField::Name => InputField::LocalPort,
        }
    }

    pub fn prev(&self) -> Self {
        match self {
            InputField::LocalPort => InputField::Name,
            InputField::RemotePort => InputField::LocalPort,
            InputField::Name => InputField::RemotePort,
        }
    }

    pub fn label(&self) -> &str {
        match self {
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
    /// True until the first refresh, which seeds the fold state.
    first_refresh: bool,

    pub profiles: Vec<(String, crate::config::Profile)>,
    pub profile_state: ListState,
    pub should_quit: bool,

    // Log viewer
    pub log_lines: Vec<String>,
    pub log_scroll: usize,
    pub log_name: String,

    // New forward form
    pub input_field: InputField,
    /// The machine `a` was pressed on.
    pub input_host: String,
    pub input_local_port: String,
    pub input_remote_port: String,
    pub input_name: String,

    pub ssh_hosts: Vec<String>,
    pub status_message: Option<String>,
}

impl AppState {
    pub fn new() -> Self {
        let ssh_hosts = crate::ssh_hosts::parse_ssh_hosts();
        let machine_source = crate::config::Config::load()
            .map(|c| c.tui.machine_source)
            .unwrap_or_default();

        Self {
            mode: Mode::Normal,
            machines: Vec::new(),
            rows: Vec::new(),
            expanded: HashSet::new(),
            table_state: TableState::new().with_selected(Some(0)),
            sel: None,
            machine_source,
            filter: String::new(),
            first_refresh: true,
            profiles: Vec::new(),
            profile_state: ListState::default().with_selected(Some(0)),
            should_quit: false,
            log_lines: Vec::new(),
            log_scroll: 0,
            log_name: String::new(),
            input_field: InputField::LocalPort,
            input_host: String::new(),
            input_local_port: String::new(),
            input_remote_port: String::new(),
            input_name: String::new(),
            ssh_hosts,
            status_message: None,
        }
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

    pub fn open_new_forward_form(&mut self, host: String) {
        self.input_host = host;
        self.input_local_port.clear();
        self.input_remote_port.clear();
        self.input_name.clear();
        self.input_field = InputField::LocalPort;
        self.mode = Mode::NewForward;
    }

    pub fn current_input(&mut self) -> &mut String {
        match self.input_field {
            InputField::LocalPort => &mut self.input_local_port,
            InputField::RemotePort => &mut self.input_remote_port,
            InputField::Name => &mut self.input_name,
        }
    }

    pub fn load_logs(&mut self, host: &str) {
        self.log_name = host.to_string();
        self.log_lines.clear();
        self.log_scroll = 0;
        if let Ok(path) = crate::paths::session_log_file(&crate::paths::sanitize_host(host)) {
            if let Ok(content) = std::fs::read_to_string(path) {
                self.log_lines = content.lines().map(|l| l.to_string()).collect();
                if self.log_lines.len() > 20 {
                    self.log_scroll = self.log_lines.len().saturating_sub(20);
                }
            }
        }
    }
}

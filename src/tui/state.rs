use crate::state::ForwardState;

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Normal,
    Logs,
    NewForward,
    ProfilePicker,
    Confirm(ConfirmAction),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConfirmAction {
    Stop(String),
    Restart(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum InputField {
    Host,
    LocalPort,
    RemotePort,
    Name,
}

impl InputField {
    pub fn next(&self) -> Self {
        match self {
            InputField::Host => InputField::LocalPort,
            InputField::LocalPort => InputField::RemotePort,
            InputField::RemotePort => InputField::Name,
            InputField::Name => InputField::Host,
        }
    }

    pub fn prev(&self) -> Self {
        match self {
            InputField::Host => InputField::Name,
            InputField::LocalPort => InputField::Host,
            InputField::RemotePort => InputField::LocalPort,
            InputField::Name => InputField::RemotePort,
        }
    }

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
    pub forwards: Vec<ForwardState>,
    pub selected: usize,
    pub profiles: Vec<(String, crate::config::Profile)>,
    pub profile_selected: usize,
    pub should_quit: bool,

    // Log viewer
    pub log_lines: Vec<String>,
    pub log_scroll: usize,
    pub log_name: String,

    // New forward form
    pub input_field: InputField,
    pub input_host: String,
    pub input_local_port: String,
    pub input_remote_port: String,
    pub input_name: String,

    // SSH host autocomplete
    pub ssh_hosts: Vec<String>,
    pub host_suggestions: Vec<String>,
    pub host_suggestion_idx: Option<usize>,

    // Status message
    pub status_message: Option<String>,
}

impl AppState {
    pub fn new() -> Self {
        let ssh_hosts = crate::ssh_hosts::parse_ssh_hosts();
        Self {
            mode: Mode::Normal,
            forwards: Vec::new(),
            selected: 0,
            profiles: Vec::new(),
            profile_selected: 0,
            should_quit: false,
            log_lines: Vec::new(),
            log_scroll: 0,
            log_name: String::new(),
            input_field: InputField::Host,
            input_host: String::new(),
            input_local_port: String::new(),
            input_remote_port: String::new(),
            input_name: String::new(),
            ssh_hosts,
            host_suggestions: Vec::new(),
            host_suggestion_idx: None,
            status_message: None,
        }
    }

    pub fn selected_name(&self) -> Option<String> {
        self.forwards.get(self.selected).map(|f| f.name.clone())
    }

    pub fn refresh_forwards(&mut self) {
        if let Ok(fwds) = crate::state::ForwardState::list_all() {
            let prev_selected = self.selected_name();
            self.forwards = fwds;
            // Try to preserve selection
            if let Some(name) = prev_selected {
                if let Some(idx) = self.forwards.iter().position(|f| f.name == name) {
                    self.selected = idx;
                }
            }
            if self.selected >= self.forwards.len() && !self.forwards.is_empty() {
                self.selected = self.forwards.len() - 1;
            }
        }
    }

    pub fn refresh_profiles(&mut self) {
        if let Ok(config) = crate::config::Config::load() {
            self.profiles = config.profiles.into_iter().collect();
        }
    }

    pub fn clear_input_form(&mut self) {
        self.input_host.clear();
        self.input_local_port.clear();
        self.input_remote_port.clear();
        self.input_name.clear();
        self.input_field = InputField::Host;
        self.host_suggestions.clear();
        self.host_suggestion_idx = None;
    }

    pub fn update_host_suggestions(&mut self) {
        let prefix = &self.input_host;
        self.host_suggestions = self
            .ssh_hosts
            .iter()
            .filter(|h| h.starts_with(prefix.as_str()))
            .cloned()
            .collect();
        self.host_suggestion_idx = None;
    }

    pub fn cycle_host_suggestion(&mut self) {
        if self.host_suggestions.is_empty() {
            return;
        }
        let idx = match self.host_suggestion_idx {
            None => 0,
            Some(i) => (i + 1) % self.host_suggestions.len(),
        };
        self.host_suggestion_idx = Some(idx);
        self.input_host = self.host_suggestions[idx].clone();
    }

    pub fn current_input(&mut self) -> &mut String {
        match self.input_field {
            InputField::Host => &mut self.input_host,
            InputField::LocalPort => &mut self.input_local_port,
            InputField::RemotePort => &mut self.input_remote_port,
            InputField::Name => &mut self.input_name,
        }
    }

    pub fn load_logs(&mut self, name: &str) {
        self.log_name = name.to_string();
        self.log_lines.clear();
        self.log_scroll = 0;
        if let Ok(path) = crate::paths::log_file(name) {
            if let Ok(content) = std::fs::read_to_string(path) {
                self.log_lines = content.lines().map(|l| l.to_string()).collect();
                // Scroll to bottom
                if self.log_lines.len() > 20 {
                    self.log_scroll = self.log_lines.len().saturating_sub(20);
                }
            }
        }
    }
}

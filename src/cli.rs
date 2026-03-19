use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(name = "pf", about = "SSH Port Forward Manager", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Start a port forward (profile name or host + ports)
    Start {
        /// Profile name or SSH host
        name_or_host: String,

        /// Port mapping (LOCAL:REMOTE), e.g. 8080:80
        ports: Option<String>,

        /// Name for ad-hoc forwards
        #[arg(long)]
        name: Option<String>,

        /// Disable auto-reconnect
        #[arg(long)]
        no_reconnect: bool,

        /// Max reconnect attempts (0 = unlimited)
        #[arg(long, default_value = "0")]
        max_retries: u32,

        /// Delay between retries in seconds
        #[arg(long, default_value = "5")]
        retry_delay: u64,
    },

    /// Stop a running forward
    Stop {
        /// Forward name
        name: Option<String>,

        /// Stop all forwards
        #[arg(long)]
        all: bool,
    },

    /// List all forwards
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Restart a forward
    Restart {
        /// Forward name
        name: Option<String>,

        /// Restart all forwards
        #[arg(long)]
        all: bool,
    },

    /// View watcher log
    Logs {
        /// Forward name
        name: String,

        /// Follow (tail) the log
        #[arg(short, long)]
        follow: bool,
    },

    /// Manage saved profiles
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },

    /// Remove stale state files
    Clean,

    /// Launch interactive TUI dashboard
    Tui,

    /// List SSH hosts from ~/.ssh/config
    Hosts,

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: ShellType,
    },

    /// Internal: provide dynamic completions for shell (hidden)
    #[command(hide = true)]
    Complete {
        /// The subcommand being completed
        #[arg(long)]
        subcommand: String,
        /// Current word prefix
        #[arg(long, default_value = "")]
        prefix: String,
    },

    /// Internal: run as watcher daemon (hidden)
    #[command(hide = true)]
    Watcher {
        #[arg(long)]
        name: String,
        #[arg(long)]
        host: String,
        #[arg(long)]
        local_port: u16,
        #[arg(long)]
        remote_port: u16,
        #[arg(long)]
        remote_host: Option<String>,
        #[arg(long)]
        reconnect: bool,
        #[arg(long, default_value = "0")]
        max_retries: u32,
        #[arg(long, default_value = "5")]
        retry_delay: u64,
    },
}

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Save a new profile
    Add {
        /// Profile name
        name: String,
        /// SSH host
        host: String,
        /// Port mapping (LOCAL:REMOTE)
        ports: String,
    },

    /// Remove a saved profile
    Remove {
        /// Profile name
        name: String,
    },

    /// List saved profiles
    List,
}

#[derive(Clone, ValueEnum)]
pub enum ShellType {
    Bash,
    Zsh,
    Fish,
}


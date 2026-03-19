use thiserror::Error;

#[derive(Error, Debug)]
pub enum PfError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("TOML deserialization error: {0}")]
    TomlDeser(#[from] toml::de::Error),

    #[error("TOML serialization error: {0}")]
    TomlSer(#[from] toml::ser::Error),

    #[error("Forward '{0}' is already running")]
    AlreadyRunning(String),

    #[error("Forward '{0}' not found")]
    NotFound(String),

    #[error("Port {0} is already in use")]
    PortInUse(u16),

    #[error("Profile '{0}' not found")]
    ProfileNotFound(String),

    #[error("Profile '{0}' already exists")]
    ProfileExists(String),

    #[error("Invalid port mapping: {0}")]
    InvalidPortMapping(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, PfError>;

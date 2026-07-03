//! Error types for lim-config.

use thiserror::Error;

pub type ConfigResult<T> = std::result::Result<T, ConfigError>;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file {path}: {source}")]
    Read {
        path: camino::Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse config file {path}: {source}")]
    Parse {
        path: camino::Utf8PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("home directory not found (HOME env not set)")]
    HomeNotFound,

    #[error("workspace root does not exist: {0}")]
    WorkspaceNotFound(camino::Utf8PathBuf),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

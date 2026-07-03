//! Error types for lim-skills.

use thiserror::Error;

pub type SkillResult<T> = std::result::Result<T, SkillError>;

#[derive(Debug, Error)]
pub enum SkillError {
    #[error("failed to read skill file {path}: {source}")]
    Read {
        path: camino::Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse skill frontmatter in {path}: {source}")]
    FrontmatterParse {
        path: camino::Utf8PathBuf,
        #[source]
        source: serde_yaml::Error,
    },

    #[error("skill file {path} has no frontmatter (expected `---` delimiters)")]
    NoFrontmatter { path: camino::Utf8PathBuf },

    #[error("skill file {path} has empty name in frontmatter")]
    EmptyName { path: camino::Utf8PathBuf },

    #[error("skill `{name}` not found")]
    NotFound { name: String },

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

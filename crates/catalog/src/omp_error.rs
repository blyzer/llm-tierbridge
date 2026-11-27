//! Failures of the omp integration (`export omp`, `apply omp`, `snapshot omp-live`). Like
//! [`crate::CatalogError`], anything here aborts the run and nothing is written.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum OmpError {
    #[error("catalog is not a JSON array of models: {0}")]
    Catalog(String),
    #[error(
        "the catalog has no rows of `omp-live:{provider}`: build it with `--source omp-live:{provider}` and `--merge`"
    )]
    NoRows { provider: String },
    #[error("--current {}: {reason}", path.display())]
    Current { path: PathBuf, reason: String },
    #[error("--current holds a roster of `{provider}`, which is not one of the --provider values")]
    CurrentProviderNotExported { provider: String },
    #[error("two --current files are rosters of `{provider}`")]
    CurrentDuplicate { provider: String },
    #[error("overrides file: {0}")]
    Overrides(String),
    #[error("models.yml: {0}")]
    ModelsYml(String),
    #[error("{0}")]
    Snapshot(String),
    #[error("i/o error on {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl OmpError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

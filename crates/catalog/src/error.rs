//! Structural failures: anything here aborts a run and nothing is written.

use std::path::PathBuf;

/// A run-level failure. Value-level problems are [`crate::Diagnostic`]s, not errors.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("source is not valid JSON (truncated or corrupt?): {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("top level of the source is not an object of providers")]
    NotAnObject,
    #[error("provider `{provider}` is not an object")]
    ProviderNotObject { provider: String },
    #[error("provider `{provider}` has no `models` object")]
    MissingModels { provider: String },
    #[error("model `{provider}/{id}` is not an object")]
    ModelNotObject { provider: String, id: String },
    /// The snapshot does not have the shape the adapter needs (modelparams.dev).
    #[error("malformed source: {0}")]
    Malformed(String),
    /// The catalog does not hold what an export needs.
    #[error("export: {0}")]
    Export(String),
    #[error("source contains zero models")]
    NoModels,
    #[error("source contains {found} models, fewer than the required minimum of {min}")]
    BelowMinModels { found: usize, min: usize },
    #[error("{count} value(s) were rejected and strict mode is on")]
    StrictRejections { count: usize },
    #[error("invalid timestamp `{value}`: {reason}")]
    InvalidTimestamp { value: String, reason: &'static str },
    #[error("i/o error on {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

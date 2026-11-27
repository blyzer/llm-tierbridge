//! Normalized model catalog: source adapters (models.dev, modelparams.dev) -> observations ->
//! single-source `model[]`.
//!
//! The wire shapes mirror `schema/gateway-model.schema.json`; the adapters are ports of the Python
//! prototype that report values outside the contract as [`Diagnostic`]s.

mod build;
pub mod cliproxyapi_cache;
mod diagnostics;
pub mod dsh;
mod error;
pub mod export_cpa;
pub mod freellmapi;
mod gateway;
pub mod matching;
mod merge;
pub mod merge_catalog;
pub mod model;
pub mod modelparams;
pub mod models_dev;
pub mod omp_bundled;
mod omp_error;
pub mod omp_export;
pub mod omp_live;
pub mod omp_models_yml;
mod output;
pub mod overrides;
pub mod placeholder;
pub mod precedence;
pub mod router_models;
pub mod snapshot;
pub mod time;

pub use build::{
    BuildOptions, BuildOutput, Counts, FetchMeta, Report, ReportSource, Source, SourceInput,
    build_cliproxyapi_cache, build_modelparams, build_models_dev, build_omp_bundled,
    build_omp_live,
};
pub use diagnostics::{Diagnostic, Findings, SentinelMapping};
pub use error::CatalogError;
pub use merge::{SINGLE_SOURCE_RULE, single_source, single_source_all};
pub use merge_catalog::{
    MergeDiagnostic, MergeOutput, MergeReport, SourceRole, merge_catalog, merge_catalog_with,
};
pub use omp_error::OmpError;
pub use output::{
    MERGE_REPORT_FILE, MERGED_CATALOG_FILE, WrittenFile, catalog_file, observations_file,
    report_file, write_merge, write_outputs,
};

//! One catalog build: adapt a source, check structural minimums, derive `model[]`, assemble the report.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::diagnostics::{Diagnostic, SentinelMapping};
use crate::dsh;
use crate::freellmapi;
use crate::merge::single_source_all;
use crate::model::{Model, Observation, ProvenanceSource};
use crate::models_dev::Adapted;
use crate::time::format_rfc3339;
use crate::{
    CatalogError, cliproxyapi_cache, modelparams, models_dev, omp_bundled, omp_live, router_models,
};

/// HTTP facts recorded when the snapshot was fetched rather than read from disk.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchMeta {
    pub url: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// The raw snapshot plus where/when it came from.
#[derive(Debug, Clone, Copy)]
pub struct SourceInput<'a> {
    pub bytes: &'a [u8],
    pub fetched_at_ms: i64,
    pub fetch: Option<&'a FetchMeta>,
}

#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    /// Structural failure if the source holds fewer models than this.
    pub min_models: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportSource {
    #[serde(flatten)]
    pub descriptor: ProvenanceSource,
    /// Full SHA-256 of the snapshot bytes (the descriptor's origin carries the first 16 hex digits).
    pub sha256: String,
    pub bytes: usize,
    pub fetched_at: String,
    pub fetched_at_ms: i64,
    /// Present only for `--fetch` runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http: Option<FetchMeta>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub providers: usize,
    pub observations: usize,
    pub models: usize,
    pub rejections: usize,
    /// Every mapped sentinel, i.e. `report.sentinels.len()`: documented sentinels plus zero limits.
    pub sentinels: usize,
    /// `sentinels` broken down by field (e.g. `maxTokens`, `thinking.minBudgetTokens`).
    pub sentinels_by_field: BTreeMap<String, usize>,
    /// The part of `sentinels` that are limits published as `0` and mapped to unknown.
    pub zero_limits_mapped_to_unknown: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub source: ReportSource,
    pub counts: Counts,
    pub rejections: Vec<Diagnostic>,
    pub sentinels: Vec<SentinelMapping>,
    /// Values with no field in the contract (kept out of the records, listed here).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unmodelled: Vec<Diagnostic>,
}

/// Everything a successful run produces.
#[derive(Debug)]
pub struct BuildOutput {
    pub observations: Vec<Observation>,
    pub models: Vec<Model>,
    pub report: Report,
}

impl BuildOutput {
    /// `--strict`: any rejection fails the run.
    pub fn ensure_no_rejections(&self) -> Result<(), CatalogError> {
        match self.report.rejections.len() {
            0 => Ok(()),
            count => Err(CatalogError::StrictRejections { count }),
        }
    }
}

/// Builds the catalog from a models.dev snapshot.
pub fn build_models_dev(
    input: SourceInput<'_>,
    options: &BuildOptions,
) -> Result<BuildOutput, CatalogError> {
    assemble(
        models_dev::adapt(input.bytes, input.fetched_at_ms)?,
        input,
        options,
    )
}

/// Builds the catalog from a modelparams.dev snapshot.
pub fn build_modelparams(
    input: SourceInput<'_>,
    options: &BuildOptions,
) -> Result<BuildOutput, CatalogError> {
    assemble(
        modelparams::adapt(input.bytes, input.fetched_at_ms)?,
        input,
        options,
    )
}

/// Builds the catalog from an omp cache of a CLIProxyAPI gateway's model list.
pub fn build_cliproxyapi_cache(
    input: SourceInput<'_>,
    options: &BuildOptions,
) -> Result<BuildOutput, CatalogError> {
    assemble(cliproxyapi_cache::adapt(input.bytes)?, input, options)
}

/// Builds the catalog from a saved `omp models <provider> --json` roster of a gateway provider
/// (`easycliproxy`, `dsh`).
pub fn build_omp_live(
    provider: &str,
    input: SourceInput<'_>,
    options: &BuildOptions,
) -> Result<BuildOutput, CatalogError> {
    assemble(
        omp_live::adapt(input.bytes, input.fetched_at_ms, provider)?,
        input,
        options,
    )
}

/// Builds the catalog from oh-my-pi's bundled model roster (decompressed `models.json`).
pub fn build_omp_bundled(
    input: SourceInput<'_>,
    options: &BuildOptions,
) -> Result<BuildOutput, CatalogError> {
    assemble(
        omp_bundled::adapt(input.bytes, input.fetched_at_ms)?,
        input,
        options,
    )
}

/// The source adapters a build can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    ModelsDev,
    ModelparamsDev,
    CliproxyapiCache,
    OmpLiveEasycliproxy,
    OmpLiveDsh,
    OmpBundled,
    RouterModels,
    Dsh,
    Freellmapi,
}

impl Source {
    pub const ALL: [Source; 9] = [
        Source::ModelsDev,
        Source::ModelparamsDev,
        Source::CliproxyapiCache,
        Source::OmpLiveEasycliproxy,
        Source::OmpLiveDsh,
        Source::OmpBundled,
        Source::RouterModels,
        Source::Dsh,
        Source::Freellmapi,
    ];

    /// The `sourceId` (also the spelling of `--source`).
    pub fn id(self) -> &'static str {
        match self {
            Self::ModelsDev => models_dev::SOURCE_ID,
            Self::ModelparamsDev => modelparams::SOURCE_ID,
            Self::CliproxyapiCache => cliproxyapi_cache::SOURCE_ID,
            Self::OmpLiveEasycliproxy => omp_live::EASYCLIPROXY_SOURCE_ID,
            Self::OmpLiveDsh => omp_live::DSH_SOURCE_ID,
            Self::OmpBundled => omp_bundled::SOURCE_ID,
            Self::RouterModels => router_models::SOURCE_ID,
            Self::Dsh => dsh::SOURCE_ID,
            Self::Freellmapi => freellmapi::SOURCE_ID,
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.id() == id)
    }

    /// Where `--fetch` downloads the snapshot from; `None` for sources read from a local file.
    pub fn api_url(self) -> Option<&'static str> {
        match self {
            Self::ModelsDev => Some(models_dev::API_URL),
            Self::ModelparamsDev => Some(modelparams::API_URL),
            Self::CliproxyapiCache
            | Self::OmpLiveEasycliproxy
            | Self::OmpLiveDsh
            | Self::OmpBundled
            | Self::RouterModels
            | Self::Dsh
            | Self::Freellmapi => None,
        }
    }

    pub fn build(
        self,
        input: SourceInput<'_>,
        options: &BuildOptions,
    ) -> Result<BuildOutput, CatalogError> {
        match self {
            Self::ModelsDev => build_models_dev(input, options),
            Self::ModelparamsDev => build_modelparams(input, options),
            Self::CliproxyapiCache => build_cliproxyapi_cache(input, options),
            Self::OmpLiveEasycliproxy => build_omp_live(omp_live::EASYCLIPROXY, input, options),
            Self::OmpLiveDsh => build_omp_live(omp_live::DSH, input, options),
            Self::OmpBundled => build_omp_bundled(input, options),
            Self::RouterModels => assemble(
                router_models::adapt(input.bytes, input.fetched_at_ms)?,
                input,
                options,
            ),
            Self::Dsh => assemble(
                dsh::adapt(input.bytes, input.fetched_at_ms)?,
                input,
                options,
            ),
            Self::Freellmapi => assemble(
                freellmapi::adapt(input.bytes, input.fetched_at_ms)?,
                input,
                options,
            ),
        }
    }
}

fn assemble(
    adapted: Adapted,
    input: SourceInput<'_>,
    options: &BuildOptions,
) -> Result<BuildOutput, CatalogError> {
    let found = adapted.observations.len();
    if let Some(min) = options.min_models
        && found < min
    {
        return Err(CatalogError::BelowMinModels { found, min });
    }
    let models = single_source_all(&adapted.observations);
    let findings = adapted.findings;
    let report = Report {
        counts: Counts {
            providers: adapted.providers,
            observations: found,
            models: models.len(),
            rejections: findings.rejections.len(),
            sentinels: findings.sentinels.len(),
            sentinels_by_field: by_field(&findings.sentinels),
            zero_limits_mapped_to_unknown: findings.zero_limits,
        },
        source: ReportSource {
            descriptor: adapted.source,
            sha256: adapted.sha256,
            bytes: input.bytes.len(),
            fetched_at: format_rfc3339(input.fetched_at_ms),
            fetched_at_ms: input.fetched_at_ms,
            http: input.fetch.cloned(),
        },
        rejections: findings.rejections,
        sentinels: findings.sentinels,
        unmodelled: findings.unmodelled,
    };
    Ok(BuildOutput {
        observations: adapted.observations,
        models,
        report,
    })
}

fn by_field(sentinels: &[SentinelMapping]) -> BTreeMap<String, usize> {
    let mut map = BTreeMap::new();
    for s in sentinels {
        *map.entry(s.field.clone()).or_insert(0) += 1;
    }
    map
}

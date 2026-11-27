//! `tierbridge-router.json` for the CLIProxyAPI plugin: the ranked candidate list behind one alias.
//!
//! The plugin's `ModelRouter` only answers its own alias (`tierbridge/auto`); this module decides,
//! deterministically and offline, which models stand behind it and in what order. The candidates
//! are the rows of ONE gateway (the one CLIProxyAPI serves) and the file is written for the CPA
//! that runs the plugin, the **host** (`target: "host"`): every `candidate.model` is what that CPA
//! itself accepts for the row, `model_prefix` + `id`, and there is no provider (`tierbridge-router/1`
//! named one, `/2` does not). A live CPA lists the plain id in its `/v1/models` (empty prefix); a
//! scratch CPA configured with `export cpa --alias-prefix tb-` accepts `tb-` + id, so there
//! `candidate.model` is an alias `export cpa` registers.
//!
//! # What is left out
//!
//! A row of the gateway is not a candidate when
//! - its `kind` is known and is not `chat` (`not-chat`);
//! - its `lifecycle.status` is `retired` (`retired`);
//! - its **limits are placeholders only** (`placeholder-limits`): `contextWindow`,
//!   `maxInputTokens` and `maxTokens` are all `null` AND at least one of the three has a
//!   provenance entry that holds nothing but placeholders. The merge never lets a placeholder (a
//!   `(contextWindow, maxTokens)` pair that covers >= 20% of a gateway's rows, see
//!   [`crate::placeholder`]) win a field, so such a row reaches the catalog with a `null` limit and
//!   a `provenance.fields.<limit>` of confidence `unknown` whose every alternate has confidence
//!   `unknown` (only a gateway placeholder gets `unknown`; reference values are `declared` or
//!   `inferred`). A row with `null` limits and no such provenance is merely unknown and stays;
//!   so does a row that has at least one real limit;
//! - another eligible row already produced the same alias (`duplicate-alias`; the first in key
//!   order wins, as in `export cpa`).
//!
//! # Profile `free-first`
//!
//! Known-free, then known cost by ascending `input + output` (USD per million tokens), then
//! unknown cost. A `known` cost is rankable only with a finite, non-negative input AND output; an
//! incomplete one counts as unknown and is written as `{status: unknown}` so a consumer never
//! multiplies by a missing price. Ties break by evidence confidence (best first), then
//! `contextWindow` descending (`null` last), then `id`, then catalog key, all ascending.
//!
//! # Confidence of a row
//!
//! The merge records a confidence per field, not per row, and most fields are `declared` for every
//! row simply because the gateway lists them, so "best of all fields" would not tell rows apart.
//! The row's confidence is the **weakest** confidence among the facts this ranking actually rests
//! on that the row HAS: the cost (unless it is unknown) and the context window (unless it is
//! `null`). A row with neither is `unknown`. A fact the row lacks is not penalised twice: it
//! already sorts last through the cost bucket or the `null` context window. A field without
//! provenance counts as `unknown`.
//!
//! # Quota
//!
//! The merged `catalog.json` does not carry freellmapi's free-tier limits: they live in `raw` of
//! the freellmapi *observations* (`raw.freeTier`), and the model schema has no `raw`. So the quota
//! comes from `observations.freellmapi.jsonl`, passed apart, and is `null` without it. A catalog
//! row gets a quota only when a freellmapi chat observation with at least one limit matches it:
//! 1. **`provider-id`**: the row's `provider` and `id` equal the observation's (always the case
//!    when the gateway is freellmapi itself);
//! 2. **`canonicalId`**: the row's `canonicalId` equals the `canonicalId` the merge gave the
//!    `freellmapi:<provider>/<id>` row of that observation (the observation itself has none).
//!    Several platforms can serve one canonical model; the quota of the first by
//!    `<provider>/<id>` is used and `matches` says how many qualified.
//!
//! Equal ids alone never match. A `provider-id` hit whose observation has no limits ends the
//! search: the row has no quota rather than another platform's.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::CatalogError;
use crate::freellmapi::SOURCE_ID as FREELLMAPI;
use crate::model::{Cost, CostStatus, Lifecycle, LifecycleStatus, Modality, ModelKind};
use crate::models_dev::sha256_hex;

/// `schema` of the written file. `/2` is the host mode: no `provider`, plus `target` and `modelPrefix`.
pub const SCHEMA: &str = "tierbridge-router/2";
/// `target` of the written file: the CPA that runs the plugin resolves every `candidate.model` itself.
pub const TARGET: &str = "host";
/// Alias the plugin serves unless told otherwise.
pub const DEFAULT_ALIAS: &str = "tierbridge/auto";

/// How candidates are ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Known-free > cheapest known cost > unknown cost.
    FreeFirst,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FreeFirst => "free-first",
        }
    }
}

/// What the caller decides; everything else comes from the catalog.
#[derive(Debug, Clone)]
pub struct RouterOptions<'a> {
    /// Gateway source id as it prefixes catalog keys (`omp-live:easycliproxy`).
    pub gateway: &'a str,
    /// Prepended to every id to form `candidate.model`, the name the host CPA accepts: empty for a
    /// live CPA (the plain id its `/v1/models` lists), `tb-` for one configured by
    /// `export cpa --alias-prefix tb-`.
    pub model_prefix: &'a str,
    /// The alias clients call (`tierbridge/auto`). It must not equal any candidate's `model`.
    pub alias: &'a str,
    pub profile: Profile,
    /// Bytes of `observations.freellmapi.jsonl`; `None` leaves every quota `null`.
    pub freellmapi_observations: Option<&'a [u8]>,
}

/// Why a catalog row of the chosen gateway is not a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SkipReason {
    /// `kind` is known and is not `chat`.
    NotChat,
    /// `lifecycle.status` is `retired`.
    Retired,
    /// No limit has a value and one of them holds only placeholders (see the module docs).
    PlaceholderLimits,
    /// Another eligible row already produced the same alias.
    DuplicateAlias,
}

impl SkipReason {
    pub fn label(self) -> &'static str {
        match self {
            Self::NotChat => "not-chat",
            Self::Retired => "retired",
            Self::PlaceholderLimits => "placeholder-limits",
            Self::DuplicateAlias => "duplicate-alias",
        }
    }
}

/// How far to trust a fact (`provenance.fields.<field>.confidence`), best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Evidence {
    Verified,
    Declared,
    Inferred,
    #[serde(other)]
    Unknown,
}

impl Evidence {
    fn strength(self) -> u8 {
        match self {
            Self::Verified => 3,
            Self::Declared => 2,
            Self::Inferred => 1,
            Self::Unknown => 0,
        }
    }
}

/// Prices as the router uses them: what ranking saw, nothing the catalog left out.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateCost {
    pub status: CostStatus,
    /// USD per million tokens; `null` when the status is `unknown`.
    pub input: Option<f64>,
    pub output: Option<f64>,
}

/// Free-tier limits of the freellmapi platform row that matched.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Quota {
    pub rpm: Option<u64>,
    pub rpd: Option<u64>,
    pub tpm: Option<u64>,
    pub tpd: Option<u64>,
    /// Free text as freellmapi writes it (`~30M`, `~3-5M`); not a number.
    pub monthly_token_budget: Option<String>,
    /// Catalog key of the freellmapi row the limits belong to (`freellmapi:groq/<id>`).
    pub source: String,
    /// `provider-id` or `canonicalId`.
    pub matched_by: &'static str,
    /// How many freellmapi rows qualified at that strength (> 1: the first was used).
    pub matches: usize,
}

/// One model the alias can route to.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    /// What the host CPA accepts for this model: `model_prefix` + `id`.
    pub model: String,
    /// Id the gateway serves.
    pub id: String,
    /// Catalog key of the row.
    pub key: String,
    pub context_window: Option<u64>,
    pub max_tokens: Option<u64>,
    pub supports_tools: Option<bool>,
    /// Input modalities, sorted; `null` when the catalog does not know them.
    pub input: Option<Vec<Modality>>,
    pub cost: CandidateCost,
    /// Tie-break confidence of the row (see the module docs).
    pub confidence: Evidence,
    pub quota: Option<Quota>,
    /// Why it is here and where: `known-free` | `cheapest-known-cost` | `unknown-cost`, then
    /// `freellmapi-limits` when `quota` is set.
    pub reasons: Vec<&'static str>,
}

/// The result: the file's text and what went into it.
#[derive(Debug, Clone)]
pub struct RouterExport {
    /// The JSON document, pretty-printed, ending in a newline.
    pub json: String,
    /// Candidates in rank order.
    pub candidates: Vec<Candidate>,
    /// `(catalog key, reason)` of every row of the gateway that is not a candidate.
    pub skipped: Vec<(String, SkipReason)>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Document<'a> {
    schema: &'static str,
    /// Who resolves `candidate.model`: always [`TARGET`].
    target: &'static str,
    /// What was prepended to every `id` to form `candidate.model`; `""` for a live CPA.
    model_prefix: &'a str,
    /// SHA-256 of the exact `catalog.json` bytes this was computed from.
    catalog_sha256: String,
    /// SHA-256 of the observations the quotas came from; `null` without any.
    quota_source_sha256: Option<String>,
    gateway: &'a str,
    aliases: [AliasEntry<'a>; 1],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AliasEntry<'a> {
    alias: &'a str,
    profile: &'static str,
    /// Rows of the gateway left out, by reason.
    skipped: BTreeMap<&'static str, usize>,
    candidates: &'a [Candidate],
}

#[derive(Deserialize)]
struct Alternate {
    confidence: Option<Evidence>,
}

#[derive(Deserialize)]
struct FieldProvenance {
    confidence: Evidence,
    #[serde(default)]
    alternates: Vec<Alternate>,
}

#[derive(Deserialize)]
struct Provenance {
    #[serde(default)]
    fields: BTreeMap<String, FieldProvenance>,
}

/// The fields of a `catalog.json` row this export reads; the rest are ignored.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Row {
    key: String,
    provider: String,
    id: String,
    canonical_id: Option<String>,
    kind: Option<ModelKind>,
    context_window: Option<u64>,
    max_input_tokens: Option<u64>,
    max_tokens: Option<u64>,
    cost: Option<Cost>,
    input: Option<Vec<Modality>>,
    supports_tools: Option<bool>,
    lifecycle: Option<Lifecycle>,
    provenance: Option<Provenance>,
}

const LIMIT_FIELDS: [&str; 3] = ["contextWindow", "maxInputTokens", "maxTokens"];

impl Row {
    /// Source id the row came from: the key is `<sourceId>:<provider>/<id>` and the source id may
    /// itself contain `:` (`omp-live:easycliproxy`).
    fn gateway(&self) -> Option<&str> {
        let tail = format!("{}/{}", self.provider, self.id);
        self.key.strip_suffix(&tail)?.strip_suffix(':')
    }

    fn field(&self, name: &str) -> Option<&FieldProvenance> {
        self.provenance.as_ref()?.fields.get(name)
    }

    fn confidence_of(&self, name: &str) -> Evidence {
        self.field(name).map_or(Evidence::Unknown, |f| f.confidence)
    }

    /// The merge keeps a placeholder as an alternate only: no winner, confidence `unknown`, and
    /// nothing but `unknown` alternates.
    fn limit_is_placeholder_only(&self, name: &str) -> bool {
        self.field(name).is_some_and(|f| {
            f.confidence == Evidence::Unknown
                && !f.alternates.is_empty()
                && f.alternates
                    .iter()
                    .all(|a| a.confidence == Some(Evidence::Unknown))
        })
    }

    fn placeholder_limits(&self) -> bool {
        self.context_window.is_none()
            && self.max_input_tokens.is_none()
            && self.max_tokens.is_none()
            && LIMIT_FIELDS
                .iter()
                .any(|f| self.limit_is_placeholder_only(f))
    }
}

#[derive(Clone, Copy)]
enum Price {
    Free,
    /// `input + output`.
    Known(f64),
    Unknown,
}

fn rankable(price: f64) -> bool {
    price.is_finite() && price >= 0.0
}

fn price_of(cost: Option<&Cost>) -> (Price, CandidateCost) {
    match cost {
        Some(c) if c.status == CostStatus::Free => (
            Price::Free,
            CandidateCost {
                status: CostStatus::Free,
                input: Some(0.0),
                output: Some(0.0),
            },
        ),
        Some(Cost {
            status: CostStatus::Known,
            input: Some(input),
            output: Some(output),
            ..
        }) if rankable(*input) && rankable(*output) => {
            let sum = input + output;
            (
                // `-0.0` would sort before `0.0` under `total_cmp`
                Price::Known(if sum == 0.0 { 0.0 } else { sum }),
                CandidateCost {
                    status: CostStatus::Known,
                    input: Some(*input),
                    output: Some(*output),
                },
            )
        }
        _ => (
            Price::Unknown,
            CandidateCost {
                status: CostStatus::Unknown,
                input: None,
                output: None,
            },
        ),
    }
}

/// Weakest confidence among the ranking facts the row has; see the module docs.
fn row_confidence(row: &Row, price: Price) -> Evidence {
    let mut facts = Vec::with_capacity(2);
    if !matches!(price, Price::Unknown) {
        facts.push(row.confidence_of("cost"));
    }
    if row.context_window.is_some() {
        facts.push(row.confidence_of("contextWindow"));
    }
    facts
        .into_iter()
        .min_by_key(|e| e.strength())
        .unwrap_or(Evidence::Unknown)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ObservationEnvelope {
    source: ObservationSource,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ObservationSource {
    source_id: String,
}

#[derive(Deserialize)]
struct Observation {
    provider: String,
    id: String,
    kind: Option<ModelKind>,
    raw: Option<ObservationRaw>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ObservationRaw {
    free_tier: Option<FreeTier>,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct FreeTier {
    rpm: Option<u64>,
    rpd: Option<u64>,
    tpm: Option<u64>,
    tpd: Option<u64>,
    monthly_token_budget: Option<String>,
}

impl FreeTier {
    fn has_limits(&self) -> bool {
        self.rpm.is_some()
            || self.rpd.is_some()
            || self.tpm.is_some()
            || self.tpd.is_some()
            || self.monthly_token_budget.is_some()
    }
}

/// The freellmapi chat observations and how to get from a catalog row to one of them.
struct QuotaIndex {
    /// `<provider>/<id>` -> its limits; `None` when the observation states none.
    by_platform_id: BTreeMap<String, Option<FreeTier>>,
    /// `canonicalId` -> the `<provider>/<id>` of every observation WITH limits whose catalog row
    /// has that canonical id.
    by_canonical: BTreeMap<String, BTreeSet<String>>,
}

impl QuotaIndex {
    /// `freellmapi_rows`: `(canonicalId, provider/id)` of the catalog rows of the freellmapi source.
    fn build(
        observations: &[u8],
        freellmapi_rows: &[(String, String)],
    ) -> Result<Self, CatalogError> {
        let text = std::str::from_utf8(observations).map_err(|e| {
            CatalogError::Export(format!("freellmapi observations are not UTF-8: {e}"))
        })?;
        let mut by_platform_id = BTreeMap::new();
        for (n, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let bad = |e: serde_json::Error| {
                CatalogError::Export(format!("freellmapi observations, line {}: {e}", n + 1))
            };
            let envelope: ObservationEnvelope = serde_json::from_str(line).map_err(bad)?;
            if envelope.source.source_id != FREELLMAPI {
                continue;
            }
            let obs: Observation = serde_json::from_str(line).map_err(bad)?;
            if obs.kind.is_some_and(|k| k != ModelKind::Chat) {
                continue;
            }
            let tier = obs
                .raw
                .and_then(|r| r.free_tier)
                .filter(FreeTier::has_limits);
            by_platform_id.insert(format!("{}/{}", obs.provider, obs.id), tier);
        }
        let mut by_canonical: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (canonical, platform_id) in freellmapi_rows {
            if by_platform_id
                .get(platform_id)
                .is_some_and(|tier| tier.is_some())
            {
                by_canonical
                    .entry(canonical.clone())
                    .or_default()
                    .insert(platform_id.clone());
            }
        }
        Ok(Self {
            by_platform_id,
            by_canonical,
        })
    }

    fn quota_for(&self, row: &Row) -> Option<Quota> {
        let exact = format!("{}/{}", row.provider, row.id);
        if let Some(tier) = self.by_platform_id.get(&exact) {
            return tier.as_ref().map(|t| quota(t, &exact, "provider-id", 1));
        }
        let candidates = self.by_canonical.get(row.canonical_id.as_deref()?)?;
        let first = candidates.iter().next()?;
        let tier = self.by_platform_id.get(first)?.as_ref()?;
        Some(quota(tier, first, "canonicalId", candidates.len()))
    }
}

fn quota(tier: &FreeTier, platform_id: &str, matched_by: &'static str, matches: usize) -> Quota {
    Quota {
        rpm: tier.rpm,
        rpd: tier.rpd,
        tpm: tier.tpm,
        tpd: tier.tpd,
        monthly_token_budget: tier.monthly_token_budget.clone(),
        source: format!("{FREELLMAPI}:{platform_id}"),
        matched_by,
        matches,
    }
}

/// A candidate with what `free-first` sorts on.
struct Ranked {
    candidate: Candidate,
    price: Price,
}

fn bucket(price: Price) -> u8 {
    match price {
        Price::Free => 0,
        Price::Known(_) => 1,
        Price::Unknown => 2,
    }
}

fn amount(price: Price) -> f64 {
    match price {
        Price::Known(sum) => sum,
        Price::Free | Price::Unknown => 0.0,
    }
}

/// `free-first`: cost bucket, price, confidence (best first), context window (descending, `null`
/// last), id, key.
fn free_first(a: &Ranked, b: &Ranked) -> Ordering {
    bucket(a.price)
        .cmp(&bucket(b.price))
        .then_with(|| amount(a.price).total_cmp(&amount(b.price)))
        .then_with(|| {
            b.candidate
                .confidence
                .strength()
                .cmp(&a.candidate.confidence.strength())
        })
        .then_with(
            || match (a.candidate.context_window, b.candidate.context_window) {
                (Some(x), Some(y)) => y.cmp(&x),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            },
        )
        .then_with(|| a.candidate.id.cmp(&b.candidate.id))
        .then_with(|| a.candidate.key.cmp(&b.candidate.key))
}

/// Build the router file for `opts.gateway` from the bytes of a merged `catalog.json`.
pub fn export_cpa_router(
    catalog: &[u8],
    opts: &RouterOptions<'_>,
) -> Result<RouterExport, CatalogError> {
    if opts.alias.trim().is_empty() {
        return Err(CatalogError::Export("the router alias is empty".to_owned()));
    }
    let mut rows: Vec<Row> = serde_json::from_slice(catalog)?;
    let available: BTreeSet<String> = rows
        .iter()
        .filter_map(|r| r.gateway().map(str::to_owned))
        .collect();
    let freellmapi_rows: Vec<(String, String)> = rows
        .iter()
        .filter(|r| r.gateway() == Some(FREELLMAPI))
        .filter_map(|r| {
            r.canonical_id
                .clone()
                .map(|c| (c, format!("{}/{}", r.provider, r.id)))
        })
        .collect();
    rows.retain(|r| r.gateway() == Some(opts.gateway));
    if rows.is_empty() {
        return Err(CatalogError::Export(format!(
            "no rows from gateway `{}` in the catalog (gateways present: {})",
            opts.gateway,
            if available.is_empty() {
                "none".to_owned()
            } else {
                available.into_iter().collect::<Vec<_>>().join(", ")
            }
        )));
    }
    rows.sort_by(|a, b| a.key.cmp(&b.key));
    let quotas = opts
        .freellmapi_observations
        .map(|bytes| QuotaIndex::build(bytes, &freellmapi_rows))
        .transpose()?;

    let mut skipped = Vec::new();
    let mut taken = BTreeSet::new();
    let mut ranked = Vec::with_capacity(rows.len());
    for row in &rows {
        if row.kind.is_some_and(|k| k != ModelKind::Chat) {
            skipped.push((row.key.clone(), SkipReason::NotChat));
            continue;
        }
        if row
            .lifecycle
            .as_ref()
            .is_some_and(|l| l.status == LifecycleStatus::Retired)
        {
            skipped.push((row.key.clone(), SkipReason::Retired));
            continue;
        }
        if row.placeholder_limits() {
            skipped.push((row.key.clone(), SkipReason::PlaceholderLimits));
            continue;
        }
        let model = format!("{}{}", opts.model_prefix, row.id);
        if model == opts.alias {
            return Err(CatalogError::Export(format!(
                "alias `{}` is also the model of candidate `{}`; pick another --alias or --model-prefix",
                opts.alias, row.key
            )));
        }
        if !taken.insert(model.clone()) {
            skipped.push((row.key.clone(), SkipReason::DuplicateAlias));
            continue;
        }
        let (price, cost) = price_of(row.cost.as_ref());
        let quota = quotas.as_ref().and_then(|q| q.quota_for(row));
        let mut reasons = vec![match price {
            Price::Free => "known-free",
            Price::Known(_) => "cheapest-known-cost",
            Price::Unknown => "unknown-cost",
        }];
        if quota.is_some() {
            reasons.push("freellmapi-limits");
        }
        let input = row.input.as_ref().and_then(|list| {
            let sorted: Vec<Modality> = list
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            (!sorted.is_empty()).then_some(sorted)
        });
        ranked.push(Ranked {
            price,
            candidate: Candidate {
                model,
                id: row.id.clone(),
                key: row.key.clone(),
                context_window: row.context_window,
                max_tokens: row.max_tokens,
                supports_tools: row.supports_tools,
                input,
                cost,
                confidence: row_confidence(row, price),
                quota,
                reasons,
            },
        });
    }
    match opts.profile {
        Profile::FreeFirst => ranked.sort_by(free_first),
    }
    let candidates: Vec<Candidate> = ranked.into_iter().map(|r| r.candidate).collect();

    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for (_, reason) in &skipped {
        *counts.entry(reason.label()).or_default() += 1;
    }
    let document = Document {
        schema: SCHEMA,
        target: TARGET,
        model_prefix: opts.model_prefix,
        catalog_sha256: sha256_hex(catalog),
        quota_source_sha256: opts.freellmapi_observations.map(sha256_hex),
        gateway: opts.gateway,
        aliases: [AliasEntry {
            alias: opts.alias,
            profile: opts.profile.as_str(),
            skipped: counts,
            candidates: &candidates,
        }],
    };
    let mut json = serde_json::to_string_pretty(&document)?;
    json.push('\n');
    Ok(RouterExport {
        json,
        candidates,
        skipped,
    })
}

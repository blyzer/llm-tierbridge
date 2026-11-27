//! Gateway observations + reference catalogs -> `catalog.json` (`docs/catalog-proof.md` §9).
//!
//! One [`Model`] per gateway row (`<gatewaySourceId>:<provider>/<id>`); the references
//! (models.dev, modelparams.dev) only enrich it. See [`crate::precedence`] for the principles.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;
use serde_json::{Value, json};

use crate::matching::{IdKey, RefIndex, TierCounts, is_first_party};
use crate::model::{
    AcceptedParam, AuthType, Confidence, Cost, CostStatus, Effort, FieldName, FieldProvenance,
    Fields, Lifecycle, LifecycleStatus, MatchTier, Modality, Model, ModelProvenance, ModelRef,
    Observation, ProvenanceSource, RecordType, Thinking, ThinkingMode, ThinkingOptions,
};
use crate::overrides::{OVERRIDES_SOURCE_ID, OverrideFields, Overrides};
use crate::placeholder::Placeholders;
use crate::precedence::{
    Fallback, GwCand, Level, NO_SOURCE, Outcome, RefCand, Resolved, Stage, Win, configured, finish,
    resolve,
};
use crate::{BuildOutput, CatalogError, Diagnostic, Source};

pub const MODELS_DEV: &str = "models.dev";
pub const MODELPARAMS: &str = "modelparams.dev";

/// What a source is to the merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceRole {
    /// Defines which models exist (one `Model` per row).
    Gateway,
    /// Only enriches gateway rows (models.dev, modelparams.dev).
    Reference,
    /// Shown, never used: omp's bundled roster is derived from the references (circular).
    Probe,
}

impl Source {
    pub fn role(self) -> SourceRole {
        match self {
            Self::ModelsDev | Self::ModelparamsDev => SourceRole::Reference,
            Self::OmpBundled => SourceRole::Probe,
            Self::CliproxyapiCache
            | Self::OmpLiveEasycliproxy
            | Self::OmpLiveDsh
            | Self::RouterModels
            | Self::Dsh
            | Self::Freellmapi => SourceRole::Gateway,
        }
    }
}

const LIMITS: &str = "limits: gateway-live > ref-first-party > ref-majority";
const MAX_TOKENS: &str = "limits: gateway-live > ref-first-party > ref-majority > range-max";
const COST: &str = "cost: gateway-known > ref-first-party > ref-majority > unknown";
const NAME: &str = "name: gateway-live > ref-first-party";
const CANONICAL: &str = "canonicalId: ref-first-party > ref-majority";
const INPUT: &str = "input: gateway-live > ref-first-party";
const OUTPUT: &str = "output: gateway-live > ref-first-party";
const TOOLS: &str = "supportsTools: gateway-live > ref-first-party > ref-majority";
const REASONING: &str = "reasoning: gateway-live > modelparams > models.dev";
const THINKING: &str = "thinking: gateway-live > modelparams-first-party > models.dev-first-party";
const LIFECYCLE: &str = "lifecycle: gateway-live > modelparams > models.dev > gateway-listed";
const PARAMS: &str = "acceptedParams: modelparams-api-key";

/// A merge-level finding about one row (nothing is dropped silently).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeDiagnostic {
    pub kind: String,
    pub key: Option<String>,
    pub source_id: String,
    pub provider: String,
    pub id: String,
    pub field: String,
    pub value: Value,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldStats {
    /// Winning rule (`<chain> [<level>]`) -> models.
    pub winner_rules: BTreeMap<String, usize>,
    pub conflicts: usize,
    /// Models whose field ended null / unknown.
    pub unresolved: usize,
    /// Placeholder values kept as alternates.
    pub placeholder_alternates: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaceholderSet {
    pub pairs: Vec<(Option<u64>, Option<u64>)>,
    pub efforts: Vec<Vec<crate::model::Effort>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayReport {
    pub source_id: String,
    /// Observations the source has.
    pub observations: usize,
    /// Retired rows do not create models (the channel dropped them; that is not a vendor retirement).
    pub retired_skipped: usize,
    /// Models produced.
    pub rows: usize,
    pub matched: usize,
    pub unmatched: usize,
    pub matched_pct: f64,
    /// Per reference: the tier that matched each row, or unmatched.
    pub per_reference: BTreeMap<String, TierCounts>,
    pub placeholders: PlaceholderSet,
    /// Rows whose own value for the field is a placeholder.
    pub placeholder_rows: BTreeMap<FieldName, usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CostStats {
    pub known: usize,
    pub free: usize,
    pub unknown: usize,
    /// Winning gateway costs that are free or all-zero: must be 0 (a gateway's 0 is "unknown").
    pub gateway_zero_winners: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaceholderWinners {
    pub context_window: usize,
    pub max_input_tokens: usize,
    pub max_tokens: usize,
    pub thinking: usize,
    pub total: usize,
}

/// models.dev `limit.output` vs modelparams.dev `range.max` on matched pairs (§10.2).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaxTokensStudy {
    pub modelparams_with_max_tokens: usize,
    pub no_models_dev_match: usize,
    pub models_dev_has_no_output: usize,
    pub models_dev_ambiguous: usize,
    pub comparable: usize,
    pub equal: usize,
    pub range_max_greater: usize,
    pub range_max_lower: usize,
    pub equal_pct: f64,
    pub disagreeing_by_provider: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeSourceInfo {
    pub source_id: String,
    pub role: SourceRole,
    pub observations: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeReport {
    pub sources: Vec<MergeSourceInfo>,
    pub models: usize,
    pub gateways: Vec<GatewayReport>,
    pub fields: BTreeMap<FieldName, FieldStats>,
    pub placeholder_winners: PlaceholderWinners,
    pub cost: CostStats,
    /// modelparams entries not used for values because the same `provider/id` has an `api_key` variant.
    pub reference_auth_variants_skipped: usize,
    pub max_tokens_study: Option<MaxTokensStudy>,
    pub overrides: Option<OverridesReport>,
    pub diagnostics: Vec<MergeDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverridesReport {
    pub origin: String,
    pub sha256: String,
    pub entries: usize,
    /// Models that got at least one `configured` field.
    pub models_overridden: usize,
}

#[derive(Debug)]
pub struct MergeOutput {
    pub models: Vec<Model>,
    pub report: MergeReport,
}

struct RefSource<'a> {
    id: &'a str,
    out: &'a BuildOutput,
    /// The observations as the merge reads them (a `default`-only effort ladder is a toggle).
    obs: Vec<Cow<'a, Observation>>,
    keys: Vec<IdKey>,
    index: RefIndex,
    /// `(provider, id)` -> values the adapter could not carry (models.dev `cost.reasoning`, audio).
    unmodelled: HashMap<(&'a str, &'a str), Vec<&'a Diagnostic>>,
    /// What normalising the observations changed.
    notes: Vec<MergeDiagnostic>,
}

struct Hit<'a> {
    obs: &'a Observation,
    first_party: bool,
}

struct Hits<'a> {
    source: &'a str,
    tier: Option<MatchTier>,
    hits: Vec<Hit<'a>>,
}

fn preferred_auth(a: Option<AuthType>) -> bool {
    matches!(a, None | Some(AuthType::ApiKey))
}

impl<'a> RefSource<'a> {
    fn new(out: &'a BuildOutput) -> Self {
        let rejected = default_rejected(out);
        let mut notes = Vec::new();
        let obs: Vec<Cow<'a, Observation>> = out
            .observations
            .iter()
            .map(|o| match as_toggle(o, &rejected) {
                Some((n, note)) => {
                    notes.push(note);
                    Cow::Owned(n)
                }
                None => Cow::Borrowed(o),
            })
            .collect();
        let keys: Vec<IdKey> = out.observations.iter().map(IdKey::from).collect();
        let mut unmodelled: HashMap<(&str, &str), Vec<&Diagnostic>> = HashMap::new();
        for d in &out.report.unmodelled {
            unmodelled
                .entry((d.provider.as_str(), d.id.as_str()))
                .or_default()
                .push(d);
        }
        Self {
            id: &out.report.source.descriptor.source_id,
            out,
            obs,
            index: RefIndex::new(&keys),
            keys,
            unmodelled,
            notes,
        }
    }

    /// Entries matching `gateway_id`; only the `api_key` variant of a `provider/id` that has
    /// several auth types. Returns the hits and how many entries that skipped.
    fn hits(&self, gateway_id: &str) -> (Hits<'_>, usize) {
        let mut hits = Hits {
            source: self.id,
            tier: None,
            hits: Vec::new(),
        };
        let Some((tier, idx)) = self.index.lookup_indices(gateway_id) else {
            return (hits, 0);
        };
        hits.tier = Some(tier);
        let obs = &self.obs;
        let mut skipped = 0;
        for &i in &idx {
            let o = obs[i].as_ref();
            let has_preferred = idx.iter().any(|&j| {
                let p = obs[j].as_ref();
                p.fields.provider == o.fields.provider
                    && p.fields.id == o.fields.id
                    && preferred_auth(p.fields.auth_type)
            });
            if !preferred_auth(o.fields.auth_type) && has_preferred {
                skipped += 1;
                continue;
            }
            hits.hits.push(Hit {
                obs: o,
                first_party: is_first_party(&self.keys[i], gateway_id),
            });
        }
        (hits, skipped)
    }
}

/// `(provider, id)` of the entries whose `thinking.efforts` had a `default` level rejected.
fn default_rejected(out: &BuildOutput) -> HashSet<(&str, &str)> {
    out.report
        .rejections
        .iter()
        .filter(|d| d.field == "thinking.efforts" && d.value == json!("default"))
        .map(|d| (d.provider.as_str(), d.id.as_str()))
        .collect()
}

/// Groq-style `reasoning_effort` accepts `none` and `default`: an on/off switch. The contract
/// has no `default` level, so after rejecting it only `off` is left, and `effort` with `[off]`
/// would say the model can never think. It is a `toggle` that can be disabled. (An effort
/// list that is `[off]` because the source says only `none` is left alone: that is no switch.)
fn as_toggle(
    o: &Observation,
    rejected: &HashSet<(&str, &str)>,
) -> Option<(Observation, MergeDiagnostic)> {
    let t = o.fields.thinking.as_ref()?;
    if t.mode != ThinkingMode::Effort
        || t.efforts != [Effort::Off]
        || !rejected.contains(&(o.fields.provider.as_str(), o.fields.id.as_str()))
    {
        return None;
    }
    let mut options = t.options.clone().unwrap_or(ThinkingOptions {
        can_disable: None,
        min_budget_tokens: None,
        max_budget_tokens: None,
        dynamic_budget: None,
    });
    options.can_disable = Some(true);
    let mut n = o.clone();
    n.fields.thinking = Some(Thinking {
        mode: ThinkingMode::Toggle,
        efforts: Vec::new(),
        options: Some(options),
        default_level: t.default_level,
        effort_map: None,
    });
    let note = MergeDiagnostic {
        kind: "thinking-toggle".into(),
        key: None,
        source_id: o.source.source_id.clone(),
        provider: o.fields.provider.clone(),
        id: o.fields.id.clone(),
        field: "thinking".into(),
        value: json!({"efforts": ["off"], "rejected": "default"}),
        reason: "reasoning_effort accepts only none and default: an on/off switch, recorded as toggle (canDisable) instead of an effort list with only `off`".into(),
    };
    Some((n, note))
}

fn all<V>(_: &V) -> bool {
    true
}

fn degrades(l: &Lifecycle) -> bool {
    matches!(
        l.status,
        LifecycleStatus::Preview | LifecycleStatus::Deprecated | LifecycleStatus::Retired
    )
}

fn k_cost(c: &Cost) -> String {
    format!(
        "{:.6}/{:.6}",
        c.input.unwrap_or(0.0),
        c.output.unwrap_or(0.0)
    )
}
fn k_dbg<T: std::fmt::Debug>(v: &T) -> String {
    format!("{v:?}")
}
fn k_json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_default()
}
fn k_thinking(t: &Thinking) -> String {
    format!("{:?}/{:?}", t.mode, t.efforts)
}

/// The facts about one gateway row the field rules read.
struct Row<'a> {
    gateway: &'a Observation,
    source_id: &'a str,
    ph: &'a Placeholders,
    refs: &'a [Hits<'a>],
}

impl Row<'_> {
    fn gw<V>(
        &self,
        field: FieldName,
        get: impl Fn(&Fields) -> Option<V>,
        key: impl Fn(&V) -> String,
    ) -> Option<(&str, GwCand<V>)> {
        let value = get(&self.gateway.fields)?;
        Some((
            self.source_id,
            GwCand {
                key: key(&value),
                placeholder: self.ph.is_placeholder(&self.gateway.fields, field),
                value,
            },
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn stage<V>(
        &self,
        source: &str,
        first_party_only: bool,
        level: Option<Level>,
        accept: fn(&V) -> bool,
        get: impl Fn(&Fields) -> Option<V>,
        key: impl Fn(&V) -> String,
    ) -> Option<Stage<V>> {
        let h = self.refs.iter().find(|h| h.source == source)?;
        let tier = h.tier?;
        let cands = h
            .hits
            .iter()
            .filter_map(|hit| {
                let value = get(&hit.obs.fields)?;
                Some(RefCand {
                    key: key(&value),
                    value,
                    first_party: hit.first_party,
                    tier,
                })
            })
            .collect();
        Some(Stage {
            source: source.to_owned(),
            cands,
            first_party_only,
            level,
            accept,
        })
    }
}

/// Accumulates one model's provenance and the report's per-field tallies.
struct Builder<'a> {
    provenance: BTreeMap<FieldName, FieldProvenance>,
    stats: &'a mut BTreeMap<FieldName, FieldStats>,
}

impl Builder<'_> {
    fn settle<V: Clone>(
        &mut self,
        name: FieldName,
        chain: &str,
        r: Resolved<V>,
        fallback: Option<Fallback<V>>,
        confidence: impl Fn(&Win<V>) -> Confidence,
    ) -> Option<V> {
        let o: Outcome<V> = finish(chain, r, fallback, confidence);
        let s = self.stats.entry(name).or_default();
        *s.winner_rules.entry(o.rule).or_default() += 1;
        s.conflicts += usize::from(o.conflict);
        s.unresolved += usize::from(o.value.is_none() || o.level == Level::Unresolved);
        s.placeholder_alternates += o.placeholder_alternates;
        if let Some(p) = o.provenance {
            self.provenance.insert(name, p);
        }
        o.value
    }
}

fn default_confidence<V>(w: &Win<V>) -> Confidence {
    w.level.confidence()
}

fn tally(c: &mut TierCounts, tier: Option<MatchTier>) {
    match tier {
        Some(MatchTier::ExactFull) => c.t1 += 1,
        Some(MatchTier::ExactLeaf) => c.t2 += 1,
        Some(MatchTier::NormalizedLeaf) => c.t3 += 1,
        Some(MatchTier::DateStripped) => c.t4 += 1,
        None => c.unmatched += 1,
    }
}

fn pct(n: usize, of: usize) -> f64 {
    if of == 0 {
        0.0
    } else {
        (n as f64 / of as f64 * 10_000.0).round() / 100.0
    }
}

/// Merges the gateway sources in `outs` with the reference sources in `outs`.
pub fn merge_catalog(outs: &[&BuildOutput]) -> Result<MergeOutput, CatalogError> {
    merge_catalog_with(outs, None)
}

/// [`merge_catalog`] with `configured` overrides on top of every source.
pub fn merge_catalog_with(
    outs: &[&BuildOutput],
    overrides: Option<&Overrides>,
) -> Result<MergeOutput, CatalogError> {
    let mut gateways: Vec<&BuildOutput> = Vec::new();
    let mut refs: Vec<RefSource<'_>> = Vec::new();
    let mut sources = Vec::new();
    for out in outs {
        let id = out.report.source.descriptor.source_id.as_str();
        let source = Source::parse(id)
            .ok_or_else(|| CatalogError::Malformed(format!("unknown source `{id}`")))?;
        let role = source.role();
        sources.push(MergeSourceInfo {
            source_id: id.to_owned(),
            role,
            observations: out.observations.len(),
        });
        match role {
            SourceRole::Gateway => gateways.push(out),
            SourceRole::Reference => refs.push(RefSource::new(out)),
            SourceRole::Probe => {}
        }
    }
    if gateways.is_empty() {
        return Err(CatalogError::Malformed(
            "nothing to merge: no gateway source (cliproxyapi-cache, omp-live:*, router-for-me.models, dsh, freellmapi)".into(),
        ));
    }
    // models.dev before modelparams.dev: stage order never depends on argument order.
    refs.sort_by_key(|r| r.id != MODELS_DEV);

    let mut stats: BTreeMap<FieldName, FieldStats> = BTreeMap::new();
    let mut report = MergeReport {
        sources,
        models: 0,
        gateways: Vec::new(),
        fields: BTreeMap::new(),
        placeholder_winners: PlaceholderWinners::default(),
        cost: CostStats::default(),
        reference_auth_variants_skipped: 0,
        max_tokens_study: None,
        overrides: None,
        diagnostics: Vec::new(),
    };
    for r in &refs {
        report.diagnostics.extend(r.notes.iter().cloned());
    }
    let ov_source = overrides.map(Overrides::descriptor);
    let mut models_overridden = 0;
    let mut seen_keys: BTreeMap<String, ()> = BTreeMap::new();
    let mut models = Vec::new();
    for out in gateways {
        let gid = out.report.source.descriptor.source_id.as_str();
        let ph = Placeholders::detect(out.observations.iter().map(|o| &o.fields));
        let rejected = default_rejected(out);
        let mut per_ref: BTreeMap<String, TierCounts> = refs
            .iter()
            .map(|r| (r.id.to_owned(), TierCounts::default()))
            .collect();
        let mut gr = GatewayReport {
            source_id: gid.to_owned(),
            observations: out.observations.len(),
            retired_skipped: 0,
            rows: 0,
            matched: 0,
            unmatched: 0,
            matched_pct: 0.0,
            per_reference: BTreeMap::new(),
            placeholders: PlaceholderSet {
                pairs: ph.pairs.iter().copied().collect(),
                efforts: ph.efforts.iter().cloned().collect(),
            },
            placeholder_rows: BTreeMap::new(),
        };
        for g in &out.observations {
            if g.fields
                .lifecycle
                .as_ref()
                .is_some_and(|l| l.status == LifecycleStatus::Retired)
            {
                gr.retired_skipped += 1;
                continue;
            }
            let dup_auth = g.fields.auth_type.is_some()
                && out
                    .observations
                    .iter()
                    .filter(|o| {
                        o.fields.provider == g.fields.provider && o.fields.id == g.fields.id
                    })
                    .count()
                    > 1;
            let mut key = format!("{gid}:{}/{}", g.fields.provider, g.fields.id);
            if dup_auth && let Some(a) = g.fields.auth_type {
                key.push('#');
                key.push_str(auth_name(a));
            }
            if seen_keys.insert(key.clone(), ()).is_some() {
                report.diagnostics.push(MergeDiagnostic {
                    kind: "duplicate-key".into(),
                    key: Some(key),
                    source_id: gid.to_owned(),
                    provider: g.fields.provider.clone(),
                    id: g.fields.id.clone(),
                    field: "key".into(),
                    value: Value::Null,
                    reason: "another row of this source has the same key; this one was not merged"
                        .into(),
                });
                continue;
            }
            let mut hits = Vec::new();
            for r in &refs {
                let (h, skipped) = r.hits(&g.fields.id);
                report.reference_auth_variants_skipped += skipped;
                tally(
                    per_ref.get_mut(r.id).expect("every reference is listed"),
                    h.tier,
                );
                hits.push(h);
            }
            let toggled = as_toggle(g, &rejected);
            if let Some((_, note)) = &toggled {
                report.diagnostics.push(note.clone());
            }
            let row = Row {
                gateway: toggled.as_ref().map_or(g, |(n, _)| n),
                source_id: gid,
                ph: &ph,
                refs: &hits,
            };
            let ov = overrides.and_then(|o| o.get(&key));
            models_overridden += usize::from(ov.is_some());
            let model = merge_row(
                &row,
                key,
                (ov, ov_source.as_ref()),
                out,
                &refs,
                &mut stats,
                &mut report.diagnostics,
            );
            // report
            gr.rows += 1;
            if hits.iter().any(|h| h.tier.is_some()) {
                gr.matched += 1;
            }
            for f in [
                FieldName::ContextWindow,
                FieldName::MaxInputTokens,
                FieldName::MaxTokens,
                FieldName::Thinking,
            ] {
                let own = match f {
                    FieldName::ContextWindow => g.fields.context_window.is_some(),
                    FieldName::MaxInputTokens => g.fields.max_input_tokens.is_some(),
                    FieldName::MaxTokens => g.fields.max_tokens.is_some(),
                    _ => g.fields.thinking.is_some(),
                };
                if own && ph.is_placeholder(&g.fields, f) {
                    *gr.placeholder_rows.entry(f).or_default() += 1;
                    if model
                        .provenance
                        .fields
                        .get(&f)
                        .is_some_and(|p| p.source == gid)
                    {
                        let w = &mut report.placeholder_winners;
                        match f {
                            FieldName::ContextWindow => w.context_window += 1,
                            FieldName::MaxInputTokens => w.max_input_tokens += 1,
                            FieldName::MaxTokens => w.max_tokens += 1,
                            _ => w.thinking += 1,
                        }
                        w.total += 1;
                    }
                }
            }
            if let Some(c) = &model.fields.cost {
                match c.status {
                    CostStatus::Known => report.cost.known += 1,
                    CostStatus::Free => report.cost.free += 1,
                    CostStatus::Unknown => report.cost.unknown += 1,
                }
                let from_gateway = model
                    .provenance
                    .fields
                    .get(&FieldName::Cost)
                    .is_some_and(|p| p.source == gid);
                let zero =
                    c.status == CostStatus::Free || (c.input == Some(0.0) && c.output == Some(0.0));
                if from_gateway && zero {
                    report.cost.gateway_zero_winners += 1;
                }
            }
            models.push(model);
        }
        gr.unmatched = gr.rows - gr.matched;
        gr.matched_pct = pct(gr.matched, gr.rows);
        gr.per_reference = per_ref;
        report.gateways.push(gr);
    }
    if let (Some(md), Some(mp)) = (
        refs.iter().find(|r| r.id == MODELS_DEV),
        refs.iter().find(|r| r.id == MODELPARAMS),
    ) {
        report.max_tokens_study = Some(max_tokens_study(md, mp));
    }
    if let Some(o) = overrides {
        for key in o.keys().filter(|k| !seen_keys.contains_key(*k)) {
            report.diagnostics.push(MergeDiagnostic {
                kind: "unmatched-override".into(),
                key: Some(key.to_owned()),
                source_id: OVERRIDES_SOURCE_ID.into(),
                provider: String::new(),
                id: String::new(),
                field: "key".into(),
                value: Value::Null,
                reason:
                    "no merged model has this key (spelled `<gatewaySourceId>:<provider>/<id>`)"
                        .into(),
            });
        }
        report.overrides = Some(OverridesReport {
            origin: o.origin().to_owned(),
            sha256: o.sha256().to_owned(),
            entries: o.len(),
            models_overridden,
        });
    }
    report.models = models.len();
    report.fields = stats;
    Ok(MergeOutput { models, report })
}

fn auth_name(a: AuthType) -> &'static str {
    match a {
        AuthType::ApiKey => "api_key",
        AuthType::Subscription => "subscription",
        AuthType::Oauth => "oauth",
        AuthType::None => "none",
    }
}

fn merge_row(
    row: &Row<'_>,
    key: String,
    (ov, ov_source): (Option<&OverrideFields>, Option<&ProvenanceSource>),
    out: &BuildOutput,
    refs: &[RefSource<'_>],
    stats: &mut BTreeMap<FieldName, FieldStats>,
    diags: &mut Vec<MergeDiagnostic>,
) -> Model {
    let g = &row.gateway.fields;
    let mut f = Fields::empty(g.provider.clone(), g.id.clone());
    f.auth_type = g.auth_type;
    let o = ov.cloned().unwrap_or_default();
    f.kind = o.kind.or(g.kind);
    let mut b = Builder {
        provenance: BTreeMap::new(),
        stats,
    };
    let md = MODELS_DEV;
    let mp = MODELPARAMS;

    // name, canonicalId
    let mut stages: Vec<Stage<String>> = Vec::new();
    stages.extend(row.stage(md, true, None, all, |x| x.name.clone(), |v| v.clone()));
    f.name = b.settle(
        FieldName::Name,
        NAME,
        configured(
            resolve(
                row.gw(FieldName::Name, |x| x.name.clone(), |v| v.clone()),
                &stages,
            ),
            o.name.clone(),
            OVERRIDES_SOURCE_ID,
            |v: &String| v.clone(),
        ),
        None,
        default_confidence,
    );
    let mut stages: Vec<Stage<String>> = Vec::new();
    stages.extend(row.stage(
        md,
        false,
        None,
        all,
        |x| x.canonical_id.clone(),
        |v| v.clone(),
    ));
    f.canonical_id = b.settle(
        FieldName::CanonicalId,
        CANONICAL,
        configured(
            resolve(
                row.gw(
                    FieldName::CanonicalId,
                    |x| x.canonical_id.clone(),
                    |v| v.clone(),
                ),
                &stages,
            ),
            o.canonical_id.clone(),
            OVERRIDES_SOURCE_ID,
            |v: &String| v.clone(),
        ),
        None,
        |_| Confidence::Declared,
    );

    // limits
    let limit = |b: &mut Builder<'_>,
                 name: FieldName,
                 chain: &str,
                 get: fn(&Fields) -> Option<u64>,
                 with_range_max: bool,
                 over: Option<u64>| {
        let mut stages = Vec::new();
        stages.extend(row.stage(md, false, None, all, get, u64::to_string));
        if with_range_max {
            stages.extend(row.stage(mp, false, Some(Level::RangeMax), all, get, u64::to_string));
        }
        b.settle(
            name,
            chain,
            configured(
                resolve(row.gw(name, get, u64::to_string), &stages),
                over,
                OVERRIDES_SOURCE_ID,
                u64::to_string,
            ),
            None,
            default_confidence,
        )
    };
    f.context_window = limit(
        &mut b,
        FieldName::ContextWindow,
        LIMITS,
        |x| x.context_window,
        false,
        o.context_window,
    );
    f.max_input_tokens = limit(
        &mut b,
        FieldName::MaxInputTokens,
        LIMITS,
        |x| x.max_input_tokens,
        false,
        o.max_input_tokens,
    );
    f.max_tokens = limit(
        &mut b,
        FieldName::MaxTokens,
        MAX_TOKENS,
        |x| x.max_tokens,
        true,
        o.max_tokens,
    );

    // cost: a gateway's 0 is never "free"; refs may say free (models.dev publishes real zeros)
    let gw_cost = |x: &Fields| {
        x.cost
            .clone()
            .filter(|c| c.status == CostStatus::Known && c.input.is_some())
    };
    let ref_cost = |x: &Fields| {
        x.cost.clone().filter(|c| {
            matches!(c.status, CostStatus::Known | CostStatus::Free) && c.input.is_some()
        })
    };
    let mut stages = Vec::new();
    stages.extend(row.stage(md, false, None, all, ref_cost, k_cost));
    let unknown = Cost {
        status: CostStatus::Unknown,
        input: None,
        output: None,
        cache_read: None,
        cache_write: None,
        tiers: Vec::new(),
    };
    f.cost = b.settle(
        FieldName::Cost,
        COST,
        configured(
            resolve(row.gw(FieldName::Cost, gw_cost, k_cost), &stages),
            o.cost.clone(),
            OVERRIDES_SOURCE_ID,
            k_cost,
        ),
        Some(Fallback {
            value: unknown,
            source: NO_SOURCE.to_owned(),
            level: Level::Unresolved,
            key: "unknown".into(),
        }),
        default_confidence,
    );

    // modalities, tools
    let mut stages: Vec<Stage<Vec<Modality>>> = Vec::new();
    stages.extend(row.stage(md, true, None, all, |x| x.input.clone(), k_dbg));
    f.input = b.settle(
        FieldName::Input,
        INPUT,
        configured(
            resolve(
                row.gw(FieldName::Input, |x| x.input.clone(), k_dbg),
                &stages,
            ),
            o.input.clone(),
            OVERRIDES_SOURCE_ID,
            k_dbg,
        ),
        None,
        default_confidence,
    );
    let mut stages: Vec<Stage<Vec<Modality>>> = Vec::new();
    stages.extend(row.stage(md, true, None, all, |x| x.output.clone(), k_dbg));
    f.output = b.settle(
        FieldName::Output,
        OUTPUT,
        configured(
            resolve(
                row.gw(FieldName::Output, |x| x.output.clone(), k_dbg),
                &stages,
            ),
            o.output.clone(),
            OVERRIDES_SOURCE_ID,
            k_dbg,
        ),
        None,
        default_confidence,
    );
    let mut stages: Vec<Stage<bool>> = Vec::new();
    stages.extend(row.stage(md, false, None, all, |x| x.supports_tools, k_dbg));
    f.supports_tools = b.settle(
        FieldName::SupportsTools,
        TOOLS,
        configured(
            resolve(
                row.gw(FieldName::SupportsTools, |x| x.supports_tools, k_dbg),
                &stages,
            ),
            o.supports_tools,
            OVERRIDES_SOURCE_ID,
            k_dbg,
        ),
        None,
        default_confidence,
    );

    // reasoning: modelparams (a reasoning-group param) before models.dev
    let mut stages: Vec<Stage<bool>> = Vec::new();
    stages.extend(row.stage(mp, false, None, all, |x| x.reasoning, k_dbg));
    stages.extend(row.stage(md, false, None, all, |x| x.reasoning, k_dbg));
    f.reasoning = b.settle(
        FieldName::Reasoning,
        REASONING,
        configured(
            resolve(
                row.gw(FieldName::Reasoning, |x| x.reasoning, k_dbg),
                &stages,
            ),
            o.reasoning,
            OVERRIDES_SOURCE_ID,
            k_dbg,
        ),
        None,
        |_| Confidence::Inferred,
    );

    // thinking: the effort ladder decides; first-party references only
    let think = |x: &Fields| {
        x.thinking
            .clone()
            .filter(|t| t.mode != ThinkingMode::Unknown)
    };
    let mut stages: Vec<Stage<Thinking>> = Vec::new();
    stages.extend(row.stage(mp, true, None, all, think, k_thinking));
    stages.extend(row.stage(md, true, None, all, think, k_thinking));
    f.thinking = b.settle(
        FieldName::Thinking,
        THINKING,
        configured(
            resolve(row.gw(FieldName::Thinking, think, k_thinking), &stages),
            o.thinking.clone(),
            OVERRIDES_SOURCE_ID,
            k_thinking,
        ),
        None,
        default_confidence,
    );

    // lifecycle: a reference can only degrade `live`
    let life = |x: &Fields| {
        x.lifecycle
            .clone()
            .filter(|l| l.status != LifecycleStatus::Unknown)
    };
    let gw_life = |x: &Fields| life(x).filter(degrades);
    let mut stages: Vec<Stage<Lifecycle>> = Vec::new();
    stages.extend(row.stage(mp, false, None, degrades, life, k_dbg));
    stages.extend(row.stage(md, false, None, degrades, life, k_dbg));
    f.lifecycle = b.settle(
        FieldName::Lifecycle,
        LIFECYCLE,
        configured(
            resolve(row.gw(FieldName::Lifecycle, gw_life, k_dbg), &stages),
            o.lifecycle.clone(),
            OVERRIDES_SOURCE_ID,
            k_dbg,
        ),
        Some(Fallback {
            value: Lifecycle {
                status: LifecycleStatus::Live,
                replacement: None,
                shutdown_on: None,
            },
            source: row.source_id.to_owned(),
            level: Level::GatewayListed,
            key: "live".into(),
        }),
        default_confidence,
    );

    // acceptedParams: modelparams only
    let mut stages: Vec<Stage<Vec<AcceptedParam>>> = Vec::new();
    stages.extend(row.stage(mp, false, None, all, |x| x.accepted_params.clone(), k_json));
    f.accepted_params = b.settle(
        FieldName::AcceptedParams,
        PARAMS,
        configured(
            resolve(None::<(&str, GwCand<Vec<AcceptedParam>>)>, &stages),
            o.accepted_params.clone(),
            OVERRIDES_SOURCE_ID,
            k_json,
        ),
        None,
        |w| {
            if w.level == Level::RefFirstParty
                && matches!(w.tier, Some(MatchTier::ExactFull | MatchTier::ExactLeaf))
            {
                Confidence::Declared
            } else {
                Confidence::Inferred
            }
        },
    );

    // a price the winning models.dev entry has but the contract's `Cost` cannot carry
    if let Some(c) = &f.cost
        && b.provenance
            .get(&FieldName::Cost)
            .is_some_and(|p| p.source == MODELS_DEV)
    {
        let winner = k_cost(c);
        for (h, r) in row
            .refs
            .iter()
            .zip(refs)
            .filter(|(h, _)| h.source == MODELS_DEV)
        {
            for hit in &h.hits {
                let f = &hit.obs.fields;
                if !f.cost.as_ref().is_some_and(|c| k_cost(c) == winner) {
                    continue;
                }
                for d in r
                    .unmodelled
                    .get(&(f.provider.as_str(), f.id.as_str()))
                    .into_iter()
                    .flatten()
                {
                    diags.push(MergeDiagnostic {
                        kind: "unmodelled".into(),
                        key: Some(key.clone()),
                        source_id: d.source_id.clone(),
                        provider: d.provider.clone(),
                        id: d.id.clone(),
                        field: d.field.clone(),
                        value: d.value.clone(),
                        reason: d.reason.clone(),
                    });
                }
            }
        }
    }
    let mut sources: Vec<ProvenanceSource> = vec![out.report.source.descriptor.clone()];
    sources.extend(ov.and(ov_source).cloned());
    let mut references = Vec::new();
    for (h, r) in row.refs.iter().zip(refs) {
        if h.hits.is_empty() {
            continue;
        }
        sources.push(r.out.report.source.descriptor.clone());
        let tier = h.tier.expect("hits imply a tier");
        for hit in &h.hits {
            references.push(ModelRef {
                source_id: h.source.to_owned(),
                provider: hit.obs.fields.provider.clone(),
                id: hit.obs.fields.id.clone(),
                tier,
                first_party: Some(hit.first_party),
            });
        }
    }
    Model {
        record_type: RecordType::Model,
        key,
        fields: f,
        references,
        provenance: ModelProvenance {
            sources,
            fields: b.provenance,
        },
    }
}

/// §10.2: how often does modelparams `range.max` equal models.dev `limit.output`?
fn max_tokens_study(md: &RefSource<'_>, mp: &RefSource<'_>) -> MaxTokensStudy {
    let mut s = MaxTokensStudy::default();
    for o in &mp.out.observations {
        let Some(range_max) = o.fields.max_tokens else {
            continue;
        };
        if !preferred_auth(o.fields.auth_type) {
            continue;
        }
        s.modelparams_with_max_tokens += 1;
        let Some((_, idx)) = md.index.lookup_indices(&o.fields.id) else {
            s.no_models_dev_match += 1;
            continue;
        };
        let cands: Vec<(u64, bool)> = idx
            .iter()
            .filter_map(|&i| {
                Some((
                    md.out.observations[i].fields.max_tokens?,
                    is_first_party(&md.keys[i], &o.fields.id),
                ))
            })
            .collect();
        if cands.is_empty() {
            s.models_dev_has_no_output += 1;
            continue;
        }
        let first_party: Vec<u64> = cands.iter().filter(|c| c.1).map(|c| c.0).collect();
        let pool = if first_party.is_empty() {
            cands.iter().map(|c| c.0).collect()
        } else {
            first_party
        };
        let mut counts: BTreeMap<u64, usize> = BTreeMap::new();
        for v in &pool {
            *counts.entry(*v).or_default() += 1;
        }
        let Some((&value, &n)) = counts
            .iter()
            .max_by_key(|&(v, n)| (*n, std::cmp::Reverse(*v)))
        else {
            continue;
        };
        if n * 2 <= pool.len() {
            s.models_dev_ambiguous += 1;
            continue;
        }
        s.comparable += 1;
        match range_max.cmp(&value) {
            std::cmp::Ordering::Equal => s.equal += 1,
            ord => {
                if ord == std::cmp::Ordering::Greater {
                    s.range_max_greater += 1;
                } else {
                    s.range_max_lower += 1;
                }
                *s.disagreeing_by_provider
                    .entry(o.fields.provider.clone())
                    .or_default() += 1;
            }
        }
    }
    s.equal_pct = pct(s.equal, s.comparable);
    s
}

/// For tests and reports: the rule ids this module uses.
pub fn rule_chains() -> Value {
    json!({
        "limits": LIMITS, "maxTokens": MAX_TOKENS, "cost": COST, "name": NAME,
        "canonicalId": CANONICAL, "input": INPUT, "output": OUTPUT, "supportsTools": TOOLS,
        "reasoning": REASONING, "thinking": THINKING, "lifecycle": LIFECYCLE,
        "acceptedParams": PARAMS,
    })
}

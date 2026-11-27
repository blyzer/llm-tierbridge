//! `tierbridge export omp`: merged catalog rows of `omp-live:<provider>` -> omp `modelOverrides`.
//!
//! omp fills a discovered gateway model (`discovery: {type: openai-models-list}`) with stand-ins
//! (128000/32768, zero prices, a full effort ladder). The merged catalog knows better for a part of
//! those rows; this turns that knowledge into the `providers.<p>.modelOverrides` omp reads from
//! `models.yml` (`ModelPatch`, `packages/coding-agent/src/config/model-patch.ts`).
//!
//! A field is exported only when
//!   * its winner is real evidence: the field has a source other than `none` (a placeholder never
//!     wins, see [`crate::precedence`]), and not the gateway's own `omp-live:<provider>` value, which
//!     is what omp shows already, and
//!   * it differs from what omp shows now, when the caller hands in the live roster (`--current`).
//!
//! The output is deterministic (no clock), so exporting twice gives the same bytes.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::Deserialize;
use serde::de::IgnoredAny;

use crate::models_dev::sha256_hex;
use crate::omp_error::OmpError;
use crate::precedence::NO_SOURCE;

/// omp's own effort ladder (`Effort` in `packages/catalog/src/effort.ts`), least to most intensive.
/// The catalog's `off` is not an omp effort.
const OMP_EFFORTS: [&str; 6] = ["minimal", "low", "medium", "high", "xhigh", "max"];
/// The modalities `ModelPatch.input` takes, in the order omp lists them.
const OMP_INPUT: [&str; 2] = ["text", "image"];
/// Tolerance when comparing prices ($ per million tokens).
const PRICE_EPSILON: f64 = 1e-9;

// ---- catalog.json rows (only what the export reads; unknown fields are ignored) ----

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Row {
    key: String,
    provider: String,
    id: String,
    context_window: Option<u64>,
    max_tokens: Option<u64>,
    cost: Option<CostRef>,
    input: Option<Vec<String>>,
    reasoning: Option<bool>,
    thinking: Option<ThinkingRef>,
    #[serde(default)]
    references: Vec<IgnoredAny>,
    provenance: RowProvenance,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CostRef {
    status: String,
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
    #[serde(default)]
    tiers: Vec<IgnoredAny>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThinkingRef {
    mode: String,
    #[serde(default)]
    efforts: Vec<String>,
    default_level: Option<String>,
    effort_map: Option<BTreeMap<String, Option<String>>>,
}

#[derive(Deserialize)]
struct RowProvenance {
    #[serde(default)]
    sources: Vec<SourceRef>,
    #[serde(default)]
    fields: BTreeMap<String, FieldRef>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceRef {
    source_id: String,
    revision: Option<String>,
}

#[derive(Deserialize)]
struct FieldRef {
    source: String,
    conflict: Option<bool>,
    #[serde(default)]
    alternates: Vec<AlternateRef>,
}

#[derive(Deserialize)]
struct AlternateRef {
    source: String,
}

// ---- what omp shows now (`omp models <provider> --json`) ----

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CurrentCost {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
}

/// One row of the live roster.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CurrentModel {
    pub context_window: Option<u64>,
    pub max_tokens: Option<u64>,
    pub pricing_status: Option<String>,
    pub cost: Option<CurrentCost>,
    pub input: Option<Vec<String>>,
    pub reasoning: Option<bool>,
    /// The effort ladder omp lists (`thinking` is an array of effort names there).
    pub thinking: Option<Vec<String>>,
}

/// The roster `omp models <provider> --json` prints, keyed by model id.
#[derive(Debug, Clone, PartialEq)]
pub struct CurrentRoster {
    pub provider: String,
    pub models: BTreeMap<String, CurrentModel>,
}

#[derive(Deserialize)]
struct RosterFile {
    models: Vec<RosterRow>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RosterRow {
    provider: String,
    id: String,
    context_window: Option<u64>,
    max_tokens: Option<u64>,
    pricing_status: Option<String>,
    cost: Option<RosterCost>,
    input: Option<Vec<String>>,
    reasoning: Option<bool>,
    thinking: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RosterCost {
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
}

impl CurrentRoster {
    /// Reads the bytes of `omp models <provider> --json`; the provider is the one its rows name.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let file: RosterFile = serde_json::from_slice(bytes)
            .map_err(|e| format!("not the output of `omp models <provider> --json`: {e}"))?;
        let Some(first) = file.models.first() else {
            return Err("the roster has no models".to_owned());
        };
        let provider = first.provider.clone();
        let mut models = BTreeMap::new();
        for row in file.models {
            if row.provider != provider {
                return Err(format!(
                    "rows of two providers (`{provider}` and `{}`); pass one roster per provider",
                    row.provider
                ));
            }
            models.insert(
                row.id,
                CurrentModel {
                    context_window: row.context_window,
                    max_tokens: row.max_tokens,
                    pricing_status: row.pricing_status,
                    cost: row.cost.map(|c| CurrentCost {
                        input: c.input,
                        output: c.output,
                        cache_read: c.cache_read,
                        cache_write: c.cache_write,
                    }),
                    input: row.input,
                    reasoning: row.reasoning,
                    thinking: row.thinking,
                },
            );
        }
        Ok(Self { provider, models })
    }
}

// ---- options and results ----

#[derive(Debug, Clone, Default)]
pub struct ExportOptions {
    /// Providers to export (`easycliproxy`, `dsh`), in output order.
    pub providers: Vec<String>,
    /// Live rosters (one per provider, optional). Without one, only the gateway-independent
    /// rule applies: values the gateway itself reported are not exported.
    pub current: Vec<CurrentRoster>,
    /// Also export `reasoning` and `thinking` (omp `ThinkingConfig`).
    pub with_thinking: bool,
}

/// What happened to one field over the models considered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FieldCount {
    /// Written to the overrides.
    pub emitted: usize,
    /// Real evidence, but omp shows exactly that already (needs `--current`).
    pub unchanged: usize,
    /// The winner is the gateway's own value: omp shows it already.
    pub gateway: usize,
    /// No evidence (winner `none`, or a value that cannot be expressed).
    pub none: usize,
}

/// Models that keep the gateway's stand-ins, and why.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Remaining {
    /// Models whose context window or max tokens is still a placeholder.
    pub placeholder_models: usize,
    pub context_window: usize,
    pub max_tokens: usize,
    /// Reason -> models (each model counted once, under its first reason).
    pub reasons: BTreeMap<&'static str, usize>,
    /// Models whose price is still unknown (omp keeps showing zeros with status `unknown`).
    pub cost_unknown: usize,
}

pub const REASON_UNMATCHED: &str = "no reference entry matched the model id";
pub const REASON_DISAGREE: &str = "references disagree (no first-party value, no strict majority)";
pub const REASON_NO_LIMIT: &str = "matched, but no reference carries the limit";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderStats {
    pub provider: String,
    /// Catalog rows of `omp-live:<provider>`.
    pub rows: usize,
    /// Rows left out because another row of the provider has the same id.
    pub duplicates: usize,
    /// With `--current`: models in the live roster.
    pub roster: Option<usize>,
    /// With `--current`: catalog rows the live roster does not list (not exported).
    pub not_in_roster: usize,
    /// With `--current`: roster models the catalog has no row for.
    pub roster_not_in_catalog: usize,
    /// Models considered (the rows, or the rows present in the live roster).
    pub considered: usize,
    pub with_overrides: usize,
    pub context_window: FieldCount,
    pub max_tokens: FieldCount,
    pub cost: FieldCount,
    pub input: FieldCount,
    pub reasoning: FieldCount,
    pub thinking: FieldCount,
    /// Exported prices that are zero (`status: free`).
    pub cost_free: usize,
    /// Known price without both `input` and `output`: not exported (a missing half would show as 0).
    pub cost_partial: usize,
    /// Exported prices that have price tiers (tiers are not exported).
    pub cost_tiers_dropped: usize,
    /// Thinking in a mode omp's `ThinkingConfig` cannot express (toggle, budget, none).
    pub thinking_inexpressible: usize,
    pub remaining: Remaining,
}

impl ProviderStats {
    /// `contextWindow 248, maxTokens 212, ...` (only the exported fields).
    pub fn field_summary(&self, with_thinking: bool) -> String {
        let mut parts = vec![
            format!("contextWindow {}", self.context_window.emitted),
            format!("maxTokens {}", self.max_tokens.emitted),
            format!("cost {}", self.cost.emitted),
            format!("input {}", self.input.emitted),
        ];
        if with_thinking {
            parts.push(format!("reasoning {}", self.reasoning.emitted));
            parts.push(format!("thinking {}", self.thinking.emitted));
        }
        parts.join(", ")
    }
}

#[derive(Debug, Clone)]
pub struct OmpExport {
    /// The `overrides.yml` text.
    pub yaml: String,
    pub stats: Vec<ProviderStats>,
    /// `sha256` of the catalog bytes.
    pub catalog_sha256: String,
}

// ---- the overrides of one model ----

#[derive(Debug, Default, PartialEq)]
struct OvCost {
    input: f64,
    output: f64,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
}

#[derive(Debug, Default, PartialEq)]
struct OvThinking {
    efforts: Vec<&'static str>,
    default_level: Option<&'static str>,
    effort_map: Vec<(&'static str, String)>,
}

#[derive(Debug, Default, PartialEq)]
struct Ov {
    context_window: Option<u64>,
    max_tokens: Option<u64>,
    cost: Option<OvCost>,
    input: Option<Vec<&'static str>>,
    reasoning: Option<bool>,
    thinking: Option<OvThinking>,
}

impl Ov {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// What the catalog says about one field of one gateway row.
enum Winner {
    /// A reference (or a configured override) settled it.
    Evidence,
    /// The gateway's own value won: omp shows it already.
    Gateway,
    /// Nothing settled it.
    None,
}

fn winner(row: &Row, field: &str, gateway_id: &str) -> Winner {
    match row.provenance.fields.get(field) {
        None => Winner::None,
        Some(f) if f.source == NO_SOURCE || f.source.is_empty() => Winner::None,
        Some(f) if f.source == gateway_id => Winner::Gateway,
        Some(_) => Winner::Evidence,
    }
}

fn price_eq(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => (a - b).abs() <= PRICE_EPSILON,
        (None, None) => true,
        _ => false,
    }
}

fn same_set(a: &[&str], b: &[String]) -> bool {
    let a: BTreeSet<&str> = a.iter().copied().collect();
    let b: BTreeSet<&str> = b.iter().map(String::as_str).collect();
    a == b
}

/// omp's price patch of a row, `None` when it is not exportable (counted in `stats`).
fn cost_patch(cost: &CostRef, stats: &mut ProviderStats) -> Option<OvCost> {
    match cost.status.as_str() {
        "free" => Some(OvCost {
            input: 0.0,
            output: 0.0,
            cache_read: cost.cache_read,
            cache_write: cost.cache_write,
        }),
        "known" => match (cost.input, cost.output) {
            (Some(input), Some(output)) => Some(OvCost {
                input,
                output,
                cache_read: cost.cache_read,
                cache_write: cost.cache_write,
            }),
            _ => {
                stats.cost_partial += 1;
                None
            }
        },
        _ => None,
    }
}

fn thinking_patch(t: &ThinkingRef) -> Option<OvThinking> {
    if t.mode != "effort" {
        return None;
    }
    let map = t.effort_map.as_ref();
    // `Some(None)` in the map: the route does not accept that level.
    let accepted = |e: &str| !matches!(map.and_then(|m| m.get(e)), Some(None));
    let efforts: Vec<&'static str> = OMP_EFFORTS
        .iter()
        .copied()
        .filter(|e| t.efforts.iter().any(|x| x == e) && accepted(e))
        .collect();
    if efforts.is_empty() {
        return None;
    }
    let default_level = t
        .default_level
        .as_deref()
        .and_then(|d| efforts.iter().copied().find(|e| *e == d));
    let effort_map = efforts
        .iter()
        .filter_map(|e| match map.and_then(|m| m.get(*e)) {
            Some(Some(native)) if native != e => Some((*e, native.clone())),
            _ => None,
        })
        .collect();
    Some(OvThinking {
        efforts,
        default_level,
        effort_map,
    })
}

/// Tallies `w` for one field and tells whether the caller should look at the value.
fn tally(count: &mut FieldCount, w: &Winner) -> bool {
    match w {
        Winner::Evidence => true,
        Winner::Gateway => {
            count.gateway += 1;
            false
        }
        Winner::None => {
            count.none += 1;
            false
        }
    }
}

/// The overrides of one catalog row against what omp shows (`cur`, when known).
fn patch_row(
    row: &Row,
    gateway_id: &str,
    cur: Option<&CurrentModel>,
    with_thinking: bool,
    stats: &mut ProviderStats,
) -> Ov {
    let mut ov = Ov::default();

    // Limits: a positive integer the references settled on.
    for (name, value, current, count, slot) in [
        (
            "contextWindow",
            row.context_window,
            cur.map(|c| c.context_window),
            &mut stats.context_window,
            &mut ov.context_window,
        ),
        (
            "maxTokens",
            row.max_tokens,
            cur.map(|c| c.max_tokens),
            &mut stats.max_tokens,
            &mut ov.max_tokens,
        ),
    ] {
        if !tally(count, &winner(row, name, gateway_id)) {
            continue;
        }
        match value.filter(|v| *v > 0) {
            None => count.none += 1,
            Some(v) if current.is_some_and(|c| c == Some(v)) => count.unchanged += 1,
            Some(v) => {
                count.emitted += 1;
                *slot = Some(v);
            }
        }
    }

    // Price: known or free; unknown stays out (omp keeps its stand-in and status `unknown`).
    if tally(&mut stats.cost, &winner(row, "cost", gateway_id)) {
        match row.cost.as_ref().and_then(|c| cost_patch(c, stats)) {
            None => stats.cost.none += 1,
            Some(p) => {
                let differs = match cur {
                    Some(CurrentModel {
                        pricing_status: Some(s),
                        cost: Some(c),
                        ..
                    }) if s != "unknown" => {
                        !(price_eq(Some(p.input), c.input)
                            && price_eq(Some(p.output), c.output)
                            && (p.cache_read.is_none() || price_eq(p.cache_read, c.cache_read))
                            && (p.cache_write.is_none() || price_eq(p.cache_write, c.cache_write)))
                    }
                    _ => true,
                };
                if differs {
                    stats.cost.emitted += 1;
                    if p.input == 0.0 && p.output == 0.0 {
                        stats.cost_free += 1;
                    }
                    if row.cost.as_ref().is_some_and(|c| !c.tiers.is_empty()) {
                        stats.cost_tiers_dropped += 1;
                    }
                    ov.cost = Some(p);
                } else {
                    stats.cost.unchanged += 1;
                }
            }
        }
    }

    // Input modalities: omp takes text and image only.
    if tally(&mut stats.input, &winner(row, "input", gateway_id)) {
        let modalities: Vec<&'static str> = OMP_INPUT
            .iter()
            .copied()
            .filter(|m| row.input.as_ref().is_some_and(|i| i.iter().any(|x| x == m)))
            .collect();
        if modalities.is_empty() {
            stats.input.none += 1;
        } else if cur
            .and_then(|c| c.input.as_ref())
            .is_some_and(|c| same_set(&modalities, c))
        {
            stats.input.unchanged += 1;
        } else {
            stats.input.emitted += 1;
            ov.input = Some(modalities);
        }
    }

    if with_thinking {
        if tally(&mut stats.reasoning, &winner(row, "reasoning", gateway_id)) {
            match row.reasoning {
                None => stats.reasoning.none += 1,
                Some(r) if cur.is_some_and(|c| c.reasoning == Some(r)) => {
                    stats.reasoning.unchanged += 1;
                }
                Some(r) => {
                    stats.reasoning.emitted += 1;
                    ov.reasoning = Some(r);
                }
            }
        }
        if tally(&mut stats.thinking, &winner(row, "thinking", gateway_id)) {
            match row.thinking.as_ref().map(thinking_patch) {
                None => stats.thinking.none += 1,
                Some(None) => {
                    stats.thinking.none += 1;
                    stats.thinking_inexpressible += 1;
                }
                Some(Some(t)) => {
                    let same = cur.and_then(|c| c.thinking.as_ref()).is_some_and(|c| {
                        c.iter().map(String::as_str).eq(t.efforts.iter().copied())
                    });
                    if same && t.effort_map.is_empty() && t.default_level.is_none() {
                        stats.thinking.unchanged += 1;
                    } else {
                        stats.thinking.emitted += 1;
                        ov.thinking = Some(t);
                    }
                }
            }
        }
    }
    ov
}

/// Why a row keeps a placeholder limit or an unknown price, tallied into `rem`.
fn tally_remaining(row: &Row, gateway_id: &str, rem: &mut Remaining) {
    let gateway_alternate = |f: &FieldRef| f.alternates.iter().any(|a| a.source == gateway_id);
    let stuck = |field: &str| {
        row.provenance
            .fields
            .get(field)
            .filter(|f| f.source == NO_SOURCE && gateway_alternate(f))
    };
    let ctx = stuck("contextWindow");
    let max = stuck("maxTokens");
    if ctx.is_some() {
        rem.context_window += 1;
    }
    if max.is_some() {
        rem.max_tokens += 1;
    }
    if ctx.is_some() || max.is_some() {
        rem.placeholder_models += 1;
        let disagree = ctx
            .iter()
            .chain(max.iter())
            .any(|f| f.conflict == Some(true));
        let reason = if row.references.is_empty() {
            REASON_UNMATCHED
        } else if disagree {
            REASON_DISAGREE
        } else {
            REASON_NO_LIMIT
        };
        *rem.reasons.entry(reason).or_default() += 1;
    }
    let cost_unknown = row.cost.as_ref().is_none_or(|c| c.status == "unknown");
    if cost_unknown {
        rem.cost_unknown += 1;
    }
}

// ---- YAML text ----

/// A YAML scalar for a model id or other free text: plain when that is unambiguous, else a
/// double-quoted JSON string (JSON strings are valid YAML double-quoted scalars).
pub fn yaml_scalar(s: &str) -> String {
    let mut chars = s.chars();
    let plain_start = chars.next().is_some_and(|c| c.is_ascii_alphabetic());
    let plain_rest =
        chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '+' | '-'));
    let reserved = matches!(
        s.to_ascii_lowercase().as_str(),
        "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "y" | "n"
    );
    if plain_start && plain_rest && !reserved {
        s.to_owned()
    } else {
        serde_json::to_string(s).expect("a string always serializes")
    }
}

fn num(v: f64) -> String {
    format!("{v}")
}

fn write_model(out: &mut String, id: &str, ov: &Ov) {
    let _ = writeln!(out, "      {}:", yaml_scalar(id));
    if let Some(v) = ov.context_window {
        let _ = writeln!(out, "        contextWindow: {v}");
    }
    if let Some(v) = ov.max_tokens {
        let _ = writeln!(out, "        maxTokens: {v}");
    }
    if let Some(c) = &ov.cost {
        out.push_str("        cost:\n");
        let _ = writeln!(out, "          input: {}", num(c.input));
        let _ = writeln!(out, "          output: {}", num(c.output));
        if let Some(v) = c.cache_read {
            let _ = writeln!(out, "          cacheRead: {}", num(v));
        }
        if let Some(v) = c.cache_write {
            let _ = writeln!(out, "          cacheWrite: {}", num(v));
        }
    }
    if let Some(input) = &ov.input {
        let _ = writeln!(out, "        input: [{}]", input.join(", "));
    }
    if let Some(r) = ov.reasoning {
        let _ = writeln!(out, "        reasoning: {r}");
    }
    if let Some(t) = &ov.thinking {
        out.push_str("        thinking:\n          mode: effort\n");
        let _ = writeln!(out, "          efforts: [{}]", t.efforts.join(", "));
        if let Some(d) = t.default_level {
            let _ = writeln!(out, "          defaultLevel: {d}");
        }
        if !t.effort_map.is_empty() {
            let items: Vec<String> = t
                .effort_map
                .iter()
                .map(|(k, v)| format!("{k}: {}", yaml_scalar(v)))
                .collect();
            let _ = writeln!(out, "          effortMap: {{{}}}", items.join(", "));
        }
    }
}

/// `sourceId@revision` of every source the exported rows cite.
fn source_list(rows: &[&Row]) -> Vec<String> {
    let set: BTreeSet<String> = rows
        .iter()
        .flat_map(|r| r.provenance.sources.iter())
        .map(|s| match &s.revision {
            Some(rev) => format!("{}@{rev}", s.source_id),
            None => s.source_id.clone(),
        })
        .collect();
    set.into_iter().collect()
}

/// Exports `providers` from the bytes of a merged `catalog.json` (`Model[]`).
pub fn export_omp(catalog: &[u8], opts: &ExportOptions) -> Result<OmpExport, OmpError> {
    let rows: Vec<Row> =
        serde_json::from_slice(catalog).map_err(|e| OmpError::Catalog(e.to_string()))?;
    let catalog_sha256 = sha256_hex(catalog);
    let short = &catalog_sha256[..16];

    for c in &opts.current {
        if !opts.providers.contains(&c.provider) {
            return Err(OmpError::CurrentProviderNotExported {
                provider: c.provider.clone(),
            });
        }
    }
    let mut stats_all = Vec::new();
    let mut sections = Vec::new();
    let mut cited: Vec<&Row> = Vec::new();

    for provider in &opts.providers {
        let gateway_id = crate::omp_live::source_id(provider);
        let prefix = format!("{gateway_id}:");
        let current = opts.current.iter().find(|c| &c.provider == provider);
        let mut stats = ProviderStats {
            provider: provider.clone(),
            ..ProviderStats::default()
        };

        let mut by_id: BTreeMap<&str, &Row> = BTreeMap::new();
        for row in rows
            .iter()
            .filter(|r| r.key.starts_with(&prefix) && &r.provider == provider)
        {
            stats.rows += 1;
            if by_id.insert(&row.id, row).is_some() {
                stats.duplicates += 1;
            }
        }
        if stats.rows == 0 {
            return Err(OmpError::NoRows {
                provider: provider.clone(),
            });
        }
        if let Some(c) = current {
            stats.roster = Some(c.models.len());
            stats.not_in_roster = by_id
                .keys()
                .filter(|id| !c.models.contains_key(**id))
                .count();
            stats.roster_not_in_catalog = c
                .models
                .keys()
                .filter(|id| !by_id.contains_key(id.as_str()))
                .count();
        }

        let mut overrides: BTreeMap<&str, Ov> = BTreeMap::new();
        for (id, row) in &by_id {
            let cur = match current {
                Some(c) => match c.models.get(*id) {
                    Some(m) => Some(m),
                    None => continue,
                },
                None => None,
            };
            stats.considered += 1;
            cited.push(row);
            tally_remaining(row, &gateway_id, &mut stats.remaining);
            let ov = patch_row(row, &gateway_id, cur, opts.with_thinking, &mut stats);
            if !ov.is_empty() {
                overrides.insert(id, ov);
            }
        }
        stats.with_overrides = overrides.len();

        let mut section = String::new();
        if overrides.is_empty() {
            let _ = writeln!(section, "  # {provider}: nothing to export");
        } else {
            let _ = writeln!(section, "  {}:", yaml_scalar(provider));
            let _ = writeln!(
                section,
                "    # catalog sha256:{short}; {} of {} models ({})",
                stats.with_overrides,
                stats.considered,
                stats.field_summary(opts.with_thinking)
            );
            section.push_str("    modelOverrides:\n");
            for (id, ov) in &overrides {
                write_model(&mut section, id, ov);
            }
        }
        sections.push(section);
        stats_all.push(stats);
    }

    let mut yaml = String::new();
    yaml.push_str("# tierbridge export omp: generated, do not edit by hand\n");
    let _ = writeln!(
        yaml,
        "# catalog sha256:{catalog_sha256} ({} models)",
        rows.len()
    );
    let _ = writeln!(yaml, "# sources: {}", source_list(&cited).join(", "));
    for s in &stats_all {
        let _ = writeln!(
            yaml,
            "# {}: {} of {} models ({})",
            s.provider,
            s.with_overrides,
            s.considered,
            s.field_summary(opts.with_thinking)
        );
    }
    if sections.iter().all(|s| s.trim_start().starts_with('#')) {
        yaml.push_str("providers: {}\n");
    } else {
        yaml.push_str("providers:\n");
    }
    for s in &sections {
        yaml.push_str(s);
    }
    Ok(OmpExport {
        yaml,
        stats: stats_all,
        catalog_sha256,
    })
}

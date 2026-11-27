//! models.dev adapter: `https://models.dev/api.json` -> [`Observation`]s.
//!
//! Port of `adapt_models_dev()` from the Python prototype, with one deliberate change: values
//! the contract cannot hold are reported as [`Diagnostic`]s instead of being dropped silently.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::CatalogError;
use crate::diagnostics::{Diagnostic, Findings, SentinelMapping};
use crate::model::{
    Confidence, Cost, CostStatus, Effort, Fields, Lifecycle, LifecycleStatus, Modality,
    Observation, PriceTier, ProvenanceKind, ProvenanceSource, Thinking, ThinkingMode,
    ThinkingOptions,
};

pub const SOURCE_ID: &str = "models.dev";
pub const API_URL: &str = "https://models.dev/api.json";
/// Prompt size above which models.dev's flat `context_over_200k` prices apply.
const OVER_200K_TOKENS: u64 = 200_000;

/// Result of adapting one models.dev snapshot.
#[derive(Debug)]
pub struct Adapted {
    pub source: ProvenanceSource,
    /// Full lowercase hex SHA-256 of the input bytes.
    pub sha256: String,
    pub providers: usize,
    /// Sorted by (provider, id).
    pub observations: Vec<Observation>,
    pub findings: Findings,
}

/// Lowercase hex SHA-256.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut acc, b| {
            acc.push_str(&format!("{b:02x}"));
            acc
        })
}

/// Adapts the raw bytes of `api.json`. `fetched_at_ms` is when the bytes were obtained.
///
/// Structural problems (invalid JSON, wrong shape, no models) return an error; value problems
/// are collected in [`Adapted::findings`].
pub fn adapt(bytes: &[u8], fetched_at_ms: i64) -> Result<Adapted, CatalogError> {
    let root: Value = serde_json::from_slice(bytes)?;
    let Value::Object(providers) = root else {
        return Err(CatalogError::NotAnObject);
    };
    let sha256 = sha256_hex(bytes);
    let source = ProvenanceSource {
        source_id: SOURCE_ID.to_owned(),
        kind: ProvenanceKind::Bundled,
        origin: format!("{API_URL}#sha256={}", &sha256[..16]),
        revision: Some(crate::time::format_rfc3339(fetched_at_ms)),
        confidence: Confidence::Declared,
        observed_at_ms: Some(fetched_at_ms),
    };

    let mut findings = Findings::default();
    let mut observations = Vec::new();
    for (provider_id, provider) in &providers {
        let Value::Object(provider) = provider else {
            return Err(CatalogError::ProviderNotObject {
                provider: provider_id.clone(),
            });
        };
        let Some(Value::Object(models)) = provider.get("models") else {
            return Err(CatalogError::MissingModels {
                provider: provider_id.clone(),
            });
        };
        for (model_id, model) in models {
            let Value::Object(model) = model else {
                return Err(CatalogError::ModelNotObject {
                    provider: provider_id.clone(),
                    id: model_id.clone(),
                });
            };
            let mut cx = Cx {
                provider: provider_id,
                id: model_id,
                findings: &mut findings,
            };
            observations.push(adapt_model(&mut cx, &source, model));
        }
    }
    if observations.is_empty() {
        return Err(CatalogError::NoModels);
    }
    Ok(Adapted {
        source,
        sha256,
        providers: providers.len(),
        observations,
        findings,
    })
}

/// Why a published limit of `0` becomes unknown.
const ZERO_LIMIT_REASON: &str = "models.dev publishes 0 when no token limit applies";

/// Per-model reporting context.
struct Cx<'a> {
    provider: &'a str,
    id: &'a str,
    findings: &'a mut Findings,
}

impl Cx<'_> {
    fn reject(&mut self, field: &str, value: &Value, reason: &str) {
        self.findings.rejections.push(Diagnostic {
            source_id: SOURCE_ID.to_owned(),
            provider: self.provider.to_owned(),
            id: self.id.to_owned(),
            field: field.to_owned(),
            value: value.clone(),
            reason: reason.to_owned(),
        });
    }

    /// A value the contract has no field for (see [`Findings::unmodelled`]).
    fn unmodelled(&mut self, field: &str, value: &Value, reason: &str) {
        self.findings.unmodelled.push(Diagnostic {
            source_id: SOURCE_ID.to_owned(),
            provider: self.provider.to_owned(),
            id: self.id.to_owned(),
            field: field.to_owned(),
            value: value.clone(),
            reason: reason.to_owned(),
        });
    }

    fn sentinel(&mut self, field: &str, value: &Value, mapped_to: &str, reason: &str) {
        self.findings.sentinels.push(SentinelMapping {
            source_id: SOURCE_ID.to_owned(),
            provider: self.provider.to_owned(),
            id: self.id.to_owned(),
            field: field.to_owned(),
            value: value.clone(),
            mapped_to: mapped_to.to_owned(),
            reason: reason.to_owned(),
        });
    }

    /// `null`/absent -> `None`; a string -> `Some`; anything else is rejected.
    fn string(&mut self, obj: &Map<String, Value>, key: &str, field: &str) -> Option<String> {
        match obj.get(key)? {
            Value::Null => None,
            Value::String(s) => Some(s.clone()),
            other => {
                self.reject(field, other, "expected a string");
                None
            }
        }
    }

    fn boolean(&mut self, obj: &Map<String, Value>, key: &str, field: &str) -> Option<bool> {
        match obj.get(key)? {
            Value::Null => None,
            Value::Bool(b) => Some(*b),
            other => {
                self.reject(field, other, "expected a boolean");
                None
            }
        }
    }

    /// Token limit. `0` is models.dev's "no token limit applies": recorded as a sentinel and
    /// mapped to unknown. Negative or non-integer values are rejected.
    fn limit(&mut self, obj: &Map<String, Value>, key: &str, field: &str) -> Option<u64> {
        let value = obj.get(key)?;
        match value {
            Value::Null => None,
            Value::Number(n) => match n.as_u64() {
                Some(0) => {
                    self.findings.zero_limits += 1;
                    self.sentinel(field, value, "unknown", ZERO_LIMIT_REASON);
                    None
                }
                Some(v) => Some(v),
                None => {
                    let reason = if n.as_i64().is_some_and(|v| v < 0) {
                        "negative limit"
                    } else {
                        "limit is not a positive integer"
                    };
                    self.reject(field, value, reason);
                    None
                }
            },
            other => {
                self.reject(field, other, "limit is not a number");
                None
            }
        }
    }

    /// USD per million tokens. `Err(())` = rejected (already reported).
    fn price(
        &mut self,
        obj: &Map<String, Value>,
        key: &str,
        field: &str,
    ) -> Result<Option<f64>, ()> {
        let Some(value) = obj.get(key) else {
            return Ok(None);
        };
        match value {
            Value::Null => Ok(None),
            Value::Number(n) => match n.as_f64() {
                Some(v) if v >= 0.0 => Ok(Some(v)),
                _ => {
                    self.reject(field, value, "negative price");
                    Err(())
                }
            },
            other => {
                self.reject(field, other, "price is not a number");
                Err(())
            }
        }
    }

    /// Budget bound (`min`/`max`): non-negative integer, otherwise rejected.
    fn budget_bound(&mut self, value: Option<&Value>, field: &str) -> Option<u64> {
        let value = value?;
        match value {
            Value::Null => None,
            Value::Number(n) => {
                if let Some(v) = n.as_u64() {
                    Some(v)
                } else {
                    self.reject(field, value, "budget bound is not a non-negative integer");
                    None
                }
            }
            other => {
                self.reject(field, other, "budget bound is not a number");
                None
            }
        }
    }
}

fn adapt_model(cx: &mut Cx<'_>, source: &ProvenanceSource, m: &Map<String, Value>) -> Observation {
    let empty = Map::new();
    let limit = match m.get("limit") {
        Some(Value::Object(o)) => o,
        Some(Value::Null) | None => &empty,
        Some(other) => {
            cx.reject("limit", other, "expected an object");
            &empty
        }
    };
    let reasoning = cx.boolean(m, "reasoning", "reasoning");
    let release_date = cx.string(m, "release_date", "raw.releaseDate");
    let family = cx.string(m, "family", "raw.family");

    let mut fields = Fields::empty(cx.provider, cx.id);
    fields.name = cx.string(m, "name", "name");
    fields.canonical_id = cx.string(m, "canonical_model_id", "canonicalId");
    fields.context_window = cx.limit(limit, "context", "contextWindow");
    fields.max_input_tokens = cx.limit(limit, "input", "maxInputTokens");
    fields.max_tokens = cx.limit(limit, "output", "maxTokens");
    fields.cost = adapt_cost(cx, m.get("cost"));
    let modalities = match m.get("modalities") {
        Some(Value::Object(o)) => Some(o),
        Some(Value::Null) | None => None,
        Some(other) => {
            cx.reject("modalities", other, "expected an object");
            None
        }
    };
    fields.input = modalities.and_then(|o| adapt_modalities(cx, o, "input"));
    fields.output = modalities.and_then(|o| adapt_modalities(cx, o, "output"));
    fields.reasoning = reasoning;
    fields.thinking = reasoning.map(|r| adapt_thinking(cx, r, m.get("reasoning_options")));
    fields.supports_tools = cx.boolean(m, "tool_call", "supportsTools");
    fields.lifecycle = Some(adapt_lifecycle(cx, m.get("status")));

    let mut obs = Observation::new(source.clone(), fields);
    obs.raw = Some(Map::from_iter([
        ("releaseDate".to_owned(), json!(release_date)),
        ("family".to_owned(), json!(family)),
    ]));
    obs
}

fn adapt_modalities(cx: &mut Cx<'_>, o: &Map<String, Value>, key: &str) -> Option<Vec<Modality>> {
    let items = match o.get(key)? {
        Value::Array(a) => a,
        Value::Null => return None,
        other => {
            cx.reject(key, other, "expected an array");
            return None;
        }
    };
    let mut set = BTreeSet::new();
    for item in items {
        match item.as_str().and_then(Modality::parse) {
            Some(m) => {
                set.insert(m);
            }
            None => cx.reject(key, item, "modality outside the contract enum"),
        }
    }
    (!set.is_empty()).then(|| set.into_iter().collect())
}

fn adapt_lifecycle(cx: &mut Cx<'_>, status: Option<&Value>) -> Lifecycle {
    let status = match status {
        None | Some(Value::Null) => LifecycleStatus::Live,
        Some(Value::String(s)) => match s.as_str() {
            "deprecated" => LifecycleStatus::Deprecated,
            "beta" | "alpha" => LifecycleStatus::Preview,
            _ => {
                cx.reject("lifecycle.status", &json!(s), "unknown status");
                LifecycleStatus::Unknown
            }
        },
        Some(other) => {
            cx.reject("lifecycle.status", other, "expected a string");
            LifecycleStatus::Unknown
        }
    };
    Lifecycle {
        status,
        replacement: None,
        shutdown_on: None,
    }
}

fn adapt_thinking(cx: &mut Cx<'_>, reasoning: bool, options: Option<&Value>) -> Thinking {
    let plain = |mode| Thinking {
        mode,
        efforts: Vec::new(),
        options: None,
        default_level: None,
        effort_map: None,
    };
    if !reasoning {
        return plain(ThinkingMode::None);
    }
    let options: &[Value] = match options {
        None | Some(Value::Null) => &[],
        Some(Value::Array(a)) => a,
        Some(other) => {
            cx.reject("reasoning_options", other, "expected an array");
            &[]
        }
    };
    if options.is_empty() {
        return plain(ThinkingMode::Unknown);
    }

    let mut efforts = BTreeSet::new();
    let mut budget: Option<&Map<String, Value>> = None;
    let mut toggle = false;
    for option in options {
        let Some(o) = option.as_object() else {
            cx.reject("reasoning_options", option, "expected an object");
            continue;
        };
        match o.get("type").and_then(Value::as_str) {
            Some("effort") => {
                let values = match o.get("values") {
                    None | Some(Value::Null) => &[][..],
                    Some(Value::Array(a)) => a,
                    Some(other) => {
                        cx.reject("thinking.efforts", other, "expected an array");
                        &[]
                    }
                };
                for v in values {
                    match v.as_str().and_then(Effort::parse) {
                        Some(e) => {
                            efforts.insert(e);
                        }
                        None => {
                            cx.reject("thinking.efforts", v, "effort outside the contract enum")
                        }
                    }
                }
            }
            Some("budget_tokens") => budget = Some(o),
            Some("toggle") => toggle = true,
            _ => cx.reject(
                "reasoning_options.type",
                o.get("type").unwrap_or(&Value::Null),
                "unknown reasoning option type",
            ),
        }
    }

    let efforts: Vec<Effort> = efforts.into_iter().collect();
    let (mut min_budget, mut max_budget, mut dynamic_budget) = (None, None, None);
    if let Some(b) = budget {
        match b.get("min") {
            // models.dev spells "the provider picks the budget" as min = -1.
            Some(v) if v.as_i64() == Some(-1) => {
                cx.sentinel(
                    "thinking.minBudgetTokens",
                    v,
                    "dynamicBudget: true",
                    "models.dev spells \"the provider picks the budget\" as min = -1",
                );
                dynamic_budget = Some(true);
            }
            v => min_budget = cx.budget_bound(v, "thinking.minBudgetTokens"),
        }
        max_budget = cx.budget_bound(b.get("max"), "thinking.maxBudgetTokens");
    }
    let mode = if !efforts.is_empty() {
        ThinkingMode::Effort
    } else if budget.is_some() {
        ThinkingMode::Budget
    } else {
        ThinkingMode::Toggle
    };
    let can_disable = (toggle || efforts.contains(&Effort::Off)).then_some(true);
    Thinking {
        mode,
        efforts,
        options: Some(ThinkingOptions {
            can_disable,
            min_budget_tokens: min_budget,
            max_budget_tokens: max_budget,
            dynamic_budget,
        }),
        default_level: None,
        effort_map: None,
    }
}

/// models.dev prices the contract's `Cost` has no field for.
const UNMODELLED_COST_KEYS: [&str; 3] = ["reasoning", "input_audio", "output_audio"];

fn adapt_cost(cx: &mut Cx<'_>, cost: Option<&Value>) -> Option<Cost> {
    let c = match cost? {
        Value::Null => return None,
        Value::Object(o) => o,
        other => {
            cx.reject("cost", other, "expected an object");
            return None;
        }
    };
    if c.is_empty() {
        return None;
    }
    for key in UNMODELLED_COST_KEYS {
        if let Some(v) = c.get(key).filter(|v| !v.is_null()) {
            cx.unmodelled(
                &format!("cost.{key}"),
                v,
                "the contract has no price for this; the record's cost omits it",
            );
        }
    }

    let mut tiers = Vec::new();
    match c.get("tiers") {
        None | Some(Value::Null) => {}
        Some(Value::Array(list)) => {
            for (i, t) in list.iter().enumerate() {
                if let Some(tier) = adapt_tier(cx, t, &format!("cost.tiers[{i}]")) {
                    tiers.push(tier);
                }
            }
        }
        Some(other) => cx.reject("cost.tiers", other, "expected an array"),
    }
    if tiers.is_empty()
        && let Some(Value::Object(over)) = c.get("context_over_200k")
        && !over.is_empty()
    {
        let prices = |cx: &mut Cx<'_>| -> Option<PriceTier> {
            let p = "cost.context_over_200k";
            let (input, output) = (
                cx.price(over, "input", &format!("{p}.input")),
                cx.price(over, "output", &format!("{p}.output")),
            );
            let (read, write) = (
                cx.price(over, "cache_read", &format!("{p}.cache_read")),
                cx.price(over, "cache_write", &format!("{p}.cache_write")),
            );
            Some(PriceTier {
                prompt_tokens_above: OVER_200K_TOKENS,
                input: input.ok()?,
                output: output.ok()?,
                cache_read: read.ok()?,
                cache_write: write.ok()?,
            })
        };
        tiers.extend(prices(cx));
    }

    // A rejected top-level price leaves that price unknown; the rest of the record survives.
    let mut top = |key: &str| cx.price(c, key, &format!("cost.{key}")).unwrap_or(None);
    let (input, output) = (top("input"), top("output"));
    let (cache_read, cache_write) = (top("cache_read"), top("cache_write"));
    Some(cost_obj(input, output, cache_read, cache_write, tiers))
}

/// Replacement prices for prompts above a size. A tier with a rejected price is dropped whole:
/// a tier's prices *replace* the base prices, so a partial tier would misstate them.
fn adapt_tier(cx: &mut Cx<'_>, t: &Value, path: &str) -> Option<PriceTier> {
    let Some(t) = t.as_object() else {
        cx.reject(path, t, "expected an object");
        return None;
    };
    let gate = match t.get("tier") {
        Some(Value::Object(g)) if g.get("type").and_then(Value::as_str) == Some("context") => g,
        other => {
            cx.reject(
                &format!("{path}.tier"),
                other.unwrap_or(&Value::Null),
                "unsupported tier gate (only `context` tiers are modelled)",
            );
            return None;
        }
    };
    let size = match gate.get("size").and_then(Value::as_u64) {
        Some(s) if s > 0 => s,
        _ => {
            cx.reject(
                &format!("{path}.tier.size"),
                gate.get("size").unwrap_or(&Value::Null),
                "context tier without a positive integer size",
            );
            return None;
        }
    };
    let input = cx.price(t, "input", &format!("{path}.input"));
    let output = cx.price(t, "output", &format!("{path}.output"));
    let cache_read = cx.price(t, "cache_read", &format!("{path}.cache_read"));
    let cache_write = cx.price(t, "cache_write", &format!("{path}.cache_write"));
    Some(PriceTier {
        prompt_tokens_above: size,
        input: input.ok()?,
        output: output.ok()?,
        cache_read: cache_read.ok()?,
        cache_write: cache_write.ok()?,
    })
}

/// Port of the prototype's `cost_obj(..., zero_means_unknown=False)`: models.dev publishes real
/// zeros, so an all-zero price list is `free`, and no price at all is `unknown`.
fn cost_obj(
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
    tiers: Vec<PriceTier>,
) -> Cost {
    let prices = [input, output, cache_read, cache_write];
    if prices.iter().all(Option::is_none) {
        return Cost {
            status: CostStatus::Unknown,
            input: None,
            output: None,
            cache_read: None,
            cache_write: None,
            tiers: Vec::new(),
        };
    }
    if tiers.is_empty() && prices.iter().all(|p| p.unwrap_or(0.0) == 0.0) {
        return Cost {
            status: CostStatus::Free,
            input: Some(0.0),
            output: Some(0.0),
            cache_read,
            cache_write,
            tiers,
        };
    }
    Cost {
        status: CostStatus::Known,
        input,
        output,
        cache_read,
        cache_write,
        tiers,
    }
}

//! `cliproxyapi-cache` adapter: omp's cache of the model list a CLIProxyAPI gateway serves
//! (`~/.omp/agent/cliproxyapi-models.json`) -> [`Observation`]s.
//!
//! Port of `adapt_cliproxy_cache()` from the Python prototype. Only the published model list
//! (`models[]`, plus `fetchedAt`/`modelsUrl` for provenance) is read: nothing about the gateway's
//! channels or accounts is touched.
//!
//! Gateway rule: the cache fills `0` where the gateway has no price, so an all-zero price list is
//! *unknown*, never free. Same rules as the other adapters: a snapshot with the wrong shape is a
//! [`CatalogError`]; a value outside the contract is removed and reported as a [`Diagnostic`].
//!
//! Deliberate differences from the prototype (none changes a row of the committed snapshot
//! except where noted):
//!   * a `thinkingLevelMap` level outside the contract that the route *accepts* (`ultra` with a
//!     native spelling, 8 rows) is a rejection; the prototype dropped it silently. The same level
//!     mapped to `null` (not accepted) says nothing the contract could hold and is not reported;
//!   * `contextWindow`/`maxTokens` of `0` are sentinels mapped to unknown, negatives rejected;
//!   * a tier without a positive `inputTokensAbove` is a rejection, not a silent skip;
//!   * a row without a boolean `reasoning` has unknown `thinking`; the prototype said `none`;
//!   * `compat` (`supportsMidConvoEffort`) has no field in the schema and is not carried.

use std::collections::{BTreeMap, HashSet};

use serde_json::{Map, Value, json};

use crate::CatalogError;
use crate::diagnostics::Findings;
use crate::gateway::{Cx, ensure_unique};
use crate::model::{
    Confidence, Effort, Fields, ModelKind, Observation, ProvenanceKind, ProvenanceSource, Thinking,
    ThinkingMode,
};
use crate::models_dev::{Adapted, sha256_hex};

pub const SOURCE_ID: &str = "cliproxyapi-cache";
/// The `provider` of every row: the gateway itself.
pub const PROVIDER: &str = "cliproxyapi";
/// Origin recorded when the cache does not say which URL it was fetched from.
const FALLBACK_ORIGIN: &str = "cliproxyapi-models.json";

/// Adapts the raw bytes of `cliproxyapi-models.json`. The cache records when it was fetched
/// (`fetchedAt`), which is what `observedAtMs` and the revision carry; the caller's own
/// timestamp is not used.
pub fn adapt(bytes: &[u8]) -> Result<Adapted, CatalogError> {
    let root: Value = serde_json::from_slice(bytes)?;
    let Value::Object(root) = root else {
        return Err(malformed("top level of the cache is not an object"));
    };
    let Some(fetched_at) = root.get("fetchedAt").and_then(Value::as_i64) else {
        return Err(malformed(
            "`fetchedAt` is missing or not an integer (milliseconds since the epoch)",
        ));
    };
    let Some(Value::Array(models)) = root.get("models") else {
        return Err(malformed("`models` is missing or not an array"));
    };
    if models.is_empty() {
        return Err(CatalogError::NoModels);
    }
    let origin = match root.get("modelsUrl") {
        None | Some(Value::Null) => FALLBACK_ORIGIN.to_owned(),
        Some(Value::String(url)) if !url.is_empty() => url.clone(),
        Some(_) => return Err(malformed("`modelsUrl` is not a non-empty string")),
    };

    let source = ProvenanceSource {
        source_id: SOURCE_ID.to_owned(),
        kind: ProvenanceKind::Discovered,
        origin,
        revision: Some(format!("fetchedAt={fetched_at}")),
        confidence: Confidence::Inferred,
        observed_at_ms: Some(fetched_at),
    };

    let mut findings = Findings::default();
    let mut seen = HashSet::new();
    let mut observations = Vec::with_capacity(models.len());
    for (index, model) in models.iter().enumerate() {
        let Value::Object(model) = model else {
            return Err(malformed(format!("models[{index}] is not an object")));
        };
        let Some(Value::String(id)) = model.get("id") else {
            return Err(malformed(format!("models[{index}] has no string `id`")));
        };
        ensure_unique(&mut seen, PROVIDER, id, "the cache")?;
        let mut cx = Cx {
            source_id: SOURCE_ID,
            provider: PROVIDER,
            id,
            findings: &mut findings,
        };
        observations.push(adapt_model(&mut cx, &source, model));
    }
    observations.sort_by(|a, b| a.fields.id.cmp(&b.fields.id));
    Ok(Adapted {
        source,
        sha256: sha256_hex(bytes),
        providers: 1,
        observations,
        findings,
    })
}

fn malformed(what: impl Into<String>) -> CatalogError {
    CatalogError::Malformed(what.into())
}

fn adapt_model(cx: &mut Cx<'_>, source: &ProvenanceSource, m: &Map<String, Value>) -> Observation {
    let reasoning = cx.boolean(m, "reasoning", "reasoning");
    let mut fields = Fields::empty(cx.provider, cx.id);
    fields.name = cx.string(m, "name", "name");
    fields.kind = Some(ModelKind::Chat);
    fields.context_window = cx.limit(m, "contextWindow", "contextWindow");
    fields.max_tokens = cx.limit(m, "maxTokens", "maxTokens");
    fields.cost = cx.gateway_cost(m.get("cost"), Some("inputTokensAbove"), true);
    fields.input = cx.modalities(m, "input", "input");
    fields.reasoning = reasoning;
    fields.thinking = reasoning.map(|r| level_map_thinking(cx, r, m.get("thinkingLevelMap")));

    let mut obs = Observation::new(source.clone(), fields);
    obs.raw = Some(Map::from_iter([
        (
            "stale".to_owned(),
            m.get("stale").cloned().unwrap_or(Value::Null),
        ),
        (
            "lastSeenAt".to_owned(),
            m.get("lastSeenAt").cloned().unwrap_or(Value::Null),
        ),
    ]));
    obs
}

/// `thinkingLevelMap`: `{level: native spelling | null}`; `null` = the route does not accept
/// that level. Absent map on a reasoning model = the options are unknown.
fn level_map_thinking(cx: &mut Cx<'_>, reasoning: bool, map: Option<&Value>) -> Thinking {
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
    let levels = match map {
        None | Some(Value::Null) => return plain(ThinkingMode::Unknown),
        Some(Value::Object(levels)) => levels,
        Some(other) => {
            cx.reject("thinkingLevelMap", other, "expected an object");
            return plain(ThinkingMode::Unknown);
        }
    };
    let mut effort_map: BTreeMap<Effort, Option<String>> = BTreeMap::new();
    for (level, native) in levels {
        let field = format!("thinking.effortMap.{level}");
        let native = match native {
            Value::Null => None,
            Value::String(s) => Some(s.clone()),
            other => {
                cx.reject(&field, other, "native level spelling is not a string");
                continue;
            }
        };
        match Effort::parse(level) {
            Some(effort) => {
                if effort_map.insert(effort, native).is_some() {
                    cx.reject(
                        &field,
                        &json!(level),
                        "level repeats an alias of the same effort",
                    );
                }
            }
            None if native.is_some() => {
                cx.reject(&field, &json!(native), "effort outside the contract enum");
            }
            None => {}
        }
    }
    let efforts: Vec<Effort> = effort_map
        .iter()
        .filter(|(_, native)| native.is_some())
        .map(|(&effort, _)| effort)
        .collect();
    Thinking {
        mode: if efforts.is_empty() {
            ThinkingMode::Unknown
        } else {
            ThinkingMode::Effort
        },
        efforts,
        options: None,
        default_level: None,
        effort_map: Some(effort_map),
    }
}

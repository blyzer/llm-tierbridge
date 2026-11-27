//! `omp-bundled` adapter: oh-my-pi's own checked-in model roster
//! (`fixtures/llm-oracle/catalog/models.json.zst`, decompressed: `{provider: {id: model}}`)
//! -> [`Observation`]s.
//!
//! Port of `adapt_omp_bundled()` from the Python prototype. The prototype uses it as a probe of
//! how circular matching against omp's own roster would be; it is a `bundled` source with
//! `declared` confidence.
//!
//! Prices are real here (an all-zero list is `free`), with one exception: `openrouter/auto` and
//! `openrouter/auto-beta` (under `openrouter`; `kilo`'s copies carry real zeros) carry `input`/`output` of `-1000000`.
//! omp does not document that as "variable pricing" (nothing in its catalog sources says so; it
//! looks like OpenRouter's `-1` scaled to USD per million), so it is **not** a sentinel: the two
//! prices are per-row rejections and the row's whole `cost` becomes unknown. The prototype
//! turned them into `None` and then reported `free` with `input = output = 0`, which would have
//! advertised a variable-priced router as free; those 2 rows are the one place the output
//! differs from the prototype's.
//!
//! Not carried, as in the prototype: `cost.longContext` (49 rows), the `thinking.mode` and
//! `defaultLevel`/`effortMap` details (only the effort list is read) and the wire-level fields
//! (`api`, `baseUrl`, `compat`, ...).

use serde_json::{Map, Value};

use crate::CatalogError;
use crate::diagnostics::Findings;
use crate::gateway::Cx;
use crate::model::{
    Confidence, Fields, Observation, ProvenanceKind, ProvenanceSource, Thinking, ThinkingMode,
};
use crate::models_dev::{Adapted, sha256_hex};

pub const SOURCE_ID: &str = "omp-bundled";
/// Where oh-my-pi keeps the roster this snapshot was decompressed from.
pub const ORIGIN: &str = "fixtures/llm-oracle/catalog/models.json.zst";
pub const REVISION: &str = "oh-my-pi working tree";

/// Adapts the raw bytes of the decompressed roster. `fetched_at_ms` is when it was read.
pub fn adapt(bytes: &[u8], fetched_at_ms: i64) -> Result<Adapted, CatalogError> {
    let root: Value = serde_json::from_slice(bytes)?;
    let Value::Object(providers) = root else {
        return Err(CatalogError::NotAnObject);
    };
    let source = ProvenanceSource {
        source_id: SOURCE_ID.to_owned(),
        kind: ProvenanceKind::Bundled,
        origin: ORIGIN.to_owned(),
        revision: Some(REVISION.to_owned()),
        confidence: Confidence::Declared,
        observed_at_ms: Some(fetched_at_ms),
    };

    let mut findings = Findings::default();
    let mut observations = Vec::new();
    for (provider_id, provider) in &providers {
        let Value::Object(models) = provider else {
            return Err(CatalogError::ProviderNotObject {
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
                source_id: SOURCE_ID,
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
    observations.sort_by(|a, b| {
        (&a.fields.provider, &a.fields.id).cmp(&(&b.fields.provider, &b.fields.id))
    });
    Ok(Adapted {
        source,
        sha256: sha256_hex(bytes),
        providers: providers.len(),
        observations,
        findings,
    })
}

fn adapt_model(cx: &mut Cx<'_>, source: &ProvenanceSource, m: &Map<String, Value>) -> Observation {
    let reasoning = cx.boolean(m, "reasoning", "reasoning");
    let mut fields = Fields::empty(cx.provider, cx.id);
    fields.name = cx.string(m, "name", "name");
    fields.context_window = cx.limit(m, "contextWindow", "contextWindow");
    fields.max_tokens = cx.limit(m, "maxTokens", "maxTokens");
    fields.cost = cx.gateway_cost(m.get("cost"), None, false);
    fields.input = cx.modalities(m, "input", "input");
    fields.reasoning = reasoning;
    fields.thinking = reasoning.map(|r| adapt_thinking(cx, r, m.get("thinking")));
    Observation::new(source.clone(), fields)
}

/// `thinking: {mode, efforts, ...}`; only the effort list is read.
fn adapt_thinking(cx: &mut Cx<'_>, reasoning: bool, thinking: Option<&Value>) -> Thinking {
    let (mode, efforts) = if !reasoning {
        (ThinkingMode::None, Vec::new())
    } else {
        let list = match thinking {
            None | Some(Value::Null) => None,
            Some(Value::Object(t)) => cx.effort_list(t.get("efforts"), "thinking.efforts"),
            Some(other) => {
                cx.reject("thinking", other, "expected an object");
                None
            }
        };
        match list {
            Some(efforts) if !efforts.is_empty() => (ThinkingMode::Effort, efforts),
            _ => (ThinkingMode::Unknown, Vec::new()),
        }
    };
    Thinking {
        mode,
        efforts,
        options: None,
        default_level: None,
        effort_map: None,
    }
}

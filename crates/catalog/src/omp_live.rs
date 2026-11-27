//! `omp-live:<provider>` adapter: the live roster `omp models <provider> --json --no-extensions`
//! prints for a gateway provider (`easycliproxy`, `dsh`) -> [`Observation`]s.
//!
//! Port of `adapt_omp_live()` from the Python prototype. Input is a saved copy of that command's
//! output (`{"models": [...]}`); running the command is left to the caller (`--input` only).
//! Only each provider's published roster is read, never its channels or accounts.
//!
//! Gateway rule: `pricingStatus: "unknown"` means the zeros in `cost` are stand-ins, so the cost
//! is unknown, never free. Any other (or missing) `pricingStatus` is treated the same way, so a
//! price the roster did not vouch for is never promoted to "free" (the prototype read a missing
//! status as "zeros are real"; every row of the committed snapshots says `unknown`).
//!
//! Differences from the prototype besides the shared gateway rules (see [`crate::gateway`]):
//!   * effort spellings outside the contract are rejections, not silent drops;
//!   * a row whose own `provider` differs from the requested one fails the snapshot (wrong file);
//!   * `reasoning` missing leaves `thinking` unknown instead of `none`.

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::CatalogError;
use crate::diagnostics::Findings;
use crate::gateway::{Cx, ensure_unique};
use crate::model::{
    Confidence, Fields, Observation, ProvenanceKind, ProvenanceSource, Thinking, ThinkingMode,
};
use crate::models_dev::{Adapted, sha256_hex};

/// Providers whose roster this adapter reads, and the `sourceId` of each.
pub const EASYCLIPROXY: &str = "easycliproxy";
pub const EASYCLIPROXY_SOURCE_ID: &str = "omp-live:easycliproxy";
pub const DSH: &str = "dsh";
pub const DSH_SOURCE_ID: &str = "omp-live:dsh";
/// The `omp` release the committed snapshots were taken with. The roster does not record it.
pub const OMP_VERSION: &str = "19.2.1";

/// `sourceId` of the roster of `provider`.
pub fn source_id(provider: &str) -> String {
    format!("omp-live:{provider}")
}

/// Adapts the raw bytes of `omp models <provider> --json`. `fetched_at_ms` is when the roster
/// was printed.
pub fn adapt(bytes: &[u8], fetched_at_ms: i64, provider: &str) -> Result<Adapted, CatalogError> {
    let root: Value = serde_json::from_slice(bytes)?;
    let Value::Object(root) = root else {
        return Err(malformed("top level of the roster is not an object"));
    };
    let Some(Value::Array(models)) = root.get("models") else {
        return Err(malformed("`models` is missing or not an array"));
    };
    if models.is_empty() {
        return Err(CatalogError::NoModels);
    }
    let sha256 = sha256_hex(bytes);
    let sid = source_id(provider);
    let source = ProvenanceSource {
        source_id: sid.clone(),
        kind: ProvenanceKind::Discovered,
        origin: format!("omp models {provider} --json --no-extensions"),
        revision: Some(format!("omp {OMP_VERSION}")),
        confidence: Confidence::Inferred,
        observed_at_ms: Some(fetched_at_ms),
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
        if let Some(own) = model.get("provider").and_then(Value::as_str)
            && own != provider
        {
            return Err(malformed(format!(
                "models[{index}] ({id}) belongs to provider `{own}`, not `{provider}`"
            )));
        }
        ensure_unique(&mut seen, provider, id, "the roster")?;
        let mut cx = Cx {
            source_id: &sid,
            provider,
            id,
            findings: &mut findings,
        };
        observations.push(adapt_model(&mut cx, &source, model));
    }
    observations.sort_by(|a, b| a.fields.id.cmp(&b.fields.id));
    Ok(Adapted {
        source,
        sha256,
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
    let pricing_status = cx.string(m, "pricingStatus", "raw.pricingStatus");
    let prices_are_real = pricing_status.as_deref() == Some("known");

    let mut fields = Fields::empty(cx.provider, cx.id);
    fields.name = cx.string(m, "name", "name");
    fields.kind = cx.kind(m);
    fields.context_window = cx.limit(m, "contextWindow", "contextWindow");
    fields.max_tokens = cx.limit(m, "maxTokens", "maxTokens");
    fields.cost = cx.gateway_cost(m.get("cost"), None, !prices_are_real);
    fields.input = cx.modalities(m, "input", "input");
    fields.reasoning = reasoning;
    fields.thinking = reasoning.map(|r| adapt_thinking(cx, r, m.get("thinking")));

    let mut obs = Observation::new(source.clone(), fields);
    obs.selector = cx.string(m, "selector", "selector");
    obs.raw = Some(Map::from_iter([(
        "pricingStatus".to_owned(),
        m.get("pricingStatus").cloned().unwrap_or(Value::Null),
    )]));
    obs
}

/// `thinking`: the effort levels the model accepts, or null when the roster does not list any.
fn adapt_thinking(cx: &mut Cx<'_>, reasoning: bool, levels: Option<&Value>) -> Thinking {
    let (mode, efforts) = if !reasoning {
        (ThinkingMode::None, Vec::new())
    } else {
        match cx.effort_list(levels, "thinking.efforts") {
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

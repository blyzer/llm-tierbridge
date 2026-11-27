//! modelparams.dev adapter: `https://modelparams.dev/api/v1/models.json` -> [`Observation`]s.
//!
//! Port of `adapt_modelparams()` from the Python prototype. The source contributes request-body
//! knobs (`acceptedParams`), lifecycle, the effort levels they imply (`thinking`), the largest
//! output length they allow (`maxTokens`) and `authType`. It carries no prices, limits or
//! modalities, so those stay unknown.
//!
//! Same rules as the models.dev adapter: a snapshot with the wrong shape is a [`CatalogError`];
//! a value outside the contract is removed and reported as a [`Diagnostic`]; a sentinel the
//! source documents is kept/mapped and reported as a [`SentinelMapping`].
//!
//! Deliberate differences from the prototype, none of which changes a row of the current snapshot:
//!   * values the prototype dropped silently (effort `default`) are now rejections;
//!   * a negative bound on a reasoning or length parameter is rejected unless the source's own
//!     description documents `-1` (then it is a sentinel and the wire value is kept);
//!   * `count` must match the number of models, and `(provider, model, authType)` must be unique.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use crate::CatalogError;
use crate::diagnostics::{Diagnostic, Findings, SentinelMapping};
use crate::model::{
    AcceptedParam, AuthType, Confidence, Effort, Fields, Lifecycle, LifecycleStatus, Observation,
    ParamRange, ParamType, ProvenanceKind, ProvenanceSource, Thinking, ThinkingMode,
    ThinkingOptions,
};
use crate::models_dev::{Adapted, sha256_hex};
use crate::time::parse_rfc3339_ms;

pub const SOURCE_ID: &str = "modelparams.dev";
pub const API_URL: &str = "https://modelparams.dev/api/v1/models.json";

/// Wire paths that bound the length of the generated output (`_LEN_PATHS` in the prototype).
const LEN_PATHS: [&str; 5] = [
    "max_tokens",
    "max_completion_tokens",
    "max_output_tokens",
    "inferenceConfig.maxTokens",
    "generationConfig.maxOutputTokens",
];

/// Parameter groups whose numeric bounds are counts (tokens), so a negative bound is not a
/// plausible value (unlike sampling penalties, which legitimately start at -2).
const COUNT_GROUPS: [&str; 2] = ["reasoning", "generation_length"];

/// Adapts the raw bytes of `models.json`. `fetched_at_ms` is when the bytes were obtained.
pub fn adapt(bytes: &[u8], fetched_at_ms: i64) -> Result<Adapted, CatalogError> {
    let root: Value = serde_json::from_slice(bytes)?;
    let Value::Object(root) = root else {
        return Err(malformed("top level of the source is not an object"));
    };
    let Some(Value::String(generated_at)) = root.get("generatedAt") else {
        return Err(malformed("`generatedAt` is missing or not a string"));
    };
    parse_rfc3339_ms(generated_at)?;
    let Some(Value::Array(entries)) = root.get("models") else {
        return Err(malformed("`models` is missing or not an array"));
    };
    if let Some(declared) = root.get("count") {
        match declared.as_u64() {
            Some(n) if usize::try_from(n) == Ok(entries.len()) => {}
            Some(n) => {
                return Err(malformed(format!(
                    "`count` declares {n} models but `models` lists {} (truncated snapshot?)",
                    entries.len()
                )));
            }
            None => return Err(malformed("`count` is not a non-negative integer")),
        }
    }
    if entries.is_empty() {
        return Err(CatalogError::NoModels);
    }

    let sha256 = sha256_hex(bytes);
    let source = ProvenanceSource {
        source_id: SOURCE_ID.to_owned(),
        kind: ProvenanceKind::Bundled,
        origin: format!("{API_URL}#sha256={}", &sha256[..16]),
        revision: Some(generated_at.clone()),
        confidence: Confidence::Declared,
        observed_at_ms: Some(fetched_at_ms),
    };

    let mut findings = Findings::default();
    let mut keyed: Vec<((String, String, u8), Observation)> = Vec::with_capacity(entries.len());
    let mut seen = BTreeSet::new();
    for (index, entry) in entries.iter().enumerate() {
        let obs = adapt_entry(index, entry, &source, &mut findings)?;
        let auth_rank = obs.fields.auth_type.map_or(u8::MAX, |a| a as u8);
        let key = (
            obs.fields.provider.clone(),
            obs.fields.id.clone(),
            auth_rank,
        );
        if !seen.insert(key.clone()) {
            return Err(malformed(format!(
                "models[{index}] repeats {}/{} with the same authType",
                key.0, key.1
            )));
        }
        keyed.push((key, obs));
    }
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    let observations: Vec<Observation> = keyed.into_iter().map(|(_, o)| o).collect();
    let providers = observations
        .iter()
        .map(|o| o.fields.provider.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    Ok(Adapted {
        source,
        sha256,
        providers,
        observations,
        findings,
    })
}

fn malformed(what: impl Into<String>) -> CatalogError {
    CatalogError::Malformed(what.into())
}

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
}

fn adapt_entry(
    index: usize,
    entry: &Value,
    source: &ProvenanceSource,
    findings: &mut Findings,
) -> Result<Observation, CatalogError> {
    let Value::Object(e) = entry else {
        return Err(malformed(format!("models[{index}] is not an object")));
    };
    let required = |key: &str| match e.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Ok(s.as_str()),
        _ => Err(malformed(format!(
            "models[{index}] has no non-empty string `{key}`"
        ))),
    };
    let provider = required("provider")?;
    let model = required("model")?;
    let auth_type = required("authType")?;
    let api_surface = required("apiSurface")?;
    let Some(Value::Array(raw_params)) = e.get("params") else {
        return Err(malformed(format!(
            "models[{index}] ({provider}/{model}) has no `params` array"
        )));
    };

    let mut cx = Cx {
        provider,
        id: model,
        findings,
    };
    let mut fields = Fields::empty(provider, model);
    fields.auth_type = AuthType::parse(auth_type);
    if fields.auth_type.is_none() {
        cx.reject(
            "authType",
            &Value::from(auth_type),
            "authType outside the contract enum",
        );
    }

    let params: Vec<AcceptedParam> = raw_params
        .iter()
        .enumerate()
        .filter_map(|(i, p)| adapt_param(&mut cx, i, p))
        .collect();

    let has_reasoning = params
        .iter()
        .any(|p| p.group.as_deref() == Some("reasoning"));
    let efforts = collect_efforts(&mut cx, &params);
    let max_tokens = params
        .iter()
        .filter(|p| LEN_PATHS.contains(&p.path.as_str()))
        .filter_map(|p| p.range.as_ref()?.max)
        .rfind(|&max| max > 0.0)
        .map(|max| max as u64);

    fields.max_tokens = max_tokens;
    // Absence of reasoning params is NOT evidence that the model does not reason.
    fields.reasoning = has_reasoning.then_some(true);
    fields.thinking = (!efforts.is_empty()).then(|| Thinking {
        mode: ThinkingMode::Effort,
        options: Some(ThinkingOptions {
            can_disable: efforts.contains(&Effort::Off).then_some(true),
            min_budget_tokens: None,
            max_budget_tokens: None,
            dynamic_budget: None,
        }),
        efforts: efforts.into_iter().collect(),
        default_level: None,
        effort_map: None,
    });
    fields.lifecycle = Some(adapt_lifecycle(&mut cx, e));
    fields.accepted_params = Some(params);

    let mut raw = Map::new();
    raw.insert("apiSurface".to_owned(), Value::from(api_surface));
    if let Some(wire_id) = cx.string(e, "wireId", "raw.wireId") {
        raw.insert("wireId".to_owned(), Value::from(wire_id));
    }
    let mut obs = Observation::new(source.clone(), fields);
    obs.raw = Some(raw);
    Ok(obs)
}

/// Effort levels named by the reasoning enum parameters whose path mentions `effort`.
fn collect_efforts(cx: &mut Cx<'_>, params: &[AcceptedParam]) -> BTreeSet<Effort> {
    let mut efforts = BTreeSet::new();
    let effort_params = params.iter().filter(|p| {
        p.group.as_deref() == Some("reasoning")
            && p.param_type == ParamType::Enum
            && p.path.to_lowercase().contains("effort")
    });
    for p in effort_params {
        for v in p.values.iter().flatten() {
            match v.as_str().and_then(Effort::parse) {
                Some(e) => {
                    efforts.insert(e);
                }
                None => cx.reject("thinking.efforts", v, "effort outside the contract enum"),
            }
        }
    }
    efforts
}

fn adapt_param(cx: &mut Cx<'_>, index: usize, p: &Value) -> Option<AcceptedParam> {
    let Value::Object(o) = p else {
        cx.reject(&format!("acceptedParams[{index}]"), p, "expected an object");
        return None;
    };
    let path = match o.get("path") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        other => {
            cx.reject(
                &format!("acceptedParams[{index}].path"),
                other.unwrap_or(&Value::Null),
                "expected a non-empty string",
            );
            return None;
        }
    };
    let at = |suffix: &str| format!("acceptedParams[{path}]{suffix}");

    let param_type = match o.get("type") {
        Some(Value::String(s)) => match ParamType::parse(s) {
            Some(t) => t,
            None => {
                cx.reject(
                    &at(".type"),
                    &Value::from(s.as_str()),
                    "type outside the contract enum",
                );
                return None;
            }
        },
        other => {
            cx.reject(
                &at(".type"),
                other.unwrap_or(&Value::Null),
                "expected a string",
            );
            return None;
        }
    };
    let group = cx.string(o, "group", &at(".group"));
    let description = o.get("description").and_then(Value::as_str).unwrap_or("");
    let values = match o.get("values") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => Some(a.clone()),
        Some(other) => {
            cx.reject(&at(".values"), other, "expected an array");
            None
        }
    };
    let bounded = group.as_deref().is_some_and(|g| COUNT_GROUPS.contains(&g));
    let range = match o.get("range") {
        None | Some(Value::Null) => None,
        Some(Value::Object(r)) => {
            let mut bound = |key: &str, count_like: bool| {
                let field = at(&format!(".range.{key}"));
                number(cx, r.get(key), &field, count_like && bounded, description)
            };
            let range = ParamRange {
                min: bound("min", true),
                max: bound("max", true),
                step: bound("step", false),
            };
            (range.min.is_some() || range.max.is_some() || range.step.is_some()).then_some(range)
        }
        Some(other) => {
            cx.reject(&at(".range"), other, "expected an object");
            None
        }
    };
    let default = match o.get("default") {
        Some(v @ Value::Number(_)) => {
            number(cx, Some(v), &at(".default"), bounded, description).map(|_| v.clone())
        }
        other => other.cloned(),
    };

    Some(AcceptedParam {
        path,
        param_type,
        group,
        values,
        range,
        default,
        conditional: Some(o.contains_key("applicability")),
    })
}

/// A numeric member of a parameter (range bound or default). Non-numbers are rejected. On a
/// count-like parameter (`check_negative`) a negative value is rejected too, except `-1` when the
/// parameter's own description documents it: that is the provider's spelling of "dynamic" /
/// "no enforcement" and is the value to send on the wire, so it is kept and recorded as a sentinel.
fn number(
    cx: &mut Cx<'_>,
    value: Option<&Value>,
    field: &str,
    check_negative: bool,
    description: &str,
) -> Option<f64> {
    let value = value?;
    let n = match value {
        Value::Null => return None,
        Value::Number(n) => n.as_f64()?,
        other => {
            cx.reject(field, other, "expected a number");
            return None;
        }
    };
    if check_negative && n < 0.0 {
        if n == -1.0 && description.contains("-1") {
            cx.sentinel(
                field,
                value,
                "kept as the wire value",
                &format!("the source documents -1 for this parameter: \"{description}\""),
            );
        } else {
            cx.reject(
                field,
                value,
                "negative value on a token-count parameter, not documented by the source",
            );
            return None;
        }
    }
    Some(n)
}

fn adapt_lifecycle(cx: &mut Cx<'_>, e: &Map<String, Value>) -> Lifecycle {
    let status = match e.get("status") {
        None | Some(Value::Null) => LifecycleStatus::Unknown,
        Some(Value::String(s)) => match s.as_str() {
            "active" => LifecycleStatus::Live,
            "deprecated" => LifecycleStatus::Deprecated,
            "retired" => LifecycleStatus::Retired,
            _ => {
                cx.reject(
                    "lifecycle.status",
                    &Value::from(s.as_str()),
                    "unknown status",
                );
                LifecycleStatus::Unknown
            }
        },
        Some(other) => {
            cx.reject("lifecycle.status", other, "expected a string");
            LifecycleStatus::Unknown
        }
    };
    let shutdown_on = cx
        .string(e, "shutdownOn", "lifecycle.shutdownOn")
        .filter(|d| {
            let ok = is_iso_date(d);
            if !ok {
                cx.reject(
                    "lifecycle.shutdownOn",
                    &Value::from(d.as_str()),
                    "not a YYYY-MM-DD date",
                );
            }
            ok
        });
    Lifecycle {
        status,
        replacement: cx.string(e, "replacement", "lifecycle.replacement"),
        shutdown_on,
    }
}

fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        })
}

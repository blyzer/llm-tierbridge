//! router-for-me/models adapter: what CLIProxyAPI serves, per channel -> [`Observation`]s.
//!
//! Input is a snapshot envelope (`tools/snapshot-git.mjs router-for-me.models`) carrying
//! `files["models.json"]` (channel -> model list), `files["devin_models.json"]`, a projection of
//! `files["codex_client_models.json"]` (facts only, no prompt text), the native-capabilities
//! evidence file and, optionally, `retired[]` derived from the git history.
//!
//! The upstream repository has no LICENSE: only factual data is read, and the descriptor says so.
//! The `config` member of an entry (client header overrides) is channel plumbing, not catalog data,
//! and is never read.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};

use crate::CatalogError;
use crate::diagnostics::Findings;
use crate::model::{
    Confidence, Effort, Fields, Lifecycle, LifecycleStatus, Modality, ModelKind, Observation,
    ProvenanceSource, Thinking, ThinkingMode, ThinkingOptions,
};
use crate::models_dev::{Adapted, sha256_hex};
use crate::snapshot::{Cx, Envelope, expect_array, expect_object, malformed, required_str};

pub const SOURCE_ID: &str = "router-for-me.models";
pub const REPOSITORY_URL: &str = "https://github.com/router-for-me/models";
const NOTE: &str = "no upstream LICENSE: factual data only";

const CATALOG_FILES: [&str; 2] = ["models.json", "devin_models.json"];
const CODEX_FILE: &str = "codex_client_models.json";
const EVIDENCE_FILE: &str = "native-capabilities-evidence.json";

/// One model entry to adapt: a served one, or one retired from the catalog.
struct Row<'a> {
    provider: &'a str,
    id: &'a str,
    model: &'a Map<String, Value>,
    /// Present for retired entries: where in the git history it was last served and removed.
    retirement: Option<Map<String, Value>>,
}

/// Adapts a snapshot envelope. `fetched_at_ms` is when the bytes were obtained.
pub fn adapt(bytes: &[u8], fetched_at_ms: i64) -> Result<Adapted, CatalogError> {
    let env = Envelope::parse(bytes, SOURCE_ID)?;
    let sha256 = sha256_hex(bytes);
    let source = env.descriptor(SOURCE_ID, Confidence::Declared, NOTE, fetched_at_ms);
    let files = expect_object(env.root.get("files"), "`files`")?;

    let mut rows: Vec<Row<'_>> = Vec::new();
    let mut seen: BTreeSet<(&str, &str)> = BTreeSet::new();
    for file in CATALOG_FILES {
        let Some(content) = files.get(file) else {
            if file == CATALOG_FILES[0] {
                return Err(malformed(format!("`files` has no {file}")));
            }
            continue;
        };
        for (section, list) in expect_object(Some(content), file)? {
            let list = expect_array(Some(list), &format!("{file}: section `{section}`"))?;
            for (index, entry) in list.iter().enumerate() {
                let Value::Object(model) = entry else {
                    return Err(malformed(format!(
                        "{file}: `{section}`[{index}] is not an object"
                    )));
                };
                let id = required_str(model, "id", &format!("{file}: `{section}`[{index}]"))?;
                if !seen.insert((section, id)) {
                    return Err(malformed(format!(
                        "{file}: `{section}` lists `{id}` more than once"
                    )));
                }
                rows.push(Row {
                    provider: section,
                    id,
                    model,
                    retirement: None,
                });
            }
        }
    }
    if rows.is_empty() {
        return Err(CatalogError::NoModels);
    }

    if let Some(retired) = env.root.get("retired") {
        for (index, entry) in expect_array(Some(retired), "`retired`")?.iter().enumerate() {
            let what = format!("retired[{index}]");
            let obj = expect_object(Some(entry), &what)?;
            let provider = required_str(obj, "provider", &what)?;
            let id = required_str(obj, "id", &what)?;
            let model = expect_object(obj.get("model"), &format!("{what}.model"))?;
            if !seen.insert((provider, id)) {
                return Err(malformed(format!(
                    "{what} repeats `{provider}/{id}`, which is served or retired already"
                )));
            }
            let mut info = Map::new();
            for key in ["lastServedCommit", "removedInCommit", "removedAt"] {
                info.insert(key.to_owned(), json!(required_str(obj, key, &what)?));
            }
            rows.push(Row {
                provider,
                id,
                model,
                retirement: Some(info),
            });
        }
    }

    let codex = codex_by_slug(files)?;
    let evidence = match files.get(EVIDENCE_FILE) {
        None => None,
        Some(v) => Some(expect_object(Some(v), EVIDENCE_FILE)?),
    };

    let mut findings = Findings::default();
    let mut observations: Vec<Observation> = rows
        .iter()
        .map(|row| adapt_row(row, &source, &codex, evidence, &mut findings))
        .collect();
    observations.sort_by(|a, b| {
        (&a.fields.provider, &a.fields.id).cmp(&(&b.fields.provider, &b.fields.id))
    });
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

/// The Codex client template entries by `slug`. Slugs that no `codex-*` channel lists (e.g.
/// `gpt-reserve`) create no observation; the template only annotates served entries.
fn codex_by_slug(
    files: &Map<String, Value>,
) -> Result<BTreeMap<&str, &Map<String, Value>>, CatalogError> {
    let mut out = BTreeMap::new();
    let Some(content) = files.get(CODEX_FILE) else {
        return Ok(out);
    };
    let root = expect_object(Some(content), CODEX_FILE)?;
    for (index, entry) in expect_array(root.get("models"), "codex template `models`")?
        .iter()
        .enumerate()
    {
        let what = format!("codex template models[{index}]");
        let entry = expect_object(Some(entry), &what)?;
        out.insert(required_str(entry, "slug", &what)?, entry);
    }
    Ok(out)
}

fn adapt_row(
    row: &Row<'_>,
    source: &ProvenanceSource,
    codex: &BTreeMap<&str, &Map<String, Value>>,
    evidence: Option<&Map<String, Value>>,
    findings: &mut Findings,
) -> Observation {
    let m = row.model;
    let codex_entry = row
        .provider
        .starts_with("codex-")
        .then(|| codex.get(row.id).copied())
        .flatten();
    let mut cx = Cx {
        source_id: SOURCE_ID,
        provider: row.provider,
        id: row.id,
        findings,
    };

    let mut fields = Fields::empty(row.provider, row.id);
    fields.name = cx.string(m, "display_name", "name");
    let input_limit = cx.token_limit(m, "inputTokenLimit", "maxInputTokens");
    let context = cx.token_limit(m, "context_length", "contextWindow");
    // Gemini-native entries publish only `inputTokenLimit`, which is their context window; when
    // both are published (Copilot) the input limit is the separate, smaller `maxInputTokens`.
    fields.context_window = context.or(input_limit);
    fields.max_input_tokens = if context.is_some() { input_limit } else { None };
    let completion = cx.token_limit(m, "max_completion_tokens", "maxTokens");
    let output_limit = cx.token_limit(m, "outputTokenLimit", "maxTokens");
    fields.max_tokens = completion.or(output_limit);
    fields.input = cx.modalities(m, "supportedInputModalities", "input");
    fields.output = cx.modalities(m, "supportedOutputModalities", "output");
    fields.kind = match fields.output.as_deref() {
        Some([Modality::Image]) => Some(ModelKind::Image),
        Some(out) if out.contains(&Modality::Text) => Some(ModelKind::Chat),
        _ => None,
    };

    let default_level = codex_entry
        .and_then(|c| c.get("default_reasoning_level"))
        .and_then(Value::as_str);
    match m.get("thinking") {
        None | Some(Value::Null) => {}
        Some(Value::Object(t)) => {
            fields.reasoning = Some(true);
            fields.thinking = Some(adapt_thinking(&mut cx, t, default_level));
        }
        Some(other) => cx.reject("thinking", other, "expected an object"),
    }

    fields.supports_tools = match m.get("supported_parameters") {
        None | Some(Value::Null) => None,
        // Listing the parameters without `tools` is not evidence that tools are refused.
        Some(Value::Array(params)) => params.iter().any(|p| p == "tools").then_some(true),
        Some(other) => {
            cx.reject(
                "supportsTools",
                other,
                "supported_parameters is not an array",
            );
            None
        }
    };
    fields.lifecycle = Some(Lifecycle {
        status: if row.retirement.is_some() {
            LifecycleStatus::Retired
        } else {
            LifecycleStatus::Live
        },
        replacement: None,
        shutdown_on: None,
    });

    let mut raw = Map::new();
    for (key, name) in [
        ("owned_by", "ownedBy"),
        ("type", "type"),
        ("created", "created"),
        ("version", "version"),
        ("name", "resourceName"),
        ("supportedGenerationMethods", "generationMethods"),
        ("supported_parameters", "supportedParameters"),
        ("support_configuration_update", "supportConfigurationUpdate"),
        ("supports_web_search", "supportsWebSearch"),
    ] {
        if let Some(v) = m.get(key) {
            raw.insert(name.to_owned(), v.clone());
        }
    }
    if let Some(caps) = native_capabilities(&mut cx, m, evidence) {
        raw.insert("nativeCapabilities".to_owned(), caps);
    }
    if let Some(c) = codex_entry {
        let mut template = c.clone();
        template.remove("slug");
        raw.insert("codexClient".to_owned(), Value::Object(template));
    }
    if let Some(info) = &row.retirement {
        raw.insert("retirement".to_owned(), Value::Object(info.clone()));
    }

    let mut obs = Observation::new(source.clone(), fields);
    obs.raw = (!raw.is_empty()).then_some(raw);
    obs
}

fn adapt_thinking(
    cx: &mut Cx<'_>,
    t: &Map<String, Value>,
    default_level: Option<&str>,
) -> Thinking {
    let efforts = t
        .get("levels")
        .map(|v| cx.efforts(v, "thinking.efforts"))
        .unwrap_or_default();
    let zero_allowed = cx.boolean(t, "zero_allowed", "thinking.canDisable");
    let min = cx.non_negative(t.get("min"), "thinking.minBudgetTokens");
    let max = cx.non_negative(t.get("max"), "thinking.maxBudgetTokens");
    let dynamic = cx.boolean(t, "dynamic_allowed", "thinking.dynamicBudget");
    // A listed `none` level means thinking can be switched off.
    let can_disable = zero_allowed.or(efforts.contains(&Effort::Off).then_some(true));
    let mode = if !efforts.is_empty() {
        ThinkingMode::Effort
    } else if min.is_some() || max.is_some() {
        ThinkingMode::Budget
    } else {
        ThinkingMode::Unknown
    };
    let options = (can_disable.is_some() || min.is_some() || max.is_some() || dynamic.is_some())
        .then_some(ThinkingOptions {
            can_disable,
            min_budget_tokens: min,
            max_budget_tokens: max,
            dynamic_budget: dynamic,
        });
    let default_level = default_level.and_then(|d| {
        let parsed = Effort::parse(d);
        if parsed.is_none() {
            cx.reject(
                "thinking.defaultLevel",
                &json!(d),
                "effort outside the contract enum",
            );
        }
        parsed
    });
    Thinking {
        mode,
        efforts,
        options,
        default_level,
        effort_map: None,
    }
}

/// `native_capabilities` is three-state: `true`, `false` or absent (unknown). Absence stays
/// absent here, and the evidence file's entry for the exact channel/model pair rides along.
fn native_capabilities(
    cx: &mut Cx<'_>,
    m: &Map<String, Value>,
    evidence: Option<&Map<String, Value>>,
) -> Option<Value> {
    let mut out = Map::new();
    match m.get("native_capabilities") {
        None | Some(Value::Null) => {}
        Some(Value::Object(caps)) => {
            for (key, value) in caps {
                match (key.as_str(), value) {
                    ("web_search", Value::Bool(b)) => {
                        out.insert("webSearch".to_owned(), json!(b));
                    }
                    ("web_search", other) => cx.reject(
                        "nativeCapabilities.webSearch",
                        other,
                        "three-state capability: expected true or false",
                    ),
                    _ => {
                        out.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        Some(other) => cx.reject("nativeCapabilities", other, "expected an object"),
    }
    let proof = evidence
        .and_then(|e| e.get(cx.provider))
        .and_then(Value::as_object)
        .and_then(|section| section.get(cx.id));
    if let Some(proof) = proof {
        out.insert("evidence".to_owned(), proof.clone());
    }
    (!out.is_empty()).then_some(Value::Object(out))
}

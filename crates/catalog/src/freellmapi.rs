//! freellmapi adapter: the open model registry of `tashfeenahmed/freellmapi` -> [`Observation`]s.
//!
//! The registry is not a data file in the project: it is the state of the `models` and
//! `embedding_models` tables after replaying its migrations. `tools/snapshot-freellmapi.mjs` does
//! the replay (in-memory SQLite, no network) and writes the tables into a snapshot envelope; this
//! adapter reads only that JSON, never TypeScript. The signed Premium catalog feed is not used.
//!
//! The free-tier limits (`rpm`/`rpd`/`tpm`/`tpd`, monthly token budget), the ranks, the
//! paid-equivalent prices (from `db/model-pricing.ts`, an estimate for the "savings" statistic, not
//! a list price) and the quirk slugs have no field in the schema and go to `raw`.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};

use crate::CatalogError;
use crate::diagnostics::Findings;
use crate::model::{
    Confidence, Fields, Lifecycle, LifecycleStatus, Modality, ModelKind, Observation,
    ProvenanceSource,
};
use crate::models_dev::{Adapted, sha256_hex};
use crate::snapshot::{Cx, Envelope, expect_array, expect_object, malformed, required_str};

pub const SOURCE_ID: &str = "freellmapi";
pub const REPOSITORY_URL: &str = "https://github.com/tashfeenahmed/freellmapi";
const NOTE: &str = "registry replayed from migrations; free-tier limits, not list prices";

/// Columns each dumped table must have: a rename upstream is a structural error, not a table of
/// silent `null`s.
const MODELS_COLUMNS: [&str; 17] = [
    "platform",
    "model_id",
    "display_name",
    "intelligence_rank",
    "speed_rank",
    "size_label",
    "rpm_limit",
    "rpd_limit",
    "tpm_limit",
    "tpd_limit",
    "monthly_token_budget",
    "context_window",
    "enabled",
    "supports_vision",
    "supports_tools",
    "paid_input_per_m",
    "paid_output_per_m",
];
const EMBEDDING_COLUMNS: [&str; 11] = [
    "platform",
    "model_id",
    "display_name",
    "family",
    "dimensions",
    "max_input_tokens",
    "enabled",
    "rpm_limit",
    "rpd_limit",
    "tpm_limit",
    "tpd_limit",
];
const QUIRK_COLUMNS: [&str; 2] = ["id", "slug"];
const QUIRK_TARGET_COLUMNS: [&str; 3] = ["quirk_id", "platform", "model_glob"];

/// A quirk's slug and the platform/model-glob pairs it applies to.
struct QuirkTarget {
    slug: String,
    platform: String,
    glob: Option<String>,
}

/// Adapts a snapshot envelope. `fetched_at_ms` is when the bytes were obtained.
pub fn adapt(bytes: &[u8], fetched_at_ms: i64) -> Result<Adapted, CatalogError> {
    let env = Envelope::parse(bytes, SOURCE_ID)?;
    let sha256 = sha256_hex(bytes);
    let source = env.descriptor(SOURCE_ID, Confidence::Declared, NOTE, fetched_at_ms);
    let tables = expect_object(env.root.get("tables"), "`tables`")?;
    let models = table_rows(tables, "models", &MODELS_COLUMNS)?;
    let embeddings = table_rows(tables, "embedding_models", &EMBEDDING_COLUMNS)?;
    let quirks = quirk_targets(tables)?;

    let mut findings = Findings::default();
    let mut observations = Vec::new();
    let mut seen = BTreeSet::new();
    for (table, rows) in [("models", &models), ("embedding_models", &embeddings)] {
        for (index, row) in rows.iter().enumerate() {
            let what = format!("`{table}`[{index}]");
            let platform = required_str(row, "platform", &what)?;
            let id = required_str(row, "model_id", &what)?;
            if !seen.insert((platform, id)) {
                return Err(malformed(format!(
                    "{what}: `{platform}/{id}` is listed more than once"
                )));
            }
            let mut cx = Cx {
                source_id: SOURCE_ID,
                provider: platform,
                id,
                findings: &mut findings,
            };
            observations.push(if table == "models" {
                adapt_chat(&mut cx, &source, row, &quirks)
            } else {
                adapt_embedding(&mut cx, &source, row, &quirks)
            });
        }
    }
    if observations.is_empty() {
        return Err(CatalogError::NoModels);
    }
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

/// The rows of `tables[name]`, after checking that every column in `required` was dumped.
fn table_rows<'a>(
    tables: &'a Map<String, Value>,
    name: &str,
    required: &[&str],
) -> Result<Vec<&'a Map<String, Value>>, CatalogError> {
    let what = format!("table `{name}`");
    let table = expect_object(tables.get(name), &what)?;
    let columns: BTreeSet<&str> = expect_array(table.get("columns"), &format!("{what} `columns`"))?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    if let Some(missing) = required.iter().find(|c| !columns.contains(**c)) {
        return Err(malformed(format!(
            "{what} has no column `{missing}` (schema drift upstream?)"
        )));
    }
    expect_array(table.get("rows"), &format!("{what} `rows`"))?
        .iter()
        .enumerate()
        .map(|(i, row)| expect_object(Some(row), &format!("{what} rows[{i}]")))
        .collect()
}

fn quirk_targets(tables: &Map<String, Value>) -> Result<Vec<QuirkTarget>, CatalogError> {
    let slugs: BTreeMap<u64, &str> = table_rows(tables, "quirks", &QUIRK_COLUMNS)?
        .into_iter()
        .map(|q| {
            let id = q
                .get("id")
                .and_then(Value::as_u64)
                .ok_or_else(|| malformed("table `quirks`: `id` is not an integer"))?;
            Ok((id, required_str(q, "slug", "table `quirks`")?))
        })
        .collect::<Result<_, CatalogError>>()?;
    table_rows(tables, "quirk_targets", &QUIRK_TARGET_COLUMNS)?
        .into_iter()
        .map(|t| {
            let quirk = t
                .get("quirk_id")
                .and_then(Value::as_u64)
                .ok_or_else(|| malformed("table `quirk_targets`: `quirk_id` is not an integer"))?;
            let slug = slugs.get(&quirk).ok_or_else(|| {
                malformed(format!("table `quirk_targets` names unknown quirk {quirk}"))
            })?;
            Ok(QuirkTarget {
                slug: (*slug).to_owned(),
                platform: required_str(t, "platform", "table `quirk_targets`")?.to_owned(),
                glob: match t.get("model_glob") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(g)) => Some(g.clone()),
                    Some(other) => {
                        return Err(malformed(format!(
                            "table `quirk_targets`: `model_glob` is not a string ({other})"
                        )));
                    }
                },
            })
        })
        .collect()
}

/// `*`-only glob, as the quirk targets use (`*:free`, `*gemma*`).
fn glob_matches(glob: &str, text: &str) -> bool {
    let parts: Vec<&str> = glob.split('*').collect();
    let [first, middle @ .., last] = parts.as_slice() else {
        return glob == text;
    };
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

fn quirks_for(quirks: &[QuirkTarget], platform: &str, id: &str) -> Vec<String> {
    let slugs: BTreeSet<&str> = quirks
        .iter()
        .filter(|q| q.platform == platform && q.glob.as_deref().is_none_or(|g| glob_matches(g, id)))
        .map(|q| q.slug.as_str())
        .collect();
    slugs.into_iter().map(str::to_owned).collect()
}

/// A SQLite boolean column: `0`/`1`/`null`.
fn flag(cx: &mut Cx<'_>, row: &Map<String, Value>, key: &str, field: &str) -> Option<bool> {
    match row.get(key)? {
        Value::Null => None,
        Value::Number(n) if n.as_u64() == Some(0) => Some(false),
        Value::Number(n) if n.as_u64() == Some(1) => Some(true),
        other => {
            cx.reject(field, other, "expected 0 or 1");
            None
        }
    }
}

/// USD per million tokens: a non-negative number.
fn price(cx: &mut Cx<'_>, row: &Map<String, Value>, key: &str, field: &str) -> Option<f64> {
    let value = row.get(key)?;
    match value {
        Value::Null => None,
        Value::Number(n) => match n.as_f64() {
            Some(v) if v >= 0.0 => Some(v),
            _ => {
                cx.reject(field, value, "negative price");
                None
            }
        },
        other => {
            cx.reject(field, other, "price is not a number");
            None
        }
    }
}

/// The free-tier quota columns, as `raw.freeTier` (only the ones the registry fills).
fn free_tier(cx: &mut Cx<'_>, row: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (key, name) in [
        ("rpm_limit", "rpm"),
        ("rpd_limit", "rpd"),
        ("tpm_limit", "tpm"),
        ("tpd_limit", "tpd"),
    ] {
        if let Some(v) = cx.non_negative(row.get(key), &format!("raw.freeTier.{name}")) {
            out.insert(name.to_owned(), json!(v));
        }
    }
    if let Some(budget) = cx
        .string(
            row,
            "monthly_token_budget",
            "raw.freeTier.monthlyTokenBudget",
        )
        .filter(|b| !b.is_empty())
    {
        out.insert("monthlyTokenBudget".to_owned(), json!(budget));
    }
    out
}

fn adapt_chat(
    cx: &mut Cx<'_>,
    source: &ProvenanceSource,
    row: &Map<String, Value>,
    quirks: &[QuirkTarget],
) -> Observation {
    let mut fields = Fields::empty(cx.provider, cx.id);
    fields.name = cx.string(row, "display_name", "name");
    fields.kind = Some(ModelKind::Chat);
    fields.context_window = cx.token_limit(row, "context_window", "contextWindow");
    // The columns default to 0, which cannot be told apart from "never classified": only a 1 is a
    // claim. A 0 stays unknown rather than becoming "no vision" / "no tools".
    fields.input = flag(cx, row, "supports_vision", "input")
        .filter(|v| *v)
        .map(|_| vec![Modality::Image, Modality::Text]);
    fields.supports_tools = flag(cx, row, "supports_tools", "supportsTools").filter(|v| *v);
    // Present in the shipped registry = served by freellmapi.
    fields.lifecycle = Some(Lifecycle {
        status: LifecycleStatus::Live,
        replacement: None,
        shutdown_on: None,
    });

    let mut raw = Map::new();
    let tier = free_tier(cx, row);
    if !tier.is_empty() {
        raw.insert("freeTier".to_owned(), Value::Object(tier));
    }
    let mut ranks = Map::new();
    for (key, name) in [
        ("intelligence_rank", "intelligence"),
        ("speed_rank", "speed"),
    ] {
        if let Some(v) = cx.non_negative(row.get(key), &format!("raw.ranks.{name}")) {
            ranks.insert(name.to_owned(), json!(v));
        }
    }
    if let Some(label) = cx
        .string(row, "size_label", "raw.ranks.sizeLabel")
        .filter(|l| !l.is_empty())
    {
        ranks.insert("sizeLabel".to_owned(), json!(label));
    }
    if !ranks.is_empty() {
        raw.insert("ranks".to_owned(), Value::Object(ranks));
    }
    // Paid-equivalent: what the nearest paid API charges, an estimate used for savings analytics.
    let mut paid = Map::new();
    for (key, name) in [
        ("paid_input_per_m", "input"),
        ("paid_output_per_m", "output"),
    ] {
        if let Some(v) = price(
            cx,
            row,
            key,
            &format!("raw.paidEquivalentUsdPerMtok.{name}"),
        ) {
            paid.insert(name.to_owned(), json!(v));
        }
    }
    if !paid.is_empty() {
        raw.insert("paidEquivalentUsdPerMtok".to_owned(), Value::Object(paid));
    }
    if let Some(enabled) = flag(cx, row, "enabled", "raw.enabledByDefault") {
        raw.insert("enabledByDefault".to_owned(), json!(enabled));
    }
    if let Some(tag) = cx.string(row, "source", "raw.registrySource") {
        raw.insert("registrySource".to_owned(), json!(tag));
    }
    insert_quirks(&mut raw, quirks, cx.provider, cx.id);

    let mut obs = Observation::new(source.clone(), fields);
    obs.raw = (!raw.is_empty()).then_some(raw);
    obs
}

fn adapt_embedding(
    cx: &mut Cx<'_>,
    source: &ProvenanceSource,
    row: &Map<String, Value>,
    quirks: &[QuirkTarget],
) -> Observation {
    let mut fields = Fields::empty(cx.provider, cx.id);
    fields.name = cx.string(row, "display_name", "name");
    fields.kind = Some(ModelKind::Embedding);
    fields.max_input_tokens = cx.token_limit(row, "max_input_tokens", "maxInputTokens");
    fields.lifecycle = Some(Lifecycle {
        status: LifecycleStatus::Live,
        replacement: None,
        shutdown_on: None,
    });

    let mut raw = Map::new();
    if let Some(dimensions) = cx.token_limit(row, "dimensions", "raw.dimensions") {
        raw.insert("dimensions".to_owned(), json!(dimensions));
    }
    if let Some(family) = cx.string(row, "family", "raw.family") {
        raw.insert("family".to_owned(), json!(family));
    }
    let tier = free_tier(cx, row);
    if !tier.is_empty() {
        raw.insert("freeTier".to_owned(), Value::Object(tier));
    }
    if let Some(label) = cx.string(row, "quota_label", "raw.quotaLabel") {
        raw.insert("quotaLabel".to_owned(), json!(label));
    }
    if let Some(enabled) = flag(cx, row, "enabled", "raw.enabledByDefault") {
        raw.insert("enabledByDefault".to_owned(), json!(enabled));
    }
    if let Some(tag) = cx.string(row, "source", "raw.registrySource") {
        raw.insert("registrySource".to_owned(), json!(tag));
    }
    insert_quirks(&mut raw, quirks, cx.provider, cx.id);

    let mut obs = Observation::new(source.clone(), fields);
    obs.raw = (!raw.is_empty()).then_some(raw);
    obs
}

fn insert_quirks(raw: &mut Map<String, Value>, quirks: &[QuirkTarget], platform: &str, id: &str) {
    let slugs = quirks_for(quirks, platform, id);
    if !slugs.is_empty() {
        raw.insert("quirks".to_owned(), json!(slugs));
    }
}

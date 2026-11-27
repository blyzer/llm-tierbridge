//! freellmapi adapter: fixture figures (108 chat models on 15 platforms), mapping, rejections and
//! shape failures. The adapter reads the replayed-registry snapshot, never TypeScript.

mod common;

use std::collections::BTreeMap;

use common::{fetched_at_ms, schema_validator, zstd_fixture};
use serde_json::{Value, json};
use tierbridge_catalog::CatalogError;
use tierbridge_catalog::freellmapi::{SOURCE_ID, adapt};
use tierbridge_catalog::models_dev::Adapted;

const FIXTURE: &str = "freellmapi.0d64e07.snapshot.json.zst";
const COMMIT: &str = "0d64e07b829e5d33b5d4db037ad841844073d5b1";

fn fixture() -> Adapted {
    adapt(&zstd_fixture(FIXTURE), fetched_at_ms()).unwrap()
}

fn row(a: &Adapted, provider: &str, id: &str) -> Value {
    let o = a
        .observations
        .iter()
        .find(|o| o.fields.provider == provider && o.fields.id == id)
        .unwrap_or_else(|| panic!("no observation {provider}/{id}"));
    serde_json::to_value(o).unwrap()
}

/// Minimal tables with one chat model and one quirk; `patch` edits the table set before encoding.
fn tables(patch: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut tables = json!({
        "models": {
            "columns": [
                "platform", "model_id", "display_name", "intelligence_rank", "speed_rank",
                "size_label", "rpm_limit", "rpd_limit", "tpm_limit", "tpd_limit",
                "monthly_token_budget", "context_window", "enabled", "supports_vision",
                "supports_tools", "paid_input_per_m", "paid_output_per_m"
            ],
            "rows": [{
                "platform": "groq", "model_id": "m", "display_name": "M", "intelligence_rank": 3,
                "speed_rank": 2, "size_label": "Large", "rpm_limit": 30, "rpd_limit": null,
                "tpm_limit": 6000, "tpd_limit": null, "monthly_token_budget": "~6M",
                "context_window": 131072, "enabled": 1, "supports_vision": 1, "supports_tools": 0,
                "paid_input_per_m": 0.1, "paid_output_per_m": 0.3
            }]
        },
        "embedding_models": {
            "columns": [
                "platform", "model_id", "display_name", "family", "dimensions",
                "max_input_tokens", "enabled", "rpm_limit", "rpd_limit", "tpm_limit", "tpd_limit"
            ],
            "rows": []
        },
        "quirks": { "columns": ["id", "slug"], "rows": [{"id": 1, "slug": "q"}] },
        "quirk_targets": {
            "columns": ["quirk_id", "platform", "model_glob"],
            "rows": [{"quirk_id": 1, "platform": "groq", "model_glob": null}]
        }
    });
    patch(&mut tables);
    serde_json::to_vec(&json!({
        "format": "tierbridge-snapshot/1",
        "sourceId": SOURCE_ID,
        "repository": "https://example.test/r",
        "commit": "0123456789abcdef0123456789abcdef01234567",
        "commitDate": "2026-11-25T23:35:54Z",
        "tables": tables,
    }))
    .unwrap()
}

fn malformed(bytes: &[u8]) -> String {
    match adapt(bytes, 0).unwrap_err() {
        CatalogError::Malformed(m) => m,
        other => panic!("expected Malformed, got {other:?}"),
    }
}

#[test]
fn fixture_reproduces_the_measured_registry() {
    let a = fixture();
    let chat: Vec<_> = a
        .observations
        .iter()
        .filter(|o| o.fields.kind == Some(tierbridge_catalog::model::ModelKind::Chat))
        .collect();
    assert_eq!(
        chat.len(),
        108,
        "docs/freellmapi-into-cpa.md: 108 chat models"
    );
    assert_eq!(a.observations.len() - chat.len(), 10, "and 10 embeddings");
    assert_eq!(a.providers, 15, "on 15 platforms");
    let mut per_platform: BTreeMap<&str, usize> = BTreeMap::new();
    for o in &chat {
        *per_platform.entry(o.fields.provider.as_str()).or_default() += 1;
    }
    assert_eq!(
        per_platform,
        [
            ("openrouter", 23),
            ("nvidia", 13),
            ("ollama", 11),
            ("cloudflare", 10),
            ("google", 9),
            ("groq", 9),
            ("mistral", 7),
            ("opencode", 6),
            ("cerebras", 4),
            ("cohere", 4),
            ("kilo", 4),
            ("zhipu", 3),
            ("huggingface", 3),
            ("pollinations", 1),
            ("llm7", 1),
        ]
        .into_iter()
        .collect()
    );
    assert!(
        a.findings.rejections.is_empty(),
        "{:?}",
        a.findings.rejections
    );
    assert_eq!(
        a.source.revision.as_deref(),
        Some(format!("{COMMIT}@2026-11-25T23:35:54Z").as_str())
    );
    let validator = schema_validator();
    for o in &a.observations {
        let v = serde_json::to_value(o).unwrap();
        assert!(
            validator.is_valid(&v),
            "{}/{}: {v}",
            o.fields.provider,
            o.fields.id
        );
    }
}

#[test]
fn chat_mapping_and_raw_extras() {
    let a = fixture();
    let v = row(&a, "google", "gemini-2.5-pro");
    assert_eq!(v["kind"], "chat");
    assert_eq!(v["name"], "Gemini 2.5 Pro");
    assert_eq!(v["contextWindow"], 1_048_576);
    assert_eq!(v["input"], json!(["image", "text"]));
    assert_eq!(v["supportsTools"], true);
    assert_eq!(v["lifecycle"]["status"], "live");
    assert!(
        v["cost"].is_null(),
        "paid-equivalent prices are not a list price"
    );
    assert_eq!(v["raw"]["freeTier"]["rpm"], 5);
    assert_eq!(v["raw"]["freeTier"]["rpd"], 50);
    assert_eq!(v["raw"]["freeTier"]["tpm"], 250_000);
    assert!(
        v["raw"]["freeTier"].get("tpd").is_none(),
        "null limits are absent, not zero"
    );
    assert_eq!(v["raw"]["freeTier"]["monthlyTokenBudget"], "~6M");
    assert_eq!(v["raw"]["ranks"]["intelligence"], 14);
    assert_eq!(v["raw"]["ranks"]["sizeLabel"], "Large");
    assert_eq!(v["raw"]["paidEquivalentUsdPerMtok"]["input"], 1.25);
    assert_eq!(v["raw"]["paidEquivalentUsdPerMtok"]["output"], 10.0);
    assert_eq!(v["raw"]["registrySource"], "catalog");
    assert_eq!(v["raw"]["enabledByDefault"], false);
}

#[test]
fn unclassified_zero_flags_and_missing_prices_stay_unknown() {
    let a = fixture();
    // `supports_vision` 0 is the column default, not a claim of "no vision".
    let v = row(&a, "openrouter", "qwen/qwen3-coder:free");
    assert!(v["input"].is_null());
    // 8 models have no paid equivalent: the key is absent, not 0.
    let v = row(&a, "openrouter", "poolside/laguna-m.1:free");
    assert!(v["raw"].get("paidEquivalentUsdPerMtok").is_none());
}

#[test]
fn quirks_apply_by_platform_and_glob() {
    let a = fixture();
    assert_eq!(
        row(&a, "kilo", "nvidia/nemotron-3-super-120b-a12b:free")["raw"]["quirks"][0],
        "keyless-anonymous"
    );
    let free = row(&a, "openrouter", "qwen/qwen3-coder:free");
    assert!(
        free["raw"]["quirks"]
            .as_array()
            .unwrap()
            .contains(&json!("or-free-cap-account-wide"))
    );
    let paid = row(&a, "google", "gemini-2.5-pro");
    assert!(paid["raw"].get("quirks").is_none());
}

#[test]
fn embeddings_are_their_own_kind() {
    let a = fixture();
    let v = row(&a, "google", "gemini-embedding-001");
    assert_eq!(v["kind"], "embedding");
    assert_eq!(v["maxInputTokens"], 2048);
    assert_eq!(v["raw"]["dimensions"], 3072);
    assert_eq!(v["raw"]["family"], "gemini-embedding-001");
    assert!(v["contextWindow"].is_null());
}

#[test]
fn out_of_contract_values_are_rejected_per_row() {
    let bytes = tables(|t| {
        let r = &mut t["models"]["rows"][0];
        r["context_window"] = json!(0);
        r["supports_vision"] = json!(7);
        r["rpm_limit"] = json!(-1);
        r["paid_input_per_m"] = json!(-0.5);
        r["display_name"] = json!(42);
    });
    let a = adapt(&bytes, 0).unwrap();
    let fields: Vec<&str> = a
        .findings
        .rejections
        .iter()
        .map(|d| d.field.as_str())
        .collect();
    for expected in [
        "contextWindow",
        "input",
        "raw.freeTier.rpm",
        "raw.paidEquivalentUsdPerMtok.input",
        "name",
    ] {
        assert!(fields.contains(&expected), "{expected} in {fields:?}");
    }
    let v = serde_json::to_value(&a.observations[0]).unwrap();
    assert!(v["contextWindow"].is_null() && v["input"].is_null());
    assert!(schema_validator().is_valid(&v));
}

#[test]
fn shape_failures_are_structural_errors() {
    assert!(matches!(
        adapt(b"{nope", 0).unwrap_err(),
        CatalogError::InvalidJson(_)
    ));
    assert!(malformed(b"[]").contains("not a JSON object"));
    // Replay produced no tables at all.
    let bare = serde_json::to_vec(&json!({
        "format": "tierbridge-snapshot/1", "sourceId": SOURCE_ID, "repository": "r",
        "commit": "0123456789abcdef0123456789abcdef01234567", "commitDate": "2026-11-25T23:35:54Z",
    }))
    .unwrap();
    assert!(malformed(&bare).contains("`tables`"));
    assert!(
        malformed(&tables(|t| {
            t.as_object_mut().unwrap().remove("models");
        }))
        .contains("table `models`")
    );
    // A renamed column upstream must fail the run, not turn into a table of nulls.
    assert!(
        malformed(&tables(|t| {
            t["models"]["columns"] = json!(["platform", "model_id"]);
        }))
        .contains("schema drift")
    );
    assert!(
        malformed(&tables(|t| {
            t["models"]["rows"] = json!({});
        }))
        .contains("`rows`")
    );
    assert!(
        malformed(&tables(|t| {
            t["models"]["rows"] = json!([7]);
        }))
        .contains("not an object")
    );
    assert!(
        malformed(&tables(|t| {
            t["models"]["rows"][0]["model_id"] = json!("");
        }))
        .contains("`model_id`")
    );
    assert!(
        malformed(&tables(|t| {
            let first = t["models"]["rows"][0].clone();
            t["models"]["rows"].as_array_mut().unwrap().push(first);
        }))
        .contains("more than once")
    );
    assert!(
        malformed(&tables(|t| {
            t["quirk_targets"]["rows"][0]["quirk_id"] = json!(99);
        }))
        .contains("unknown quirk")
    );
    assert!(matches!(
        adapt(
            &tables(|t| {
                t["models"]["rows"] = json!([]);
            }),
            0
        )
        .unwrap_err(),
        CatalogError::NoModels
    ));
}

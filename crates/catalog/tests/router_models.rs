//! router-for-me/models adapter: fixture figures, mapping, rejections and shape failures.

mod common;

use common::{fetched_at_ms, schema_validator, zstd_fixture};
use serde_json::{Value, json};
use tierbridge_catalog::CatalogError;
use tierbridge_catalog::models_dev::Adapted;
use tierbridge_catalog::router_models::{SOURCE_ID, adapt};

const FIXTURE: &str = "router-for-me.models.d02fed832.snapshot.json.zst";
const COMMIT: &str = "d02fed8320cc167558f283104b8b87fcc88d97f5";

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

/// A minimal valid envelope around `files` (plus optional extra top-level members).
fn envelope(files: Value, extra: Value) -> Vec<u8> {
    let mut root = json!({
        "format": "tierbridge-snapshot/1",
        "sourceId": SOURCE_ID,
        "repository": "https://example.test/r",
        "commit": "0123456789abcdef0123456789abcdef01234567",
        "commitDate": "2026-11-25T04:39:59Z",
        "files": files,
    });
    if let (Value::Object(r), Value::Object(e)) = (&mut root, extra) {
        r.extend(e);
    }
    serde_json::to_vec(&root).unwrap()
}

fn one_model(model: Value) -> Vec<u8> {
    envelope(json!({ "models.json": { "claude": [model] } }), json!({}))
}

fn malformed(bytes: &[u8]) -> String {
    match adapt(bytes, 0).unwrap_err() {
        CatalogError::Malformed(m) => m,
        other => panic!("expected Malformed, got {other:?}"),
    }
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn fixture_figures_and_schema() {
    let a = fixture();
    assert_eq!(a.observations.len(), 520);
    let live = a
        .observations
        .iter()
        .filter(|o| {
            o.fields.lifecycle.as_ref().unwrap().status
                == tierbridge_catalog::model::LifecycleStatus::Live
        })
        .count();
    assert_eq!(live, 272, "212 in models.json + 60 in devin_models.json");
    assert!(a.providers >= 15);
    // Only retired (older) records carry efforts outside the contract enum: `priority`, `auto`, `ultra`.
    let rejected: std::collections::BTreeMap<String, usize> =
        a.findings
            .rejections
            .iter()
            .fold(Default::default(), |mut m, d| {
                assert_eq!(d.field, "thinking.efforts");
                *m.entry(d.value.as_str().unwrap().to_owned()).or_default() += 1;
                m
            });
    assert_eq!(
        rejected,
        [("auto", 4), ("priority", 49), ("ultra", 1)]
            .map(|(k, v)| (k.to_owned(), v))
            .into()
    );
    assert_eq!(a.source.source_id, SOURCE_ID);
    assert_eq!(
        a.source.revision.as_deref(),
        Some(format!("{COMMIT}@2026-11-25T04:39:59Z").as_str())
    );
    assert!(a.source.origin.contains("no upstream LICENSE"));

    let validator = schema_validator();
    for o in &a.observations {
        let v = serde_json::to_value(o).unwrap();
        assert!(
            validator.is_valid(&v),
            "{}/{} is not schema-valid: {v}",
            o.fields.provider,
            o.fields.id
        );
    }
    let mut keys: Vec<_> = a
        .observations
        .iter()
        .map(|o| (&o.fields.provider, &o.fields.id))
        .collect();
    let sorted = keys.clone();
    keys.sort();
    keys.dedup();
    assert_eq!(keys, sorted, "sorted by (provider, id), no duplicates");
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn budget_thinking_and_limits() {
    let a = fixture();
    let v = row(&a, "claude", "claude-haiku-4-5-20251001");
    assert_eq!(v["name"], "Claude 4.5 Haiku");
    assert_eq!(v["contextWindow"], 200_000);
    assert_eq!(v["maxTokens"], 64_000);
    assert_eq!(v["input"], json!(["image", "text"]));
    assert_eq!(v["kind"], "chat");
    assert_eq!(v["reasoning"], true);
    assert_eq!(v["thinking"]["mode"], "budget");
    assert_eq!(v["thinking"]["canDisable"], true);
    assert_eq!(v["thinking"]["minBudgetTokens"], 1024);
    assert_eq!(v["thinking"]["maxBudgetTokens"], 128_000);
    assert_eq!(v["lifecycle"]["status"], "live");
    assert!(v["cost"].is_null(), "the catalog publishes no prices");
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn gemini_native_limits_are_the_context_window() {
    let a = fixture();
    let v = row(&a, "gemini", "gemini-2.5-pro");
    assert_eq!(v["contextWindow"], 1_048_576);
    assert!(v["maxInputTokens"].is_null());
    assert_eq!(v["maxTokens"], 65_536);
    assert_eq!(v["thinking"]["dynamicBudget"], true);
    assert_eq!(v["raw"]["resourceName"], "models/gemini-2.5-pro");
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn copilot_input_limit_is_separate_from_the_context_window() {
    let a = fixture();
    let v = row(&a, "copilot", "gpt-6-sol");
    assert_eq!(v["contextWindow"], 1_000_000);
    assert_eq!(v["maxInputTokens"], 872_000);
    // A listed `none` level is "thinking can be disabled".
    assert_eq!(v["thinking"]["efforts"][0], "off");
    assert_eq!(v["thinking"]["canDisable"], true);
    assert!(
        v["supportsTools"].is_null(),
        "no supported_parameters listed"
    );
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn codex_template_annotates_served_entries_only() {
    let a = fixture();
    let v = row(&a, "codex-pro", "gpt-5.6-terra");
    assert_eq!(
        v["contextWindow"], 921_000,
        "models.json stays authoritative"
    );
    assert_eq!(v["thinking"]["defaultLevel"], "medium");
    assert_eq!(v["raw"]["codexClient"]["supports_search_tool"], true);
    assert_eq!(v["raw"]["codexClient"]["max_context_window"], 872_000);
    assert_eq!(v["raw"]["nativeCapabilities"]["webSearch"], true);
    // `gpt-reserve` is in the template but in no channel: no observation.
    assert!(
        !a.observations.iter().any(|o| o.fields.id == "gpt-reserve"),
        "template-only slugs create no observation"
    );
    // The template's `ultra` effort stays verbatim in the template block, not in `efforts`.
    assert_eq!(
        row(&a, "codex-pro", "gpt-6.2-sol")["raw"]["codexClient"]["supported_reasoning_levels"]
            .as_array()
            .unwrap()
            .last()
            .unwrap(),
        "ultra"
    );
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn native_capabilities_are_three_state_with_evidence() {
    let a = fixture();
    let proven = row(&a, "claude", "claude-opus-5");
    assert_eq!(proven["raw"]["nativeCapabilities"]["webSearch"], true);
    assert!(
        proven["raw"]["nativeCapabilities"]["evidence"]["source"]
            .as_str()
            .unwrap()
            .starts_with("https://platform.claude.com/")
    );
    // Absent stays absent: never turned into `false`.
    let unknown = row(&a, "claude", "claude-haiku-4-5-20251001");
    assert!(unknown["raw"].get("nativeCapabilities").is_none());
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn image_only_output_is_an_image_model() {
    let a = fixture();
    let image = a
        .observations
        .iter()
        .map(|o| serde_json::to_value(o).unwrap())
        .find(|v| v["kind"] == "image")
        .expect("an image-only entry");
    assert!(image["contextWindow"].is_null());
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn retired_entries_come_from_history() {
    let a = fixture();
    let retired: Vec<_> = a
        .observations
        .iter()
        .filter(|o| {
            o.fields.lifecycle.as_ref().unwrap().status
                == tierbridge_catalog::model::LifecycleStatus::Retired
        })
        .collect();
    assert_eq!(retired.len(), 248);
    let v = serde_json::to_value(retired[0]).unwrap();
    assert_eq!(v["lifecycle"]["status"], "retired");
    assert!(
        v["lifecycle"]["shutdownOn"].is_null(),
        "a removal commit is not a shutdown date"
    );
    for key in ["lastServedCommit", "removedInCommit", "removedAt"] {
        assert!(v["raw"]["retirement"][key].is_string(), "{key}");
    }
}

#[test]
fn out_of_contract_values_are_rejected_per_row() {
    let bytes = one_model(json!({
        "id": "m", "display_name": "M", "context_length": 0, "max_completion_tokens": -5,
        "supportedInputModalities": ["text", "hologram"],
        "thinking": { "levels": ["low", "ultra"], "min": -1 },
        "native_capabilities": { "web_search": "yes" },
    }));
    let a = adapt(&bytes, 0).unwrap();
    let fields: Vec<&str> = a
        .findings
        .rejections
        .iter()
        .map(|d| d.field.as_str())
        .collect();
    for expected in [
        "contextWindow",
        "maxTokens",
        "input",
        "thinking.efforts",
        "thinking.minBudgetTokens",
        "nativeCapabilities.webSearch",
    ] {
        assert!(fields.contains(&expected), "{expected} in {fields:?}");
    }
    assert!(
        a.findings
            .rejections
            .iter()
            .all(|d| d.provider == "claude" && d.id == "m")
    );
    let v = serde_json::to_value(&a.observations[0]).unwrap();
    assert!(v["contextWindow"].is_null());
    assert_eq!(v["input"], json!(["text"]));
    assert_eq!(v["thinking"]["efforts"], json!(["low"]));
    assert!(schema_validator().is_valid(&v));
}

#[test]
fn shape_failures_are_structural_errors() {
    assert!(matches!(
        adapt(b"{not json", 0).unwrap_err(),
        CatalogError::InvalidJson(_)
    ));
    assert!(malformed(b"[]").contains("not a JSON object"));

    let ok = |files: Value| envelope(files, json!({}));
    let header = |patch: Value| {
        let mut root: Value = serde_json::from_slice(&one_model(json!({"id": "m"}))).unwrap();
        root.as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        serde_json::to_vec(&root).unwrap()
    };
    assert!(malformed(&header(json!({"format": "v2"}))).contains("`format`"));
    assert!(malformed(&header(json!({"sourceId": "dsh"}))).contains("`sourceId`"));
    assert!(malformed(&header(json!({"commit": "abc"}))).contains("`commit`"));
    assert!(matches!(
        adapt(&header(json!({"commitDate": "yesterday"})), 0).unwrap_err(),
        CatalogError::InvalidTimestamp { .. }
    ));

    assert!(
        malformed(
            &serde_json::to_vec(
                &json!({"format": "tierbridge-snapshot/1", "sourceId": SOURCE_ID,
        "repository": "r", "commit": "0123456789abcdef0123456789abcdef01234567",
        "commitDate": "2026-11-25T04:39:59Z"})
            )
            .unwrap()
        )
        .contains("`files`")
    );
    assert!(malformed(&ok(json!({"devin_models.json": {}}))).contains("no models.json"));
    assert!(malformed(&ok(json!({"models.json": []}))).contains("not an object"));
    assert!(malformed(&ok(json!({"models.json": {"claude": {}}}))).contains("not an array"));
    assert!(malformed(&ok(json!({"models.json": {"claude": [1]}}))).contains("not an object"));
    assert!(
        malformed(&ok(
            json!({"models.json": {"claude": [{"display_name": "x"}]}})
        ))
        .contains("`id`")
    );
    assert!(
        malformed(&ok(
            json!({"models.json": {"claude": [{"id": "a"}, {"id": "a"}]}})
        ))
        .contains("more than once")
    );
    assert!(matches!(
        adapt(&ok(json!({"models.json": {"claude": []}})), 0).unwrap_err(),
        CatalogError::NoModels
    ));
    // `retired` entries must be well formed and must not repeat a served model.
    let retired = |entry: Value| {
        envelope(
            json!({"models.json": {"claude": [{"id": "a"}]}}),
            json!({"retired": [entry]}),
        )
    };
    assert!(
        malformed(&retired(
            json!({"provider": "claude", "id": "a", "model": {"id": "a"},
        "lastServedCommit": "x", "removedInCommit": "y", "removedAt": "z"})
        ))
        .contains("served or retired already")
    );
    assert!(malformed(&retired(json!({"provider": "claude", "id": "b"}))).contains("model"));
}

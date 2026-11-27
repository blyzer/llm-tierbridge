//! dsh adapter: fixture figures, mapping, rejections and shape failures.

mod common;

use std::collections::BTreeMap;

use common::{fetched_at_ms, schema_validator, zstd_fixture};
use serde_json::{Value, json};
use tierbridge_catalog::CatalogError;
use tierbridge_catalog::dsh::{SOURCE_ID, adapt};
use tierbridge_catalog::models_dev::Adapted;

const FIXTURE: &str = "dsh.b1142bd.snapshot.json.zst";
const COMMIT: &str = "b1142bd4cb46dadfb6f5729f5e21025eca19f094";
const FILE: &str = "src/providers/registry.json";

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

fn envelope(registry: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "format": "tierbridge-snapshot/1",
        "sourceId": SOURCE_ID,
        "repository": "https://example.test/r",
        "commit": "0123456789abcdef0123456789abcdef01234567",
        "commitDate": "2026-11-26T07:34:11Z",
        "files": { FILE: registry },
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
fn fixture_figures_and_schema() {
    let a = fixture();
    assert_eq!(a.providers, 77);
    assert_eq!(a.observations.len(), 511);
    assert_eq!(a.source.source_id, SOURCE_ID);
    assert_eq!(
        a.source.revision.as_deref(),
        Some(format!("{COMMIT}@2026-11-26T07:34:11Z").as_str())
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
    // Only Google ADC providers (vertex 5 + vertex-partner 4 models) leave the auth enum.
    assert_eq!(a.findings.rejections.len(), 9);
    assert!(
        a.findings
            .rejections
            .iter()
            .all(|d| d.field == "authType" && d.value == "adc")
    );
    assert!(a.findings.sentinels.is_empty());
}

#[test]
fn upstream_tag_rides_along_in_raw() {
    let a = fixture();
    let mut by_tag: BTreeMap<String, usize> = BTreeMap::new();
    for o in &a.observations {
        let raw = o.raw.as_ref().unwrap();
        *by_tag
            .entry(raw["registrySource"].as_str().unwrap().to_owned())
            .or_default() += 1;
        // Model facts only: no endpoints, credential names, notes or websites.
        for key in raw.keys() {
            assert!(
                [
                    "registrySource",
                    "registrySourceUrl",
                    "category",
                    "format",
                    "local",
                    "authKind"
                ]
                .contains(&key.as_str()),
                "unexpected raw key {key}"
            );
        }
    }
    assert_eq!(
        by_tag,
        [
            ("9router", 113),
            ("custom", 91),
            ("models.dev", 278),
            ("official", 29)
        ]
        .map(|(k, v)| (k.to_owned(), v))
        .into()
    );
}

#[test]
fn reasoning_vision_and_limits() {
    let a = fixture();
    let v = row(&a, "kilo", "kilo-auto/free");
    assert_eq!(v["name"], "Auto Free");
    assert_eq!(v["contextWindow"], 256_000);
    assert_eq!(v["reasoning"], true);
    assert_eq!(
        v["thinking"]["mode"], "unknown",
        "the registry says whether, not how"
    );
    assert_eq!(v["input"], json!(["text"]), "vision false: text only");
    assert_eq!(v["authType"], "api_key");
    assert_eq!(v["raw"]["category"], "free-tier");
    assert_eq!(v["raw"]["registrySource"], "models.dev");

    let v = row(&a, "kilo", "nvidia/nemotron-3.5-super-vl:free");
    assert_eq!(v["input"], json!(["image", "text"]));

    // Unknown stays unknown: no reasoning, vision or context published.
    let v = row(&a, "baidu", "deepseek-v4-pro");
    assert_eq!(v["contextWindow"], 1_048_576);
    assert!(v["reasoning"].is_null() && v["thinking"].is_null() && v["input"].is_null());
    assert_eq!(v["raw"]["registrySource"], "9router");
    assert!(v["cost"].is_null() && v["lifecycle"].is_null());
}

#[test]
fn adc_auth_is_rejected_per_model() {
    let a = fixture();
    let v = row(&a, "vertex", "gemini-2.5-flash");
    assert!(v["authType"].is_null());
    assert_eq!(v["raw"]["authKind"], "adc", "kept verbatim in raw");
    let rejected: Vec<_> = a
        .findings
        .rejections
        .iter()
        .filter(|d| d.provider == "vertex")
        .collect();
    assert_eq!(rejected.len(), 5);
}

#[test]
fn out_of_contract_values_are_rejected_per_row() {
    let bytes = envelope(json!([{
        "id": "p", "auth": {"kind": 7},
        "models": [{"id": "m", "name": 3, "contextWindow": 0, "reasoning": "yes", "vision": 1}],
    }]));
    let a = adapt(&bytes, 0).unwrap();
    let fields: Vec<&str> = a
        .findings
        .rejections
        .iter()
        .map(|d| d.field.as_str())
        .collect();
    for expected in ["name", "contextWindow", "reasoning", "input", "authType"] {
        assert!(fields.contains(&expected), "{expected} in {fields:?}");
    }
    let v = serde_json::to_value(&a.observations[0]).unwrap();
    assert!(v["name"].is_null() && v["contextWindow"].is_null() && v["authType"].is_null());
    assert!(schema_validator().is_valid(&v));
}

#[test]
fn shape_failures_are_structural_errors() {
    assert!(matches!(
        adapt(b"not json", 0).unwrap_err(),
        CatalogError::InvalidJson(_)
    ));
    assert!(malformed(b"[]").contains("not a JSON object"));
    // The registry file is missing or is not a list.
    let no_file = serde_json::to_vec(&json!({
        "format": "tierbridge-snapshot/1", "sourceId": SOURCE_ID, "repository": "r",
        "commit": "0123456789abcdef0123456789abcdef01234567",
        "commitDate": "2026-11-26T07:34:11Z", "files": {},
    }))
    .unwrap();
    assert!(malformed(&no_file).contains(FILE));
    assert!(malformed(&envelope(json!({"p": []}))).contains("not an array"));
    assert!(malformed(&envelope(json!([1]))).contains("not an object"));
    assert!(malformed(&envelope(json!([{"models": []}]))).contains("`id`"));
    assert!(malformed(&envelope(json!([{"id": "p"}]))).contains("`models`"));
    assert!(malformed(&envelope(json!([{"id": "p", "models": {}}]))).contains("not an array"));
    assert!(malformed(&envelope(json!([{"id": "p", "models": [3]}]))).contains("not an object"));
    assert!(malformed(&envelope(json!([{"id": "p", "models": [{"name": "x"}]}]))).contains("`id`"));
    assert!(
        malformed(&envelope(
            json!([{"id": "p", "models": [{"id": "a"}, {"id": "a"}]}])
        ))
        .contains("more than once")
    );
    assert!(
        malformed(&envelope(
            json!([{"id": "p", "models": []}, {"id": "p", "models": []}])
        ))
        .contains("listed twice")
    );
    assert!(matches!(
        adapt(&envelope(json!([{"id": "p", "models": []}])), 0).unwrap_err(),
        CatalogError::NoModels
    ));
}

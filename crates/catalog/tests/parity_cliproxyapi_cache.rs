//! Parity with the Python prototype: `prototype/out/obs.cliproxyapi-cache.jsonl` (1 704 rows)
//! was produced from the committed snapshot of `~/.omp/agent/cliproxyapi-models.json`; the Rust
//! adapter must reproduce every row (null-normalisation: see `gateway_common::normalise`).
//!
//! Expected difference in the findings: the prototype dropped `ultra` silently. The 1 683 rows
//! where it is `null` (level not accepted) say nothing the contract can hold and stay unreported;
//! the 8 rows where the route accepts `ultra` are rejections.

mod common;
mod gateway_common;

use gateway_common::{SNAPSHOT_DAY, differences, jsonl_fixture, keyed, rust_rows};
use serde_json::json;
use tierbridge_catalog::cliproxyapi_cache::adapt;

fn snapshot() -> Vec<u8> {
    common::zstd_fixture(&format!("cliproxyapi-models.{SNAPSHOT_DAY}.json.zst"))
}

fn golden() -> Vec<serde_json::Value> {
    jsonl_fixture(&format!("obs.cliproxyapi-cache.{SNAPSHOT_DAY}.jsonl.zst"))
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn rust_observations_equal_prototype_rows() {
    let adapted = adapt(&snapshot()).unwrap();
    let expected = keyed(golden());
    assert_eq!(expected.len(), 1704, "golden fixture row count");
    assert_eq!(adapted.observations.len(), 1704);
    let actual = rust_rows(&adapted.observations);
    assert_eq!(actual.len(), 1704, "duplicate ids");

    let diffs = differences(&actual, &expected);
    assert!(
        diffs.is_empty(),
        "{} rows differ from the prototype, first:\n{}",
        diffs.len(),
        diffs[0]
    );
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn the_snapshot_header_becomes_the_source_descriptor() {
    let adapted = adapt(&snapshot()).unwrap();
    assert_eq!(
        serde_json::to_value(&adapted.source).unwrap(),
        json!({
            "sourceId": "cliproxyapi-cache",
            "kind": "discovered",
            "origin": "http://127.0.0.1:8317/v1/models?client_version=pi",
            "revision": "fetchedAt=1795670401936",
            "confidence": "inferred",
            "observedAtMs": 1_795_670_401_936_i64,
        })
    );
    assert_eq!(adapted.providers, 1);
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn the_findings_are_the_ultra_levels_the_route_accepts() {
    let adapted = adapt(&snapshot()).unwrap();
    let f = &adapted.findings;
    assert!(f.sentinels.is_empty());
    assert_eq!(f.zero_limits, 0);
    assert_eq!(f.rejections.len(), 8);
    for d in &f.rejections {
        assert_eq!(d.source_id, "cliproxyapi-cache");
        assert_eq!(d.provider, "cliproxyapi");
        assert_eq!(d.field, "thinking.effortMap.ultra");
        assert_eq!(d.value, json!("ultra"));
    }
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn gateway_zero_prices_are_unknown_never_free() {
    let adapted = adapt(&snapshot()).unwrap();
    let status = |id: &str| {
        let o = adapted.observations.iter().find(|o| o.fields.id == id);
        serde_json::to_value(o.unwrap().fields.cost.as_ref().unwrap()).unwrap()["status"].clone()
    };
    let unpriced = adapted
        .observations
        .iter()
        .filter(|o| serde_json::to_value(&o.fields.cost).unwrap()["status"] == json!("unknown"))
        .count();
    assert!(
        unpriced > 0,
        "the cache fills 0 where the gateway has no price"
    );
    assert_eq!(status("gpt-5.5"), json!("known"));
    let free = adapted
        .observations
        .iter()
        .filter(|o| serde_json::to_value(&o.fields.cost).unwrap()["status"] == json!("free"))
        .count();
    assert_eq!(free, 0, "a gateway never proves a model is free");
}

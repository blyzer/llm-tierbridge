//! Parity with the Python prototype: `prototype/out/obs.models.dev.jsonl` (10 049 rows) was produced
//! from the committed `api.json` snapshot; the Rust adapter must reproduce every row.
//!
//! Expected differences: none in the observations. The prototype dropped three kinds of value
//! silently; Rust reports them and ends with the *same* record:
//!   * effort `default` (groq qwen/qwen3.8-27b, qwen/qwen3.6-27b)  -> 2 rejections,
//!   * `budget_tokens.min == -1` (nvidia nemotron x2)               -> 2 mapped sentinels,
//!   * limits published as 0                                        -> counted, mapped to unknown.

mod common;

use std::collections::BTreeMap;

use serde_json::{Value, json};
use tierbridge_catalog::models_dev;

fn key(row: &Value) -> (String, String) {
    (
        row["provider"].as_str().unwrap().to_owned(),
        row["id"].as_str().unwrap().to_owned(),
    )
}

#[test]
fn rust_observations_equal_prototype_rows() {
    let adapted = models_dev::adapt(&common::snapshot_api(), common::fetched_at_ms()).unwrap();
    let expected: BTreeMap<_, _> = common::prototype_rows()
        .into_iter()
        .map(|r| (key(&r), r))
        .collect();
    assert_eq!(expected.len(), 10_049, "golden fixture row count");
    assert_eq!(adapted.observations.len(), 10_049);

    let actual: BTreeMap<_, _> = adapted
        .observations
        .iter()
        .map(|o| {
            let v = serde_json::to_value(o).unwrap();
            (key(&v), v)
        })
        .collect();
    assert_eq!(
        actual.len(),
        10_049,
        "duplicate (provider,id) in Rust output"
    );
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>(),
        "different (provider,id) sets"
    );

    let differing: Vec<_> = actual
        .iter()
        .filter(|(k, v)| !common::json_eq(v, &expected[*k]))
        .map(|(k, _)| k)
        .collect();
    assert!(
        differing.is_empty(),
        "{} rows differ from the prototype, first: {:?}\nrust:  {}\nproto: {}",
        differing.len(),
        differing[0],
        actual[differing[0]],
        expected[differing[0]]
    );
}

#[test]
fn rust_now_reports_what_the_prototype_dropped_silently() {
    let adapted = models_dev::adapt(&common::snapshot_api(), common::fetched_at_ms()).unwrap();
    let f = &adapted.findings;

    let mut rejected: Vec<_> = f
        .rejections
        .iter()
        .map(|d| {
            (
                d.provider.as_str(),
                d.id.as_str(),
                d.field.as_str(),
                d.value.clone(),
            )
        })
        .collect();
    rejected.sort_by(|a, b| a.1.cmp(b.1));
    assert_eq!(
        rejected,
        vec![
            (
                "groq",
                "qwen/qwen3.6-27b",
                "thinking.efforts",
                json!("default")
            ),
            (
                "groq",
                "qwen/qwen3.8-27b",
                "thinking.efforts",
                json!("default")
            ),
        ]
    );

    let is_limit = |field: &str| matches!(field, "contextWindow" | "maxInputTokens" | "maxTokens");
    let mut documented: Vec<_> = f
        .sentinels
        .iter()
        .filter(|s| !is_limit(&s.field))
        .map(|s| {
            (
                s.provider.as_str(),
                s.id.as_str(),
                s.field.as_str(),
                s.value.clone(),
            )
        })
        .collect();
    documented.sort_by(|a, b| a.1.cmp(b.1));
    assert_eq!(
        documented,
        vec![
            (
                "nvidia",
                "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning",
                "thinking.minBudgetTokens",
                json!(-1)
            ),
            (
                "nvidia",
                "nvidia/nemotron-3-ultra-550b-a55b",
                "thinking.minBudgetTokens",
                json!(-1)
            ),
        ]
    );

    // 155 context + 4 input + 263 output limits are published as 0 in this snapshot; each one is a
    // per-row sentinel mapped to unknown, never a rejection.
    let zeros: Vec<_> = f.sentinels.iter().filter(|s| is_limit(&s.field)).collect();
    assert_eq!(zeros.len(), 155 + 4 + 263);
    assert_eq!(f.zero_limits, zeros.len());
    for (field, expected) in [
        ("contextWindow", 155),
        ("maxInputTokens", 4),
        ("maxTokens", 263),
    ] {
        let n = zeros.iter().filter(|s| s.field == field).count();
        assert_eq!(n, expected, "{field}");
    }
    assert!(zeros.iter().all(|s| s.value == json!(0)
        && s.mapped_to == "unknown"
        && s.reason == "models.dev publishes 0 when no token limit applies"));
    assert!(!f.rejections.iter().any(|d| is_limit(&d.field)));
}

#[test]
fn rejected_efforts_leave_the_remaining_efforts_in_place() {
    let adapted = models_dev::adapt(&common::snapshot_api(), common::fetched_at_ms()).unwrap();
    let find = |id: &str| {
        let o = adapted
            .observations
            .iter()
            .find(|o| o.fields.provider == "groq" && o.fields.id == id)
            .unwrap();
        serde_json::to_value(o.fields.thinking.as_ref().unwrap()).unwrap()
    };
    assert_eq!(
        find("qwen/qwen3.8-27b")["efforts"],
        json!(["off", "low", "medium", "high"])
    );
    // only `none` + `default`: after the rejection a single effort (`off`) remains.
    assert_eq!(find("qwen/qwen3.6-27b")["efforts"], json!(["off"]));
}

#[test]
fn source_descriptor_matches_the_prototype() {
    let adapted = models_dev::adapt(&common::snapshot_api(), common::fetched_at_ms()).unwrap();
    assert_eq!(
        serde_json::to_value(&adapted.source).unwrap(),
        json!({
            "sourceId": "models.dev",
            "kind": "bundled",
            "origin": "https://models.dev/api.json#sha256=0a93fa52761a70bc",
            "revision": "2026-11-26T08:32:56Z",
            "confidence": "declared",
            "observedAtMs": 1_795_681_976_000_i64
        })
    );
    assert!(adapted.sha256.starts_with("0a93fa52761a70bc"));
}

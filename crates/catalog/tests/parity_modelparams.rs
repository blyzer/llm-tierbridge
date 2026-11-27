//! Parity with the Python prototype: `prototype/out/obs.modelparams.dev.jsonl` (510 rows) was
//! produced from the committed `models.json` snapshot; the Rust adapter must reproduce every row.
//!
//! The only normalisation: a key whose value is `null` is treated as absent at the top level of a
//! row, inside `thinking` and inside each `acceptedParams` entry. The schema says "unknown =
//! null": Rust writes the always-unknown shared fields (`cost`, `contextWindow`, `input`, ...) as
//! `null` where this prototype source omits them, and omits optional members (`values`, `range`,
//! `default`, the budget options of `thinking`) that the prototype writes as `null`. A null on
//! one side never matches a value on the other.
//!
//! Expected differences in the findings (the prototype dropped these silently):
//!   * effort `default` (groq, 5 models)                       -> 5 rejections,
//!   * `-1` documented in the parameter's own description
//!     (Gemini `thinkingBudget`, NVIDIA `reasoning_budget`)    -> 10 mapped sentinels.
//!
//! Not in this source (despite the design note): the -1 000 000 price and the effort `ultra`.

mod common;

use std::collections::BTreeMap;

use serde_json::{Value, json};
use tierbridge_catalog::modelparams;

type Key = (String, String, String);

fn key(row: &Value) -> Key {
    let text = |v: &Value| v.as_str().unwrap().to_owned();
    (
        text(&row["provider"]),
        text(&row["id"]),
        text(&row["authType"]),
    )
}

fn drop_nulls(v: &mut Value) {
    if let Value::Object(o) = v {
        o.retain(|_, x| !x.is_null());
    }
}

fn normalise(mut row: Value) -> Value {
    drop_nulls(&mut row);
    drop_nulls(&mut row["thinking"]);
    if let Some(params) = row["acceptedParams"].as_array_mut() {
        params.iter_mut().for_each(drop_nulls);
    }
    row
}

#[test]
fn rust_observations_equal_prototype_rows() {
    let adapted =
        modelparams::adapt(&common::modelparams_snapshot(), common::fetched_at_ms()).unwrap();
    let expected: BTreeMap<Key, Value> = common::modelparams_prototype_rows()
        .into_iter()
        .map(|r| (key(&r), normalise(r)))
        .collect();
    assert_eq!(expected.len(), 510, "golden fixture row count");
    assert_eq!(adapted.observations.len(), 510);

    let actual: BTreeMap<Key, Value> = adapted
        .observations
        .iter()
        .map(|o| {
            let v = serde_json::to_value(o).unwrap();
            (key(&v), normalise(v))
        })
        .collect();
    assert_eq!(actual.len(), 510, "duplicate (provider,id,authType)");
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>(),
        "different (provider,id,authType) sets"
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
fn the_snapshot_header_becomes_the_source_descriptor() {
    let adapted =
        modelparams::adapt(&common::modelparams_snapshot(), common::fetched_at_ms()).unwrap();
    let s = serde_json::to_value(&adapted.source).unwrap();
    assert_eq!(
        s,
        json!({
            "sourceId": "modelparams.dev",
            "kind": "bundled",
            "origin": "https://modelparams.dev/api/v1/models.json#sha256=46b4ffc76a3cf3f8",
            "revision": "2026-11-24T08:22:46.486Z",
            "confidence": "declared",
            "observedAtMs": 1_795_681_976_000_i64,
        })
    );
    assert_eq!(adapted.providers, 22);
}

#[test]
fn the_findings_are_exactly_the_values_the_prototype_dropped_or_passed_unflagged() {
    let adapted =
        modelparams::adapt(&common::modelparams_snapshot(), common::fetched_at_ms()).unwrap();
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
    rejected.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
    let groq = |id| ("groq", id, "thinking.efforts", json!("default"));
    assert_eq!(
        rejected,
        vec![
            groq("allam-2-7b"),
            groq("gpt-oss-safeguard-20b"),
            groq("qwen3-32b"),
            groq("qwen3.6-27b"),
            groq("qwen3.8-27b"),
        ]
    );

    let mut sentinels: Vec<_> = f
        .sentinels
        .iter()
        .map(|s| {
            (
                s.provider.as_str(),
                s.id.as_str(),
                s.field.as_str(),
                s.value.clone(),
            )
        })
        .collect();
    sentinels.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
    let budget = "acceptedParams[generationConfig.thinkingConfig.thinkingBudget]";
    let nvidia = "acceptedParams[reasoning_budget].range.min";
    let expected = [
        ("google", "gemini-2.5-flash", format!("{budget}.default")),
        ("google", "gemini-2.5-flash", format!("{budget}.default")),
        ("google", "gemini-2.5-flash", format!("{budget}.range.min")),
        ("google", "gemini-2.5-flash", format!("{budget}.range.min")),
        ("nvidia", "nemotron-3-super-120b-a12b", nvidia.to_owned()),
        ("nvidia", "nemotron-3-ultra", nvidia.to_owned()),
        ("nvidia", "nemotron-3-ultra-550b-a55b", nvidia.to_owned()),
        ("nvidia", "nemotron-3.5-bolt-e4b", nvidia.to_owned()),
        (
            "nvidia",
            "nemotron-3.5-lightning-30b-a3b",
            nvidia.to_owned(),
        ),
        (
            "nvidia",
            "nemotron-3.5-super-vl-120b-a12b",
            nvidia.to_owned(),
        ),
    ];
    assert_eq!(sentinels.len(), expected.len());
    for ((p, i, field, value), (ep, ei, ef)) in sentinels.iter().zip(&expected) {
        assert_eq!((p, i, field), (ep, ei, &ef.as_str()));
        assert_eq!(value, &json!(-1));
    }
    assert!(
        f.sentinels
            .iter()
            .all(|s| s.mapped_to == "kept as the wire value")
    );
    assert!(f.sentinels.iter().all(|s| s.reason.contains("-1")));
    assert_eq!(f.zero_limits, 0);
}

#[test]
fn no_price_and_no_ultra_effort_in_this_source() {
    // The design's three broken values: only the budget -1 (documented) and an out-of-enum effort
    // (`default`, not `ultra`) are in modelparams.dev. The -1 000 000 price is omp-bundled's.
    let root: Value = serde_json::from_slice(&common::modelparams_snapshot()).unwrap();
    for model in root["models"].as_array().unwrap() {
        let keys: Vec<_> = model.as_object().unwrap().keys().collect();
        assert!(
            keys.iter()
                .all(|k| !k.contains("cost") && !k.contains("price")),
            "modelparams.dev has no prices: {keys:?}"
        );
        for param in model["params"].as_array().unwrap() {
            for v in param["values"].as_array().into_iter().flatten() {
                assert_ne!(v, "ultra", "{model}");
            }
        }
    }
}

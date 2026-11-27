//! Parity with the Python prototype: `prototype/out/obs.omp-bundled.jsonl` (5 369 rows) was
//! produced from the committed copy of oh-my-pi's decompressed model roster; the Rust adapter
//! must reproduce every row (null-normalisation: see `gateway_common::normalise`) except the
//! 2 `openrouter/openrouter/auto*` rows, where the prototype reported `free` with `input = output = 0`
//! for a price of `-1000000`; here those two prices are rejections and the cost is unknown.

mod common;
mod gateway_common;

use gateway_common::{SNAPSHOT_DAY, differences, jsonl_fixture, keyed, rust_rows};
use serde_json::json;
use tierbridge_catalog::omp_bundled::adapt;

/// mtime of the snapshot file when the prototype ran.
const OBSERVED_AT_MS: i64 = 1_795_681_938_917;

fn snapshot() -> Vec<u8> {
    common::zstd_fixture(&format!("omp-oracle-models.{SNAPSHOT_DAY}.json.zst"))
}

#[test]
fn rust_observations_equal_prototype_rows_except_the_variable_price_router() {
    let adapted = adapt(&snapshot(), OBSERVED_AT_MS).unwrap();
    let mut expected = keyed(jsonl_fixture(&format!(
        "obs.omp-bundled.{SNAPSHOT_DAY}.jsonl.zst"
    )));
    assert_eq!(expected.len(), 5369, "golden fixture row count");
    assert_eq!(adapted.observations.len(), 5369);
    let actual = rust_rows(&adapted.observations);

    let unknown = json!({"status": "unknown", "input": null, "output": null, "cacheRead": null, "cacheWrite": null});
    let routers: Vec<_> = expected
        .keys()
        .filter(|(p, id)| p == "openrouter" && id.starts_with("openrouter/auto"))
        .cloned()
        .collect();
    assert_eq!(routers.len(), 2);
    for k in routers {
        let row = expected.get_mut(&k).unwrap();
        assert_eq!(row["cost"]["status"], "free", "prototype artefact");
        row["cost"] = unknown.clone();
    }
    let diffs = differences(&actual, &expected);
    assert!(
        diffs.is_empty(),
        "{} rows differ from the prototype, first:\n{}",
        diffs.len(),
        diffs[0]
    );
}

#[test]
fn the_variable_price_is_a_per_row_rejection_not_a_sentinel() {
    let adapted = adapt(&snapshot(), OBSERVED_AT_MS).unwrap();
    let f = &adapted.findings;
    assert!(f.sentinels.is_empty());
    assert_eq!(f.zero_limits, 0);
    let mut got: Vec<_> = f
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
    got.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
    let v = json!(-1_000_000);
    let mut want = Vec::new();
    for provider in ["openrouter"] {
        for id in ["openrouter/auto", "openrouter/auto-beta"] {
            for field in ["cost.input", "cost.output"] {
                want.push((provider, id, field, v.clone()));
            }
        }
    }
    assert_eq!(got, want);
}

#[test]
fn the_source_descriptor_is_the_bundled_roster() {
    let adapted = adapt(&snapshot(), OBSERVED_AT_MS).unwrap();
    assert_eq!(
        serde_json::to_value(&adapted.source).unwrap(),
        json!({
            "sourceId": "omp-bundled",
            "kind": "bundled",
            "origin": "fixtures/llm-oracle/catalog/models.json.zst",
            "revision": "oh-my-pi working tree",
            "confidence": "declared",
            "observedAtMs": OBSERVED_AT_MS,
        })
    );
    assert_eq!(adapted.providers, 75);
}

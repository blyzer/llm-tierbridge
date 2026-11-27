//! Parity with the Python prototype: `prototype/out/obs.omp-live_{easycliproxy,dsh}.jsonl`
//! (1 699 and 51 rows) were produced from the committed `omp models <provider> --json`
//! snapshots; the Rust adapter must reproduce every row (null-normalisation: see
//! `gateway_common::normalise`).
//!
//! The prototype stamped `observedAtMs` with the snapshot file's mtime; here it is the
//! `fetched_at_ms` argument, which the test takes from the golden rows.

mod common;
mod gateway_common;

use gateway_common::{SNAPSHOT_DAY, differences, jsonl_fixture, keyed, rust_rows};
use serde_json::json;
use tierbridge_catalog::omp_live::adapt;

struct Roster {
    provider: &'static str,
    rows: usize,
    /// mtime of the snapshot file when the prototype ran.
    observed_at_ms: i64,
}

const EASYCLIPROXY: Roster = Roster {
    provider: "easycliproxy",
    rows: 1699,
    observed_at_ms: 1_795_682_024_223,
};
const DSH: Roster = Roster {
    provider: "dsh",
    rows: 51,
    observed_at_ms: 1_795_682_025_014,
};

fn snapshot(r: &Roster) -> Vec<u8> {
    common::zstd_fixture(&format!(
        "omp-models-{}.{SNAPSHOT_DAY}.json.zst",
        r.provider
    ))
}

fn golden(r: &Roster) -> Vec<serde_json::Value> {
    jsonl_fixture(&format!(
        "obs.omp-live_{}.{SNAPSHOT_DAY}.jsonl.zst",
        r.provider
    ))
}

fn assert_parity(r: &Roster) {
    let adapted = adapt(&snapshot(r), r.observed_at_ms, r.provider).unwrap();
    let expected = keyed(golden(r));
    assert_eq!(expected.len(), r.rows, "golden fixture row count");
    assert_eq!(adapted.observations.len(), r.rows);
    let actual = rust_rows(&adapted.observations);
    assert_eq!(actual.len(), r.rows, "duplicate ids");
    let diffs = differences(&actual, &expected);
    assert!(
        diffs.is_empty(),
        "{}: {} rows differ from the prototype, first:\n{}",
        r.provider,
        diffs.len(),
        diffs[0]
    );
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn easycliproxy_equals_prototype_rows() {
    assert_parity(&EASYCLIPROXY);
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn dsh_equals_prototype_rows() {
    assert_parity(&DSH);
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn the_roster_header_becomes_the_source_descriptor() {
    let adapted = adapt(&snapshot(&DSH), DSH.observed_at_ms, "dsh").unwrap();
    assert_eq!(
        serde_json::to_value(&adapted.source).unwrap(),
        json!({
            "sourceId": "omp-live:dsh",
            "kind": "discovered",
            "origin": "omp models dsh --json --no-extensions",
            "revision": "omp 19.2.1",
            "confidence": "inferred",
            "observedAtMs": 1_795_682_025_014_i64,
        })
    );
    assert_eq!(adapted.providers, 1);
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn a_clean_roster_has_no_findings_and_no_free_model() {
    for r in [&EASYCLIPROXY, &DSH] {
        let adapted = adapt(&snapshot(r), r.observed_at_ms, r.provider).unwrap();
        assert!(adapted.findings.rejections.is_empty(), "{}", r.provider);
        assert!(adapted.findings.sentinels.is_empty(), "{}", r.provider);
        // every row is `pricingStatus: unknown` with stand-in zeros: cost unknown, never free
        let statuses: Vec<_> = adapted
            .observations
            .iter()
            .map(|o| serde_json::to_value(&o.fields.cost).unwrap()["status"].clone())
            .collect();
        assert!(statuses.iter().all(|s| s == "unknown"), "{}", r.provider);
    }
}

//! Helpers shared by the gateway-adapter tests. Include next to `mod common;`.
#![allow(dead_code)]

use std::collections::BTreeMap;

use serde_json::Value;
use tierbridge_catalog::model::Observation;

use crate::common;

/// Date the committed gateway snapshots were taken (the prototype's `fetch-date.txt` day).
pub const SNAPSHOT_DAY: &str = "2026-11-26";

pub fn jsonl_fixture(name: &str) -> Vec<Value> {
    common::zstd_fixture(name)
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_slice(l).unwrap())
        .collect()
}

/// A key whose value is `null` is treated as absent at the top level of a row and inside
/// `thinking`. The schema says "unknown = null": Rust writes the always-unknown shared fields
/// (`maxInputTokens`, `canonicalId`, ...) as `null` where the prototype omits them. A null on
/// one side never matches a value on the other, and nulls deeper down (`cost`, `effortMap`)
/// are compared as they are.
pub fn normalise(mut row: Value) -> Value {
    fn drop_nulls(v: &mut Value) {
        if let Value::Object(o) = v {
            o.retain(|_, x| !x.is_null());
        }
    }
    drop_nulls(&mut row);
    drop_nulls(&mut row["thinking"]);
    row
}

pub type Rows = BTreeMap<(String, String), Value>;

fn key(row: &Value) -> (String, String) {
    let text = |v: &Value| v.as_str().unwrap().to_owned();
    (text(&row["provider"]), text(&row["id"]))
}

pub fn keyed(rows: impl IntoIterator<Item = Value>) -> Rows {
    rows.into_iter().map(|r| (key(&r), normalise(r))).collect()
}

pub fn rust_rows(observations: &[Observation]) -> Rows {
    keyed(
        observations
            .iter()
            .map(|o| serde_json::to_value(o).unwrap()),
    )
}

/// Keys of the rows that differ (and the rows missing on either side) between Rust and the
/// prototype, with the first differing pair for the failure message.
pub fn differences(actual: &Rows, expected: &Rows) -> Vec<String> {
    let mut out = Vec::new();
    for k in expected.keys().filter(|k| !actual.contains_key(*k)) {
        out.push(format!("{k:?}: missing from Rust"));
    }
    for (k, v) in actual {
        match expected.get(k) {
            None => out.push(format!("{k:?}: not in the prototype")),
            Some(e) if !common::json_eq(v, e) => {
                out.push(format!("{k:?}\n  rust:  {v}\n  proto: {e}"));
            }
            Some(_) => {}
        }
    }
    out
}

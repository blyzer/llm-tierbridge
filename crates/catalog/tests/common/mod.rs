#![allow(dead_code)]

use std::path::PathBuf;

use serde_json::Value;
use tierbridge_catalog::time::parse_rfc3339_ms;

/// `prototype/raw/fetch-date.txt` of the committed snapshot.
pub const SNAPSHOT_FETCHED_AT: &str = "2026-11-26T08:32:56Z";

pub fn fetched_at_ms() -> i64 {
    parse_rfc3339_ms(SNAPSHOT_FETCHED_AT).unwrap()
}

/// Where the private rosters live (not published): `$TIERBRIDGE_LOCAL_FIXTURES`, else
/// `local-fixtures/` at the workspace root. Tests that need them are `#[ignore]`d and run with
/// `cargo test -- --include-ignored` (README, "Pruebas").
pub fn local_fixtures_dir() -> PathBuf {
    match std::env::var_os("TIERBRIDGE_LOCAL_FIXTURES") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../local-fixtures"),
    }
}

/// A published fixture (`tests/fixtures/`) or, failing that, a private one (`local-fixtures/`).
pub fn fixture_path(name: &str) -> PathBuf {
    let public = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    if public.exists() {
        public
    } else {
        local_fixtures_dir().join(name)
    }
}

pub fn zstd_fixture(name: &str) -> Vec<u8> {
    let path = fixture_path(name);
    let file = std::fs::File::open(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}\nprivate fixture: put it in local-fixtures/ or point TIERBRIDGE_LOCAL_FIXTURES at it (README, \"Pruebas\")",
            path.display()
        )
    });
    zstd::stream::decode_all(file).unwrap()
}

pub fn snapshot_api() -> Vec<u8> {
    zstd_fixture("models.dev.api.2026-11-26.json.zst")
}

pub fn prototype_rows() -> Vec<Value> {
    zstd_fixture("obs.models.dev.2026-11-26.jsonl.zst")
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_slice(l).unwrap())
        .collect()
}

pub fn modelparams_snapshot() -> Vec<u8> {
    zstd_fixture("modelparams.models.2026-11-26.json.zst")
}

pub fn modelparams_prototype_rows() -> Vec<Value> {
    zstd_fixture("obs.modelparams.dev.2026-11-26.jsonl.zst")
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_slice(l).unwrap())
        .collect()
}

pub fn schema_validator() -> jsonschema::Validator {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema/gateway-model.schema.json");
    let schema: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    jsonschema::draft202012::new(&schema).expect("schema compiles")
}

/// JSON equality with Python semantics for numbers (`3 == 3.0`), which is how the prototype's
/// rows were compared: the Rust side serialises prices as floats, the prototype keeps `int`s.
pub fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| json_eq(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| json_eq(v, w)))
        }
        _ => a == b,
    }
}

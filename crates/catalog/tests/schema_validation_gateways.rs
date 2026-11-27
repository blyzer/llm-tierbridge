//! Every observation and model the gateway adapters emit validates against
//! `schema/gateway-model.schema.json`.

mod common;
mod gateway_common;

use gateway_common::SNAPSHOT_DAY;
use tierbridge_catalog::{
    BuildOptions, BuildOutput, SourceInput, build_cliproxyapi_cache, build_omp_bundled,
    build_omp_live,
};

fn input(bytes: &[u8]) -> SourceInput<'_> {
    SourceInput {
        bytes,
        fetched_at_ms: common::fetched_at_ms(),
        fetch: None,
    }
}

fn assert_valid(out: &BuildOutput, expected: usize) {
    assert_eq!(out.observations.len(), expected);
    assert_eq!(out.models.len(), expected);
    let validator = common::schema_validator();
    let records = out
        .observations
        .iter()
        .map(|o| serde_json::to_value(o).unwrap())
        .chain(out.models.iter().map(|m| serde_json::to_value(m).unwrap()));
    let failures: Vec<String> = records
        .filter_map(|record| {
            let e = validator.iter_errors(&record).next()?;
            Some(format!("{} {}: {e}", record["provider"], record["id"]))
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} invalid records, first: {}",
        failures.len(),
        failures[0]
    );
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn cliproxyapi_cache_records_validate() {
    let bytes = common::zstd_fixture(&format!("cliproxyapi-models.{SNAPSHOT_DAY}.json.zst"));
    let out = build_cliproxyapi_cache(input(&bytes), &BuildOptions::default()).unwrap();
    assert_valid(&out, 1704);
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn omp_live_records_validate() {
    for (provider, rows) in [("easycliproxy", 1699), ("dsh", 51)] {
        let bytes = common::zstd_fixture(&format!("omp-models-{provider}.{SNAPSHOT_DAY}.json.zst"));
        let out = build_omp_live(provider, input(&bytes), &BuildOptions::default()).unwrap();
        assert_valid(&out, rows);
    }
}

#[test]
fn omp_bundled_records_validate() {
    let bytes = common::zstd_fixture(&format!("omp-oracle-models.{SNAPSHOT_DAY}.json.zst"));
    let out = build_omp_bundled(input(&bytes), &BuildOptions::default()).unwrap();
    assert_valid(&out, 5369);
}

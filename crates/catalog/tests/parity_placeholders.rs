//! Placeholder detection finds the same placeholders as the prototype did on the same snapshots
//! (`prototype/out/results.json`, `placeholders`).

mod common;
mod gateway_common;

use std::collections::BTreeSet;

use gateway_common::SNAPSHOT_DAY;
use serde_json::Value;
use tierbridge_catalog::model::Effort;
use tierbridge_catalog::placeholder::Placeholders;
use tierbridge_catalog::{
    BuildOptions, BuildOutput, SourceInput, build_cliproxyapi_cache, build_omp_live,
};

fn input(bytes: &[u8]) -> SourceInput<'_> {
    SourceInput {
        bytes,
        fetched_at_ms: common::fetched_at_ms(),
        fetch: None,
    }
}

fn prototype() -> Value {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../prototype/out/results.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn detect(out: &BuildOutput) -> Placeholders {
    Placeholders::detect(out.observations.iter().map(|o| &o.fields))
}

fn expected_pairs(v: &Value) -> BTreeSet<(Option<u64>, Option<u64>)> {
    v["ctxMaxPairs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p[0].as_u64(), p[1].as_u64()))
        .collect()
}

fn expected_efforts(v: &Value) -> BTreeSet<Vec<Effort>> {
    v["efforts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            l.as_array()
                .unwrap()
                .iter()
                .map(|e| Effort::parse(e.as_str().unwrap()).unwrap())
                .collect()
        })
        .collect()
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn placeholders_match_the_prototype_for_every_gateway() {
    let proto = prototype();
    let proto = &proto["placeholders"];

    let bytes = common::zstd_fixture(&format!("cliproxyapi-models.{SNAPSHOT_DAY}.json.zst"));
    let cache = build_cliproxyapi_cache(input(&bytes), &BuildOptions::default()).unwrap();
    let mut found = vec![("cliproxyapi-cache", detect(&cache))];
    for (provider, source) in [
        ("easycliproxy", "omp-live:easycliproxy"),
        ("dsh", "omp-live:dsh"),
    ] {
        let bytes = common::zstd_fixture(&format!("omp-models-{provider}.{SNAPSHOT_DAY}.json.zst"));
        let out = build_omp_live(provider, input(&bytes), &BuildOptions::default()).unwrap();
        found.push((source, detect(&out)));
    }

    for (source, ph) in found {
        assert_eq!(
            ph.pairs,
            expected_pairs(&proto[source]),
            "{source}: (ctx, max) pairs"
        );
        assert_eq!(
            ph.efforts,
            expected_efforts(&proto[source]),
            "{source}: effort ladders"
        );
    }
}

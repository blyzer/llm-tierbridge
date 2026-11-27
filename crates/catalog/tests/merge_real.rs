//! The merge over the committed snapshots (every source): schema validity, the acceptance
//! invariants, and the figures that must agree with the prototype's `results.json`.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use serde_json::Value;
use tierbridge_catalog::model::{CostStatus, FieldName, LifecycleStatus};
use tierbridge_catalog::{
    BuildOptions, BuildOutput, MergeOutput, Source, SourceInput, merge_catalog,
};

const FIXTURES: [(Source, &str); 9] = [
    (Source::ModelsDev, "models.dev.api.2026-11-26.json.zst"),
    (
        Source::ModelparamsDev,
        "modelparams.models.2026-11-26.json.zst",
    ),
    (
        Source::CliproxyapiCache,
        "cliproxyapi-models.2026-11-26.json.zst",
    ),
    (
        Source::OmpLiveEasycliproxy,
        "omp-models-easycliproxy.2026-11-26.json.zst",
    ),
    (Source::OmpLiveDsh, "omp-models-dsh.2026-11-26.json.zst"),
    (Source::OmpBundled, "omp-oracle-models.2026-11-26.json.zst"),
    (
        Source::RouterModels,
        "router-for-me.models.d02fed832.snapshot.json.zst",
    ),
    (Source::Dsh, "dsh.b1142bd.snapshot.json.zst"),
    (Source::Freellmapi, "freellmapi.0d64e07.snapshot.json.zst"),
];

fn build_all() -> Vec<BuildOutput> {
    FIXTURES
        .iter()
        .map(|(source, file)| {
            let bytes = common::zstd_fixture(file);
            source
                .build(
                    SourceInput {
                        bytes: &bytes,
                        fetched_at_ms: common::fetched_at_ms(),
                        fetch: None,
                    },
                    &BuildOptions::default(),
                )
                .unwrap_or_else(|e| panic!("{}: {e}", source.id()))
        })
        .collect()
}

static BUILDS: LazyLock<Vec<BuildOutput>> = LazyLock::new(build_all);
static MERGED: LazyLock<MergeOutput> = LazyLock::new(|| {
    let outs: Vec<&BuildOutput> = BUILDS.iter().collect();
    merge_catalog(&outs).unwrap()
});

fn builds() -> &'static Vec<BuildOutput> {
    &BUILDS
}

fn merged() -> &'static MergeOutput {
    &MERGED
}

fn prototype() -> Value {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../prototype/out/results.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn every_merged_model_validates_against_the_schema() {
    let validator = common::schema_validator();
    let failures: Vec<String> = merged()
        .models
        .iter()
        .filter_map(|m| {
            let v = serde_json::to_value(m).unwrap();
            let e = validator.iter_errors(&v).next()?;
            Some(format!("{}: {e}", m.key))
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} invalid, first: {}",
        failures.len(),
        failures[0]
    );
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn one_unique_model_per_live_gateway_row_and_retired_rows_create_none() {
    let m = merged();
    let keys: BTreeSet<&str> = m.models.iter().map(|x| x.key.as_str()).collect();
    assert_eq!(keys.len(), m.models.len(), "keys are unique");
    let rows: usize = m.report.gateways.iter().map(|g| g.rows).sum();
    assert_eq!(rows, m.models.len());
    let router = m
        .report
        .gateways
        .iter()
        .find(|g| g.source_id == "router-for-me.models")
        .unwrap();
    assert_eq!(router.retired_skipped, 248);
    assert_eq!(router.rows, 272);
    // Only a reference (modelparams.dev) can retire a model; a gateway channel dropping an
    // entry is not a vendor retirement and must not leak into the other gateways' rows.
    for model in &m.models {
        let life = model.fields.lifecycle.as_ref().unwrap();
        if life.status == LifecycleStatus::Retired {
            let from = &model.provenance.fields[&FieldName::Lifecycle].source;
            assert_eq!(from, "modelparams.dev", "{}", model.key);
        }
    }
    // omp-bundled is a probe: it never creates rows and never appears as a source
    assert!(m.models.iter().all(|x| {
        x.provenance
            .sources
            .iter()
            .all(|s| s.source_id != "omp-bundled")
    }));
    assert!(m.models.iter().all(|x| !x.key.starts_with("omp-bundled")));
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn acceptance_no_placeholder_winner_and_no_gateway_zero_cost() {
    let r = &merged().report;
    assert_eq!(
        r.placeholder_winners.total, 0,
        "{:?}",
        r.placeholder_winners
    );
    assert_eq!(r.cost.gateway_zero_winners, 0);
    for model in &merged().models {
        let cost = model.fields.cost.as_ref().expect("cost is always explicit");
        let from = &model.provenance.fields[&FieldName::Cost];
        if cost.status == CostStatus::Unknown {
            assert!(
                cost.input.is_none() && cost.output.is_none(),
                "{}",
                model.key
            );
            assert_eq!(from.source, "none");
        }
        let gateway = model
            .key
            .split_once(":")
            .map(|_| from.source == model.provenance.sources[0].source_id);
        if gateway == Some(true) {
            assert_eq!(
                cost.status,
                CostStatus::Known,
                "{}: gateway cost must be known",
                model.key
            );
        }
    }
}

/// The prototype counted, per gateway, the rows matched in models.dev whose own
/// `(contextWindow, maxTokens)` is a placeholder (`gw_placeholder`). The merge must find the
/// same rows through its own matching and detection.
#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn placeholder_rows_agree_with_the_prototype() {
    let proto = prototype();
    for gw in ["cliproxyapi-cache", "omp-live:easycliproxy", "omp-live:dsh"] {
        let report = merged()
            .report
            .gateways
            .iter()
            .find(|g| g.source_id == gw)
            .unwrap();
        let obs = &builds()
            .iter()
            .find(|b| b.report.source.descriptor.source_id == gw)
            .unwrap()
            .observations;
        let pairs: BTreeSet<_> = report.placeholders.pairs.iter().copied().collect();
        let by_id: BTreeMap<&str, _> = obs.iter().map(|o| (o.fields.id.as_str(), o)).collect();
        for (field, name) in [
            (FieldName::ContextWindow, "contextWindow"),
            (FieldName::MaxTokens, "maxTokens"),
        ] {
            let n = merged()
                .models
                .iter()
                .filter(|m| m.key.starts_with(&format!("{gw}:")))
                .filter(|m| m.references.iter().any(|r| r.source_id == "models.dev"))
                .filter(|m| {
                    let o = by_id[m.fields.id.as_str()];
                    let own = match field {
                        FieldName::ContextWindow => o.fields.context_window,
                        _ => o.fields.max_tokens,
                    };
                    own.is_some() && pairs.contains(&(o.fields.context_window, o.fields.max_tokens))
                })
                .count();
            let expected =
                proto["fields"][format!("{gw} vs models.dev :: {name}")]["gw_placeholder"]
                    .as_u64()
                    .unwrap() as usize;
            assert_eq!(
                n, expected,
                "{gw} {name}: rows matched in models.dev with a placeholder value"
            );
        }
    }
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn matching_figures_agree_with_the_prototype() {
    let proto = prototype();
    for gw in ["cliproxyapi-cache", "omp-live:easycliproxy", "omp-live:dsh"] {
        let g = merged()
            .report
            .gateways
            .iter()
            .find(|g| g.source_id == gw)
            .unwrap();
        let p = &proto["match"][gw];
        let tiers = &g.per_reference["models.dev"];
        assert_eq!(
            tiers.t1 as u64,
            p["models.dev"]["T1-exact-full"].as_u64().unwrap_or(0),
            "{gw} T1"
        );
        assert_eq!(
            tiers.unmatched as u64,
            p["models.dev"]["unmatched"].as_u64().unwrap_or(0),
            "{gw} unmatched"
        );
        assert_eq!(
            g.rows as u64,
            proto["counts"][gw].as_u64().unwrap(),
            "{gw} rows"
        );
    }
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn max_tokens_study_is_consistent() {
    let s = merged()
        .report
        .max_tokens_study
        .as_ref()
        .expect("both references present");
    assert_eq!(
        s.equal + s.range_max_greater + s.range_max_lower,
        s.comparable
    );
    assert_eq!(
        s.no_models_dev_match + s.models_dev_has_no_output + s.models_dev_ambiguous + s.comparable,
        s.modelparams_with_max_tokens
    );
    assert!(s.comparable > 0);
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn unmodelled_prices_and_toggles_are_reported_not_dropped() {
    let r = &merged().report;
    let kinds = |k: &str| r.diagnostics.iter().filter(|d| d.kind == k).count();
    // models.dev has 343 entries with cost.reasoning / audio prices (195 + 185 + 16 values);
    // the report lists each value once per merged row whose models.dev price won
    assert!(kinds("unmodelled") > 0);
    assert!(
        r.diagnostics
            .iter()
            .filter(|d| d.kind == "unmodelled")
            .all(|d| {
                d.source_id == "models.dev" && d.key.is_some() && d.field.starts_with("cost.")
            })
    );
    let md = builds()
        .iter()
        .find(|b| b.report.source.descriptor.source_id == "models.dev")
        .unwrap();
    assert_eq!(md.report.unmodelled.len(), 195 + 185 + 16);
    // groq `reasoning_effort in {none, default}`: 1 models.dev + 5 modelparams entries, all toggles
    assert_eq!(kinds("thinking-toggle"), 6);
}

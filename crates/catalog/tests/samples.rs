//! The adapters whose real rosters are private (`cliproxyapi-cache`, `omp-live`,
//! `router-for-me.models`) run here over small synthetic samples in `tests/fixtures/samples/`:
//! at most 40 rows each, public model ids, the same JSON shapes as the real files, and the cases
//! the real ones show (placeholder limits and ladders, unknown prices, rejected values, a
//! retired entry). The assertions are behaviour (counts, rejections, placeholders, matching
//! tiers, merge rules), not parity with the prototype.

mod common;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::LazyLock;

use serde_json::{Value, json};
use tierbridge_catalog::matching::{IdKey, Reference, TierCounts, match_report};
use tierbridge_catalog::model::{FieldName, LifecycleStatus, Observation};
use tierbridge_catalog::models_dev::Adapted;
use tierbridge_catalog::placeholder::Placeholders;
use tierbridge_catalog::{
    BuildOptions, BuildOutput, MergeOutput, Source, SourceInput, merge_catalog, models_dev,
};
use tierbridge_catalog::{cliproxyapi_cache, omp_live, router_models};

fn sample(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/samples")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

const CACHE: &str = "cliproxyapi-cache.sample.json";
const EASY: &str = "omp-models-easycliproxy.sample.json";
const DSH: &str = "omp-models-dsh.sample.json";
const ROUTER: &str = "router-models.sample.snapshot.json";

fn cache() -> Adapted {
    cliproxyapi_cache::adapt(&sample(CACHE)).unwrap()
}

fn live(file: &str, provider: &str) -> Adapted {
    omp_live::adapt(&sample(file), common::fetched_at_ms(), provider).unwrap()
}

fn router() -> Adapted {
    router_models::adapt(&sample(ROUTER), common::fetched_at_ms()).unwrap()
}

fn json_of(o: &Observation) -> Value {
    serde_json::to_value(o).unwrap()
}

fn cost_status(o: &Observation) -> Value {
    serde_json::to_value(&o.fields.cost).unwrap()["status"].clone()
}

fn find<'a>(a: &'a Adapted, id: &str) -> &'a Observation {
    a.observations
        .iter()
        .find(|o| o.fields.id == id)
        .unwrap_or_else(|| panic!("no observation {id}"))
}

fn rejected(a: &Adapted) -> Vec<(String, Value)> {
    let mut v: Vec<_> = a
        .findings
        .rejections
        .iter()
        .map(|d| (d.field.clone(), d.value.clone()))
        .collect();
    v.sort_by_key(|(f, v)| (f.clone(), v.to_string()));
    v
}

// ---- cliproxyapi-cache -------------------------------------------------------------------------

#[test]
fn cache_sample_rows_source_and_rejections() {
    let a = cache();
    assert_eq!(a.observations.len(), 25);
    assert_eq!(a.providers, 1);
    assert_eq!(
        serde_json::to_value(&a.source).unwrap(),
        json!({
            "sourceId": "cliproxyapi-cache",
            "kind": "discovered",
            "origin": "http://127.0.0.1:8317/v1/models?client_version=pi",
            "revision": "fetchedAt=1795670401936",
            "confidence": "inferred",
            "observedAtMs": 1_795_670_401_936_i64,
        })
    );
    // a tier without a threshold, `ultra` where the route accepts it (twice), a negative price;
    // `ultra: null` (not accepted) says nothing and is not reported
    assert_eq!(
        rejected(&a),
        vec![
            ("cost.input".to_owned(), json!(-1)),
            ("cost.tiers[0].inputTokensAbove".to_owned(), Value::Null),
            ("thinking.effortMap.ultra".to_owned(), json!("ultra")),
            ("thinking.effortMap.ultra".to_owned(), json!("ultra")),
        ]
    );
    assert_eq!(
        a.findings.sentinels.len(),
        1,
        "contextWindow 0 is a sentinel"
    );
}

#[test]
fn cache_sample_a_zero_price_is_unknown_never_free() {
    let a = cache();
    let raw: Value = serde_json::from_slice(&sample(CACHE)).unwrap();
    let zero: Vec<&str> = raw["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| {
            m["cost"]
                .as_object()
                .unwrap()
                .values()
                .all(|v| v == &json!(0))
        })
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(zero.len(), 4);
    for id in zero {
        assert_eq!(cost_status(find(&a, id)), json!("unknown"), "{id}");
    }
    let free = a
        .observations
        .iter()
        .filter(|o| cost_status(o) == json!("free"))
        .count();
    assert_eq!(free, 0, "a gateway never proves a model is free");
}

#[test]
fn cache_sample_placeholder_limits_and_ladder_are_detected() {
    let a = cache();
    let p = Placeholders::detect(a.observations.iter().map(|o| &o.fields));
    let by_limits = |cw: u64| {
        a.observations
            .iter()
            .filter(|o| o.fields.context_window == Some(cw))
            .collect::<Vec<_>>()
    };
    // 11 of 25 rows carry 272000/16384: well over the 20% share
    let defaults = by_limits(272_000);
    assert_eq!(defaults.len(), 11);
    assert!(
        defaults
            .iter()
            .all(|o| p.is_placeholder(&o.fields, FieldName::ContextWindow)
                && p.is_placeholder(&o.fields, FieldName::MaxTokens))
    );
    // a limit published once is evidence
    for o in by_limits(500_000) {
        assert!(!p.is_placeholder(&o.fields, FieldName::ContextWindow));
    }
    // the repeated low/medium/high ladder is a placeholder; a ladder with `max` is not
    assert!(p.is_placeholder(&defaults[0].fields, FieldName::Thinking));
    let with_max = a
        .observations
        .iter()
        .find(|o| {
            o.fields.context_window == Some(500_000)
                && json_of(o)["thinking"]["efforts"]
                    .as_array()
                    .is_some_and(|e| e.iter().any(|x| x == "max"))
        })
        .expect("a row with a max effort");
    assert!(!p.is_placeholder(&with_max.fields, FieldName::Thinking));
}

fn keys(a: &Adapted) -> Vec<IdKey> {
    a.observations.iter().map(IdKey::from).collect()
}

#[test]
fn cache_sample_ids_match_models_dev_in_tiers() {
    let reference =
        keys(&models_dev::adapt(&common::snapshot_api(), common::fetched_at_ms()).unwrap());
    let gateway = keys(&cache());
    let report = match_report(
        &[("cliproxyapi-cache", gateway.as_slice())],
        &[Reference {
            name: "models.dev",
            keys: &reference,
            probe: false,
        }],
    );
    let g = &report.gateways[0];
    assert_eq!(g.ids, 25);
    // 22 whole ids (`<id>:free` is itself a models.dev id), `acme/<id>` by its leaf,
    // `claude-sonnet-4.5` once `4.5` becomes `4-5`, and the unlisted id stays unmatched
    assert_eq!(
        g.per_reference["models.dev"],
        TierCounts {
            t1: 22,
            t2: 1,
            t3: 1,
            t4: 0,
            unmatched: 1,
        }
    );
}

// ---- omp-live ----------------------------------------------------------------------------------

#[test]
fn omp_sample_rows_source_rejection_and_sentinel() {
    let a = live(EASY, "easycliproxy");
    assert_eq!(a.observations.len(), 31);
    assert_eq!(a.providers, 1);
    assert_eq!(
        serde_json::to_value(&a.source).unwrap()["sourceId"],
        "omp-live:easycliproxy"
    );
    // an effort outside the contract enum is removed and reported; maxTokens 0 is a sentinel
    assert_eq!(
        rejected(&a),
        vec![("thinking.efforts".to_owned(), json!("ultra"))]
    );
    assert_eq!(a.findings.sentinels.len(), 1);
    // the roster has no prices at all: stand-in zeros are unknown, never free
    assert!(
        a.observations
            .iter()
            .all(|o| cost_status(o) == json!("unknown"))
    );
}

#[test]
fn omp_sample_the_gateway_default_limits_are_placeholders() {
    let a = live(EASY, "easycliproxy");
    let p = Placeholders::detect(a.observations.iter().map(|o| &o.fields));
    let pair = |cw: u64, mt: u64| {
        a.observations
            .iter()
            .filter(|o| o.fields.context_window == Some(cw) && o.fields.max_tokens == Some(mt))
            .collect::<Vec<_>>()
    };
    // 128000/32768 on 13 of 31 rows and 1000000/128000 on 8 are over the share; the 6 rows
    // with 262144/65536 (19%) are not
    for (cw, mt, rows, placeholder) in [
        (128_000, 32_768, 13, true),
        (1_000_000, 128_000, 8, true),
        (262_144, 65_536, 6, false),
    ] {
        let found = pair(cw, mt);
        assert_eq!(found.len(), rows, "{cw}/{mt}");
        for o in found {
            assert_eq!(
                p.is_placeholder(&o.fields, FieldName::ContextWindow),
                placeholder,
                "{cw}/{mt}"
            );
        }
    }
}

#[test]
fn omp_sample_a_second_provider_has_its_own_source_and_no_findings() {
    let a = live(DSH, "dsh");
    assert_eq!(a.observations.len(), 9);
    assert_eq!(
        serde_json::to_value(&a.source).unwrap()["sourceId"],
        "omp-live:dsh"
    );
    assert!(a.findings.rejections.is_empty() && a.findings.sentinels.is_empty());
    assert!(a.observations.iter().all(|o| o.fields.provider == "dsh"));
    // nine different limits: nothing repeats, nothing is a placeholder
    let p = Placeholders::detect(a.observations.iter().map(|o| &o.fields));
    assert!(p.pairs.is_empty());
}

// ---- router-for-me.models ----------------------------------------------------------------------

#[test]
fn router_sample_live_and_retired_rows_and_rejections() {
    let a = router();
    assert_eq!(a.observations.len(), 17);
    assert_eq!(a.providers, 6);
    let status = |o: &Observation| o.fields.lifecycle.as_ref().unwrap().status;
    let live = a
        .observations
        .iter()
        .filter(|o| status(o) == LifecycleStatus::Live)
        .count();
    assert_eq!(live, 14);
    let retired: BTreeSet<(String, String)> = a
        .observations
        .iter()
        .filter(|o| status(o) == LifecycleStatus::Retired)
        .map(|o| (o.fields.provider.clone(), o.fields.id.clone()))
        .collect();
    assert_eq!(retired.len(), 3);
    // only the retired (older) records carry efforts outside the contract enum
    assert_eq!(
        rejected(&a),
        vec![
            ("thinking.efforts".to_owned(), json!("auto")),
            ("thinking.efforts".to_owned(), json!("priority")),
            ("thinking.efforts".to_owned(), json!("ultra")),
        ]
    );
}

#[test]
fn router_sample_limits_thinking_and_image_rules() {
    let a = router();
    let row = |provider: &str, id: &str| {
        let o = a
            .observations
            .iter()
            .find(|o| o.fields.provider == provider && o.fields.id == id)
            .unwrap_or_else(|| panic!("no observation {provider}/{id}"));
        json_of(o)
    };
    // a Claude budget is a budget, not a ladder
    let claude = row("claude", "claude-sonnet-4-5");
    assert_eq!(claude["thinking"]["mode"], "budget");
    assert_eq!(claude["thinking"]["minBudgetTokens"], 1024);
    assert_eq!(claude["thinking"]["maxBudgetTokens"], 128_000);
    // Gemini's input limit is its context window
    let gemini = row("gemini", "gemini-2.5-pro");
    assert_eq!(gemini["contextWindow"], 1_048_576);
    assert_eq!(gemini["maxTokens"], 65_536);
    // Copilot's input limit is separate from the context window
    let copilot = row("copilot", "gpt-5");
    assert_eq!(copilot["contextWindow"], 400_000);
    assert_eq!(copilot["maxInputTokens"], 272_000);
    assert_eq!(copilot["thinking"]["mode"], "effort");
    // an image-only output is an image model without a context window
    let image = a
        .observations
        .iter()
        .map(json_of)
        .find(|v| v["kind"] == "image")
        .expect("an image-only entry");
    assert_eq!(image["provider"], "vertex");
    assert!(image["contextWindow"].is_null());
    // the same id under two channels is two rows
    assert_eq!(
        a.observations
            .iter()
            .filter(|o| o.fields.id == "gemini-2.5-pro")
            .count(),
        2
    );
}

// ---- merge over the public fixtures plus the samples -------------------------------------------

fn bytes_of(source: Source) -> Vec<u8> {
    match source {
        Source::CliproxyapiCache => sample(CACHE),
        Source::OmpLiveEasycliproxy => sample(EASY),
        Source::OmpLiveDsh => sample(DSH),
        Source::RouterModels => sample(ROUTER),
        Source::ModelsDev => common::zstd_fixture("models.dev.api.2026-11-26.json.zst"),
        Source::ModelparamsDev => common::zstd_fixture("modelparams.models.2026-11-26.json.zst"),
        Source::OmpBundled => common::zstd_fixture("omp-oracle-models.2026-11-26.json.zst"),
        Source::Dsh => common::zstd_fixture("dsh.b1142bd.snapshot.json.zst"),
        Source::Freellmapi => common::zstd_fixture("freellmapi.0d64e07.snapshot.json.zst"),
    }
}

static MERGED: LazyLock<MergeOutput> = LazyLock::new(|| {
    let builds: Vec<BuildOutput> = [
        Source::ModelsDev,
        Source::ModelparamsDev,
        Source::CliproxyapiCache,
        Source::OmpLiveEasycliproxy,
        Source::OmpLiveDsh,
        Source::OmpBundled,
        Source::RouterModels,
        Source::Dsh,
        Source::Freellmapi,
    ]
    .into_iter()
    .map(|source| {
        let bytes = bytes_of(source);
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
    .collect();
    merge_catalog(&builds.iter().collect::<Vec<_>>()).unwrap()
});

#[test]
fn merge_sample_every_model_validates_and_keys_are_unique() {
    let m = &*MERGED;
    let validator = common::schema_validator();
    for model in &m.models {
        let v = serde_json::to_value(model).unwrap();
        if let Some(e) = validator.iter_errors(&v).next() {
            panic!("{}: {e}", model.key);
        }
    }
    let keys: BTreeSet<&str> = m.models.iter().map(|x| x.key.as_str()).collect();
    assert_eq!(keys.len(), m.models.len());
}

#[test]
fn merge_sample_one_model_per_live_gateway_row_and_retired_rows_create_none() {
    let m = &*MERGED;
    let rows: usize = m.report.gateways.iter().map(|g| g.rows).sum();
    assert_eq!(rows, m.models.len());
    let router = m
        .report
        .gateways
        .iter()
        .find(|g| g.source_id == "router-for-me.models")
        .unwrap();
    assert_eq!(router.rows, 14);
    assert_eq!(router.retired_skipped, 3);
    // omp-bundled is a probe: it never creates rows nor appears as a source
    assert!(m.models.iter().all(|x| {
        x.provenance
            .sources
            .iter()
            .all(|s| s.source_id != "omp-bundled")
    }));
}

#[test]
fn merge_sample_no_placeholder_winner_and_no_gateway_zero_cost() {
    let m = &*MERGED;
    assert_eq!(m.report.placeholder_winners.total, 0);
    assert_eq!(m.report.cost.gateway_zero_winners, 0);
    for model in &m.models {
        let cost = model.fields.cost.as_ref().expect("cost is always explicit");
        let from = &model.provenance.fields[&FieldName::Cost];
        if cost.status == tierbridge_catalog::model::CostStatus::Unknown {
            assert!(
                cost.input.is_none() && cost.output.is_none(),
                "{}",
                model.key
            );
            assert_eq!(from.source, "none", "{}", model.key);
        }
    }
}

//! Matcher parity with the prototype: the tier counts of `prototype/out/results.json` (`match`,
//! `union`) are reproduced exactly, matching the Rust adapters' observations (not the golden
//! rows) of the three gateways against models.dev, modelparams.dev and omp-bundled (probe).

mod common;
mod gateway_common;

use gateway_common::SNAPSHOT_DAY;
use tierbridge_catalog::matching::{
    IdKey, Reference, TierCounts, match_ids, match_report, norm_leaf, split_id,
};
use tierbridge_catalog::model::{MatchTier, Observation};
use tierbridge_catalog::{cliproxyapi_cache, modelparams, models_dev, omp_bundled, omp_live};

fn keys(observations: &[Observation]) -> Vec<IdKey> {
    observations.iter().map(IdKey::from).collect()
}

struct Corpus {
    gateways: Vec<(&'static str, Vec<IdKey>)>,
    models_dev: Vec<IdKey>,
    modelparams: Vec<IdKey>,
    bundled: Vec<IdKey>,
}

fn corpus() -> Corpus {
    let at = common::fetched_at_ms();
    let day = SNAPSHOT_DAY;
    let live = |p: &str| {
        let bytes = common::zstd_fixture(&format!("omp-models-{p}.{day}.json.zst"));
        keys(&omp_live::adapt(&bytes, at, p).unwrap().observations)
    };
    let cache = common::zstd_fixture(&format!("cliproxyapi-models.{day}.json.zst"));
    Corpus {
        gateways: vec![
            (
                "cliproxyapi-cache",
                keys(&cliproxyapi_cache::adapt(&cache).unwrap().observations),
            ),
            ("omp-live:easycliproxy", live("easycliproxy")),
            ("omp-live:dsh", live("dsh")),
        ],
        models_dev: keys(
            &models_dev::adapt(&common::snapshot_api(), at)
                .unwrap()
                .observations,
        ),
        modelparams: keys(
            &modelparams::adapt(&common::modelparams_snapshot(), at)
                .unwrap()
                .observations,
        ),
        bundled: keys(
            &omp_bundled::adapt(
                &common::zstd_fixture(&format!("omp-oracle-models.{day}.json.zst")),
                at,
            )
            .unwrap()
            .observations,
        ),
    }
}

struct Expected {
    gateway: &'static str,
    ids: usize,
    models_dev: TierCounts,
    modelparams: TierCounts,
    bundled: TierCounts,
    either: usize,
    all: usize,
    neither: usize,
    exact_only: usize,
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn tier_counts_equal_results_json() {
    let c = corpus();
    let gateways: Vec<(&str, &[IdKey])> =
        c.gateways.iter().map(|(n, k)| (*n, k.as_slice())).collect();
    let references = [
        Reference {
            name: "models.dev",
            keys: &c.models_dev,
            probe: false,
        },
        Reference {
            name: "modelparams.dev",
            keys: &c.modelparams,
            probe: false,
        },
        Reference {
            name: "omp-bundled",
            keys: &c.bundled,
            probe: true,
        },
    ];
    let report = match_report(&gateways, &references);
    let expected = [
        Expected {
            gateway: "cliproxyapi-cache",
            ids: 1704,
            models_dev: TierCounts {
                t1: 810,
                t2: 248,
                t3: 152,
                t4: 3,
                unmatched: 491,
            },
            modelparams: TierCounts {
                t1: 259,
                t2: 142,
                t3: 217,
                t4: 17,
                unmatched: 1069,
            },
            bundled: TierCounts {
                t1: 564,
                t2: 277,
                t3: 148,
                t4: 0,
                unmatched: 715,
            },
            either: 1213,
            all: 635,
            neither: 491,
            exact_only: 1060,
        },
        Expected {
            gateway: "omp-live:easycliproxy",
            ids: 1699,
            models_dev: TierCounts {
                t1: 816,
                t2: 253,
                t3: 143,
                t4: 3,
                unmatched: 484,
            },
            modelparams: TierCounts {
                t1: 257,
                t2: 140,
                t3: 204,
                t4: 17,
                unmatched: 1081,
            },
            bundled: TierCounts {
                t1: 562,
                t2: 283,
                t3: 137,
                t4: 0,
                unmatched: 717,
            },
            either: 1215,
            all: 618,
            neither: 484,
            exact_only: 1071,
        },
        Expected {
            gateway: "omp-live:dsh",
            ids: 51,
            models_dev: TierCounts {
                t1: 0,
                t2: 47,
                t3: 0,
                t4: 0,
                unmatched: 4,
            },
            modelparams: TierCounts {
                t1: 0,
                t2: 14,
                t3: 6,
                t4: 0,
                unmatched: 31,
            },
            bundled: TierCounts {
                t1: 0,
                t2: 21,
                t3: 3,
                t4: 0,
                unmatched: 27,
            },
            either: 48,
            all: 19,
            neither: 3,
            exact_only: 48,
        },
    ];
    assert_eq!(report.gateways.len(), expected.len());
    for (got, want) in report.gateways.iter().zip(&expected) {
        let g = want.gateway;
        assert_eq!(got.source_id, g);
        assert_eq!(got.ids, want.ids, "{g} ids");
        assert_eq!(
            got.per_reference["models.dev"], want.models_dev,
            "{g} models.dev"
        );
        assert_eq!(
            got.per_reference["modelparams.dev"], want.modelparams,
            "{g} modelparams.dev"
        );
        assert_eq!(
            got.per_reference["omp-bundled"], want.bundled,
            "{g} omp-bundled"
        );
        assert_eq!(got.either, want.either, "{g} either");
        assert_eq!(got.all, want.all, "{g} both");
        assert_eq!(got.neither, want.neither, "{g} neither");
        assert_eq!(got.exact_only, want.exact_only, "{g} exact only");
    }
    let u = &report.union;
    assert_eq!(u.unique_ids, 1762);
    assert_eq!(u.per_reference["models.dev"], 1263);
    assert_eq!(u.per_reference["modelparams.dev"], 650);
    assert_eq!(u.either, 1264);
    assert_eq!(u.neither, 498);
    assert_eq!(u.in_more_than_one_source, 1675);
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn match_ids_reports_the_tier_and_the_references_per_id() {
    let c = corpus();
    let (_, cache) = &c.gateways[0];
    let matches = match_ids(cache, &c.models_dev);
    assert_eq!(matches.len(), 1704);
    let counts = TierCounts::of(&matches);
    assert_eq!(
        (counts.t1, counts.t2, counts.t3, counts.t4, counts.unmatched),
        (810, 248, 152, 3, 491)
    );
    let m = matches.iter().find(|m| m.id == "gpt-5.5").unwrap();
    assert!(m.tier.is_some());
    assert!(!m.refs.is_empty());
    assert!(
        m.refs
            .iter()
            .all(|r| r.source_id == "models.dev" && Some(r.tier) == m.tier)
    );
    assert!(
        matches
            .iter()
            .filter(|m| m.tier.is_none())
            .all(|m| m.refs.is_empty())
    );
}

#[test]
fn the_normalisation_rules() {
    assert_eq!(
        split_id("HF:Org/Sub/Duo-Chat-Claude"),
        (vec!["org".into(), "sub".into()], "claude".into())
    );
    assert_eq!(norm_leaf("gemini2.5", false, true), "gemini-2-5");
    assert_eq!(
        norm_leaf("claude-opus-4.6-thinking", false, true),
        "claude-opus-4-6"
    );
    assert_eq!(norm_leaf("qwen3_32b:free", false, true), "qwen-3-32b");
    assert_eq!(norm_leaf("m:latest", false, true), "m");
    assert_eq!(norm_leaf("m-high-thinking", false, true), "m");
    assert_eq!(
        norm_leaf("m-high-thinking", false, false),
        "m-high-thinking"
    );
    assert_eq!(
        norm_leaf("m-max", false, true),
        "m-max",
        "-max names a real lane"
    );
    assert_eq!(
        norm_leaf("claude-sonnet-4-20250929", true, true),
        "claude-sonnet-4"
    );
    assert_eq!(
        norm_leaf("claude-sonnet-4-2025-09-29", true, true),
        "claude-sonnet-4"
    );
    assert_eq!(norm_leaf("mistral-small-2507", true, true), "mistral-small");
    assert_eq!(
        norm_leaf("mistral-small-2513", true, true),
        "mistral-small-2513"
    );
    assert_eq!(
        norm_leaf("claude-sonnet-4-20250929", false, true),
        "claude-sonnet-4-20250929"
    );
}

#[test]
fn tiers_are_tried_in_order() {
    let k = |provider: &str, id: &str| IdKey {
        source_id: "ref".into(),
        provider: provider.into(),
        id: id.into(),
        canonical_id: None,
    };
    let reference = [k("acme", "model-4-5"), k("x", "other-20250101")];
    let gw = |id: &str| vec![k("gw", id)];
    let tier = |id: &str| match_ids(&gw(id), &reference)[0].tier;
    assert_eq!(tier("acme/model-4-5"), Some(MatchTier::ExactFull));
    assert_eq!(tier("anything/model-4-5"), Some(MatchTier::ExactLeaf));
    assert_eq!(tier("model-4.5-thinking"), Some(MatchTier::NormalizedLeaf));
    assert_eq!(tier("other"), Some(MatchTier::DateStripped));
    assert_eq!(tier("unrelated"), None);
}

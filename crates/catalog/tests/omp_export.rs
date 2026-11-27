//! `export_omp` over a synthetic merged catalog: which fields are exported and why not.

use serde_json::{Value, json};
use tierbridge_catalog::omp_export::{
    CurrentRoster, ExportOptions, REASON_NO_LIMIT, REASON_UNMATCHED, export_omp,
};

const GW: &str = "omp-live:gw";

fn field(source: &str, level: &str) -> Value {
    json!({"source": source, "rule": format!("chain [{level}]"), "confidence": "inferred"})
}

fn stuck() -> Value {
    json!({"source": "none", "rule": "chain [none]", "confidence": "unknown",
           "alternates": [{"source": GW, "value": 128000}]})
}

fn row(id: &str, fields: &Value, body: &Value, refs: usize) -> Value {
    let mut m = json!({
        "recordType": "model", "key": format!("{GW}:gw/{id}"), "provider": "gw", "id": id,
        "contextWindow": null, "maxTokens": null, "cost": null, "input": ["text"],
        "reasoning": true, "thinking": null,
        "references": vec![json!({}); refs],
        "provenance": {"sources": [{"sourceId": GW, "revision": "omp 19.2.1"}], "fields": fields},
    });
    for (k, v) in body.as_object().unwrap() {
        m[k] = v.clone();
    }
    m
}

fn catalog() -> Vec<u8> {
    let rows = vec![
        // limits and price from a reference, image input from the gateway
        row(
            "a/ref-wins",
            &json!({"contextWindow": field("models.dev", "ref-majority"),
                    "maxTokens": field("models.dev", "ref-first-party"),
                    "cost": field("models.dev", "ref-first-party"),
                    "input": field(GW, "gateway-live")}),
            &json!({"contextWindow": 512000, "maxTokens": 65536, "input": ["image", "text", "pdf"],
                    "cost": {"status": "known", "input": 0.15, "output": 0.6,
                             "cacheRead": 0.015, "cacheWrite": null}}),
            1,
        ),
        // the gateway's own real value: omp shows it already
        row(
            "gateway-real",
            &json!({"contextWindow": field(GW, "gateway-live"), "maxTokens": field(GW, "gateway-live"),
                    "cost": field("none", "none")}),
            &json!({"contextWindow": 1_000_000, "maxTokens": 384_000,
                    "cost": {"status": "unknown", "input": null, "output": null}}),
            0,
        ),
        // placeholders only, no reference matched
        row(
            "unmatched",
            &json!({"contextWindow": stuck(), "maxTokens": stuck()}),
            &json!({}),
            0,
        ),
        // matched but the reference has no limit; free price
        row(
            "matched-free",
            &json!({"contextWindow": stuck(), "maxTokens": stuck(),
                    "cost": field("models.dev", "ref-majority")}),
            &json!({"cost": {"status": "free", "input": 0.0, "output": 0.0}}),
            1,
        ),
        // already what omp shows
        row(
            "same-as-current",
            &json!({"contextWindow": field("models.dev", "ref-majority")}),
            &json!({"contextWindow": 200_000}),
            1,
        ),
        // thinking: an effort ladder with an off level and a remapped one
        row(
            "thinker",
            &json!({"thinking": field("models.dev", "ref-first-party"),
                    "reasoning": field("models.dev", "ref-first-party")}),
            &json!({"reasoning": false,
                    "thinking": {"mode": "effort", "efforts": ["off", "minimal", "high"],
                                 "defaultLevel": "high", "effortMap": {"minimal": "none"}}}),
            1,
        ),
        // a row of another gateway is not ours
        json!({"recordType": "model", "key": "dsh:dsh/x", "provider": "dsh", "id": "x",
               "references": [], "provenance": {"sources": [], "fields": {}}}),
    ];
    serde_json::to_vec(&rows).unwrap()
}

fn current() -> CurrentRoster {
    let roster = json!({"models": [
        {"provider": "gw", "id": "a/ref-wins", "contextWindow": 128000, "maxTokens": 32768,
         "pricingStatus": "unknown", "input": ["text", "image"], "reasoning": true},
        {"provider": "gw", "id": "same-as-current", "contextWindow": 200000, "maxTokens": 32768},
        {"provider": "gw", "id": "thinker", "reasoning": true,
         "thinking": ["minimal", "low", "medium", "high"]},
        {"provider": "gw", "id": "unmatched"},
        {"provider": "gw", "id": "matched-free"},
        {"provider": "gw", "id": "gateway-real"},
        {"provider": "gw", "id": "only-in-roster"},
    ]});
    CurrentRoster::parse(&serde_json::to_vec(&roster).unwrap()).unwrap()
}

fn opts(with_current: bool, with_thinking: bool) -> ExportOptions {
    ExportOptions {
        providers: vec!["gw".into()],
        current: if with_current {
            vec![current()]
        } else {
            vec![]
        },
        with_thinking,
    }
}

fn overrides(yaml: &str) -> Value {
    let v: serde_yaml::Value = serde_yaml::from_str(yaml).expect("export is valid YAML");
    let v: Value = serde_json::to_value(v).unwrap();
    v["providers"]["gw"]["modelOverrides"].clone()
}

#[test]
fn exports_only_reference_evidence_that_differs() {
    let out = export_omp(&catalog(), &opts(true, false)).unwrap();
    let ov = overrides(&out.yaml);
    assert_eq!(
        ov["a/ref-wins"],
        json!({"contextWindow": 512000, "maxTokens": 65536,
               "cost": {"input": 0.15, "output": 0.6, "cacheRead": 0.015}}),
        "gateway-owned input is not exported, unknown cacheWrite is omitted"
    );
    // free price is exported as zeros; the gateway's own limits and placeholders are not
    assert_eq!(
        ov["matched-free"],
        json!({"cost": {"input": 0, "output": 0}})
    );
    for id in ["gateway-real", "unmatched", "same-as-current", "thinker"] {
        assert!(ov.get(id).is_none(), "{id} has nothing to export");
    }
    let s = &out.stats[0];
    assert_eq!((s.rows, s.considered, s.with_overrides), (6, 6, 2));
    assert_eq!((s.not_in_roster, s.roster_not_in_catalog), (0, 1));
    assert_eq!(s.context_window.unchanged, 1);
    assert_eq!(s.context_window.gateway, 1);
    assert_eq!(s.cost_free, 1);
    assert_eq!(s.remaining.placeholder_models, 2);
    assert_eq!(s.remaining.reasons[REASON_UNMATCHED], 1);
    assert_eq!(s.remaining.reasons[REASON_NO_LIMIT], 1);
}

#[test]
fn thinking_only_with_the_flag_and_mapped_to_omp_efforts() {
    let plain = export_omp(&catalog(), &opts(true, false)).unwrap();
    assert!(overrides(&plain.yaml).get("thinker").is_none());

    let out = export_omp(&catalog(), &opts(true, true)).unwrap();
    let t = &overrides(&out.yaml)["thinker"];
    assert_eq!(t["reasoning"], false);
    assert_eq!(
        t["thinking"],
        json!({"mode": "effort", "efforts": ["minimal", "high"], "defaultLevel": "high",
               "effortMap": {"minimal": "none"}}),
        "`off` is not an omp effort"
    );
}

#[test]
fn export_is_deterministic_and_works_without_current() {
    let a = export_omp(&catalog(), &opts(false, true)).unwrap();
    let b = export_omp(&catalog(), &opts(false, true)).unwrap();
    assert_eq!(a.yaml, b.yaml);
    // without a roster nothing is compared: the same-as-current row is exported
    assert_eq!(
        overrides(&a.yaml)["same-as-current"],
        json!({"contextWindow": 200000})
    );
    assert!(a.yaml.starts_with("# tierbridge export omp"));
}

#[test]
fn awkward_ids_are_quoted_and_a_missing_provider_is_an_error() {
    let rows = vec![row(
        "weird:id #1",
        &json!({"contextWindow": field("models.dev", "ref-majority")}),
        &json!({"contextWindow": 4096}),
        1,
    )];
    let bytes = serde_json::to_vec(&rows).unwrap();
    let out = export_omp(&bytes, &opts(false, false)).unwrap();
    assert_eq!(
        overrides(&out.yaml)["weird:id #1"],
        json!({"contextWindow": 4096})
    );

    let mut missing = opts(false, false);
    missing.providers = vec!["nope".into()];
    assert!(export_omp(&bytes, &missing).is_err());
}

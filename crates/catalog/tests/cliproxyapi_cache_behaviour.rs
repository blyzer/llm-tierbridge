//! `cliproxyapi-cache` adapter rules on small hand-written caches: structural errors, rejected
//! values, sentinels and the gateway rule that a zero price is unknown.

use serde_json::{Value, json};
use tierbridge_catalog::cliproxyapi_cache::adapt;
use tierbridge_catalog::model::Observation;
use tierbridge_catalog::{CatalogError, Diagnostic};

fn cache(models: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "models": models,
        "modelsUrl": "http://127.0.0.1:8317/v1/models",
        "fetchedAt": 1_795_670_401_936_i64,
    }))
    .unwrap()
}

fn model(extra: Value) -> Value {
    let mut m = json!({
        "id": "m1", "name": "M1", "reasoning": false, "input": ["text"],
        "cost": {"input": 1, "output": 2, "cacheRead": 0.1, "cacheWrite": 0},
        "contextWindow": 1000, "maxTokens": 100,
        "lastSeenAt": 1, "stale": false,
    });
    m.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    m
}

fn adapt_one(m: Value) -> (Observation, Vec<Diagnostic>) {
    let a = adapt(&cache(json!([m]))).unwrap();
    (
        a.observations.into_iter().next().unwrap(),
        a.findings.rejections,
    )
}

fn field(o: &Observation, name: &str) -> Value {
    serde_json::to_value(o).unwrap()[name].clone()
}

fn structural(bytes: &[u8]) -> CatalogError {
    adapt(bytes).unwrap_err()
}

#[test]
fn structural_errors_fail_the_run() {
    assert!(matches!(
        structural(b"{\"models\": ["),
        CatalogError::InvalidJson(_)
    ));
    assert!(matches!(structural(b"[]"), CatalogError::Malformed(m) if m.contains("not an object")));
    let no_fetched = serde_json::to_vec(&json!({"models": [model(json!({}))]})).unwrap();
    assert!(
        matches!(structural(&no_fetched), CatalogError::Malformed(m) if m.contains("fetchedAt"))
    );
    let no_models = serde_json::to_vec(&json!({"fetchedAt": 1})).unwrap();
    assert!(matches!(structural(&no_models), CatalogError::Malformed(m) if m.contains("`models`")));
    assert!(matches!(
        structural(&cache(json!([]))),
        CatalogError::NoModels
    ));
    assert!(matches!(
        structural(&cache(json!(["m1"]))),
        CatalogError::Malformed(m) if m.contains("not an object")
    ));
    assert!(matches!(
        structural(&cache(json!([{"name": "x"}]))),
        CatalogError::Malformed(m) if m.contains("`id`")
    ));
    assert!(matches!(
        structural(&cache(json!([model(json!({})), model(json!({}))]))),
        CatalogError::Malformed(m) if m.contains("more than once")
    ));
}

#[test]
fn a_gateway_zero_price_is_unknown_not_free() {
    let zero = json!({"cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}});
    let (o, rej) = adapt_one(model(zero));
    assert!(rej.is_empty());
    assert_eq!(field(&o, "cost")["status"], "unknown");
    assert_eq!(field(&o, "cost")["input"], Value::Null);

    // a partly-zero list is still a price list: the zero cacheWrite is kept as published
    let (o, _) = adapt_one(model(json!({})));
    let cost = field(&o, "cost");
    assert_eq!(cost["status"], "known");
    assert_eq!(cost["cacheWrite"], 0.0);
}

#[test]
fn a_negative_price_is_rejected_and_makes_the_cost_unknown() {
    let (o, rej) = adapt_one(model(
        json!({"cost": {"input": -1, "output": 2, "cacheRead": 0, "cacheWrite": 0}}),
    ));
    assert_eq!(rej.len(), 1);
    assert_eq!(rej[0].field, "cost.input");
    assert_eq!(rej[0].value, json!(-1));
    assert_eq!(field(&o, "cost")["status"], "unknown");
    assert_eq!(field(&o, "cost")["output"], Value::Null);
}

#[test]
fn tiers_replace_the_base_prices_above_a_threshold() {
    let tier = |above: Value, input: Value| json!({"inputTokensAbove": above, "input": input, "output": 9, "cacheRead": 1, "cacheWrite": 0});
    let cost = json!({
        "input": 1, "output": 2, "cacheRead": 0.1, "cacheWrite": 0,
        "tiers": [tier(json!(272000), json!(3)), tier(json!(0), json!(4)), tier(json!(500000), json!(-5))],
    });
    let (o, rej) = adapt_one(model(json!({"cost": cost})));
    let c = field(&o, "cost");
    assert_eq!(c["tiers"].as_array().unwrap().len(), 1);
    assert_eq!(c["tiers"][0]["promptTokensAbove"], 272_000);
    assert_eq!(c["tiers"][0]["input"], 3.0);
    let fields: Vec<_> = rej.iter().map(|d| d.field.as_str()).collect();
    assert_eq!(
        fields,
        ["cost.tiers[1].inputTokensAbove", "cost.tiers[2].input"]
    );
}

#[test]
fn zero_limits_are_sentinels_and_negative_limits_are_rejections() {
    let a = adapt(&cache(json!([model(
        json!({"contextWindow": 0, "maxTokens": -5})
    )])))
    .unwrap();
    let o = &a.observations[0];
    assert_eq!(o.fields.context_window, None);
    assert_eq!(o.fields.max_tokens, None);
    assert_eq!(a.findings.zero_limits, 1);
    assert_eq!(a.findings.sentinels.len(), 1);
    assert_eq!(a.findings.sentinels[0].field, "contextWindow");
    assert_eq!(a.findings.sentinels[0].mapped_to, "unknown");
    assert_eq!(a.findings.rejections.len(), 1);
    assert_eq!(a.findings.rejections[0].field, "maxTokens");
}

#[test]
fn thinking_comes_from_the_level_map() {
    let levels = json!({"off": null, "minimal": null, "low": "low", "high": "xhigh", "ultra": "ultra", "turbo": null});
    let (o, rej) = adapt_one(model(
        json!({"reasoning": true, "thinkingLevelMap": levels}),
    ));
    let t = field(&o, "thinking");
    assert_eq!(t["mode"], "effort");
    assert_eq!(t["efforts"], json!(["low", "high"]));
    assert_eq!(
        t["effortMap"],
        json!({"off": null, "minimal": null, "low": "low", "high": "xhigh"})
    );
    // only the level the route accepts is reported; `turbo: null` says nothing to keep
    assert_eq!(rej.len(), 1);
    assert_eq!(rej[0].field, "thinking.effortMap.ultra");
    assert_eq!(rej[0].value, json!("ultra"));

    let (o, _) = adapt_one(model(json!({"reasoning": true})));
    assert_eq!(field(&o, "thinking")["mode"], "unknown");
    let (o, _) = adapt_one(model(
        json!({"reasoning": true, "thinkingLevelMap": {"low": null}}),
    ));
    assert_eq!(field(&o, "thinking")["mode"], "unknown");
    let (o, _) = adapt_one(model(json!({"reasoning": false})));
    assert_eq!(field(&o, "thinking")["mode"], "none");
    let (o, rej) = adapt_one(model(json!({"reasoning": "yes"})));
    assert_eq!(field(&o, "thinking"), Value::Null);
    assert_eq!(rej[0].field, "reasoning");
}

#[test]
fn modalities_outside_the_contract_are_rejected() {
    let (o, rej) = adapt_one(model(
        json!({"input": ["text", "image", "hologram", "text"]}),
    ));
    assert_eq!(field(&o, "input"), json!(["image", "text"]));
    assert_eq!(rej.len(), 1);
    assert_eq!(rej[0].value, json!("hologram"));
}

#[test]
fn the_row_keeps_the_cache_bookkeeping_in_raw() {
    let (o, _) = adapt_one(model(json!({"stale": true, "lastSeenAt": 42})));
    assert_eq!(field(&o, "raw"), json!({"stale": true, "lastSeenAt": 42}));
    assert_eq!(field(&o, "provider"), "cliproxyapi");
    assert_eq!(field(&o, "kind"), "chat");
}

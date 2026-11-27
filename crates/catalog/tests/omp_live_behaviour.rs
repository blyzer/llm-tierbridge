//! `omp-live` adapter rules on small hand-written rosters.

use serde_json::{Value, json};
use tierbridge_catalog::model::Observation;
use tierbridge_catalog::omp_live::adapt;
use tierbridge_catalog::{CatalogError, Diagnostic};

const AT: i64 = 1_795_682_025_014;

fn roster(models: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({ "models": models })).unwrap()
}

fn model(extra: Value) -> Value {
    let mut m = json!({
        "provider": "dsh", "kind": "chat", "id": "acct/m1", "selector": "dsh/acct/m1",
        "name": "M1", "contextWindow": 1000, "maxTokens": 100, "reasoning": true,
        "thinking": ["high", "low"], "input": ["text", "image"],
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
        "pricingStatus": "unknown",
    });
    m.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    m
}

fn adapt_one(m: Value) -> (Observation, Vec<Diagnostic>) {
    let a = adapt(&roster(json!([m])), AT, "dsh").unwrap();
    (
        a.observations.into_iter().next().unwrap(),
        a.findings.rejections,
    )
}

fn field(o: &Observation, name: &str) -> Value {
    serde_json::to_value(o).unwrap()[name].clone()
}

fn structural(bytes: &[u8]) -> CatalogError {
    adapt(bytes, AT, "dsh").unwrap_err()
}

#[test]
fn structural_errors_fail_the_run() {
    assert!(matches!(
        structural(b"{\"models\":"),
        CatalogError::InvalidJson(_)
    ));
    assert!(matches!(structural(b"[]"), CatalogError::Malformed(m) if m.contains("not an object")));
    assert!(matches!(structural(b"{}"), CatalogError::Malformed(m) if m.contains("`models`")));
    assert!(matches!(
        structural(&roster(json!([]))),
        CatalogError::NoModels
    ));
    assert!(matches!(
        structural(&roster(json!([5]))),
        CatalogError::Malformed(m) if m.contains("not an object")
    ));
    assert!(matches!(
        structural(&roster(json!([{"name": "x"}]))),
        CatalogError::Malformed(m) if m.contains("`id`")
    ));
    assert!(matches!(
        structural(&roster(json!([model(json!({})), model(json!({}))]))),
        CatalogError::Malformed(m) if m.contains("more than once")
    ));
    // the roster of another provider was passed: wrong file, not a value problem
    assert!(matches!(
        structural(&roster(json!([model(json!({"provider": "easycliproxy"}))]))),
        CatalogError::Malformed(m) if m.contains("easycliproxy")
    ));
}

#[test]
fn zero_prices_of_an_unpriced_model_are_unknown_never_free() {
    let (o, rej) = adapt_one(model(json!({})));
    assert!(rej.is_empty());
    assert_eq!(field(&o, "cost")["status"], "unknown");
    // a missing status is not a promise that the zeros are real
    let mut m = model(json!({}));
    m.as_object_mut().unwrap().remove("pricingStatus");
    assert_eq!(field(&adapt_one(m).0, "cost")["status"], "unknown");
    // only an explicit `known` makes zeros a real price
    let (o, _) = adapt_one(model(json!({"pricingStatus": "known"})));
    assert_eq!(field(&o, "cost")["status"], "free");
    let known = json!({"input": 1, "output": 2, "cacheRead": 0, "cacheWrite": 0});
    let (o, _) = adapt_one(model(json!({"pricingStatus": "unknown", "cost": known})));
    assert_eq!(field(&o, "cost")["status"], "known");
}

#[test]
fn the_row_keeps_the_selector_and_the_pricing_status() {
    let (o, _) = adapt_one(model(json!({})));
    assert_eq!(field(&o, "selector"), "dsh/acct/m1");
    assert_eq!(field(&o, "raw"), json!({"pricingStatus": "unknown"}));
    assert_eq!(field(&o, "provider"), "dsh");
    assert_eq!(field(&o, "kind"), "chat");
    assert_eq!(field(&o, "input"), json!(["image", "text"]));
}

#[test]
fn thinking_efforts_are_sorted_and_unknown_spellings_are_rejected() {
    let (o, rej) = adapt_one(model(
        json!({"thinking": ["max", "low", "ultra", "low", "none"]}),
    ));
    let t = field(&o, "thinking");
    assert_eq!(t["mode"], "effort");
    assert_eq!(t["efforts"], json!(["off", "low", "max"]));
    assert_eq!(rej.len(), 1);
    assert_eq!(rej[0].field, "thinking.efforts");
    assert_eq!(rej[0].value, json!("ultra"));

    let (o, _) = adapt_one(model(json!({"thinking": null})));
    assert_eq!(field(&o, "thinking")["mode"], "unknown");
    let (o, _) = adapt_one(model(json!({"thinking": []})));
    assert_eq!(field(&o, "thinking")["mode"], "unknown");
    let (o, _) = adapt_one(model(json!({"reasoning": false})));
    assert_eq!(field(&o, "thinking")["mode"], "none");
    let (o, rej) = adapt_one(model(json!({"thinking": "high"})));
    assert_eq!(field(&o, "thinking")["mode"], "unknown");
    assert_eq!(rej[0].reason, "expected an array");
}

#[test]
fn a_kind_outside_the_contract_is_rejected() {
    let (o, rej) = adapt_one(model(json!({"kind": "agent"})));
    assert_eq!(field(&o, "kind"), Value::Null);
    assert_eq!(rej[0].field, "kind");
    let (o, rej) = adapt_one(model(json!({"kind": "embedding"})));
    assert_eq!(field(&o, "kind"), "embedding");
    assert!(rej.is_empty());
}

#[test]
fn limits_follow_the_gateway_rules() {
    let a = adapt(
        &roster(json!([model(
            json!({"contextWindow": 0, "maxTokens": 1.5})
        )])),
        AT,
        "dsh",
    )
    .unwrap();
    let o = &a.observations[0];
    assert_eq!((o.fields.context_window, o.fields.max_tokens), (None, None));
    assert_eq!(a.findings.zero_limits, 1);
    assert_eq!(a.findings.rejections[0].field, "maxTokens");
}

//! Structural errors, rejected values and documented sentinels, on small synthetic snapshots.

mod common;

use serde_json::{Value, json};
use tierbridge_catalog::{
    BuildOptions, CatalogError, SourceInput, build_models_dev, models_dev, single_source,
};

const T0: i64 = 1_795_681_976_000;

fn run(doc: &Value) -> Result<tierbridge_catalog::BuildOutput, CatalogError> {
    run_bytes(doc.to_string().as_bytes(), &BuildOptions::default())
}

fn run_bytes(
    bytes: &[u8],
    options: &BuildOptions,
) -> Result<tierbridge_catalog::BuildOutput, CatalogError> {
    build_models_dev(
        SourceInput {
            bytes,
            fetched_at_ms: T0,
            fetch: None,
        },
        options,
    )
}

/// A one-model snapshot; `model` is merged over a minimal valid reasoning model.
fn one(model: Value) -> Value {
    let mut base = json!({"name": "M", "reasoning": false, "tool_call": true,
        "modalities": {"input": ["text"], "output": ["text"]}, "limit": {"context": 1000, "output": 100}});
    base.as_object_mut()
        .unwrap()
        .extend(model.as_object().unwrap().clone());
    json!({"p": {"models": {"m": base}}})
}

fn fields(out: &tierbridge_catalog::BuildOutput) -> Value {
    serde_json::to_value(&out.observations[0]).unwrap()
}

// ---- structural errors -------------------------------------------------------------------------

#[test]
fn truncated_json_is_a_structural_error() {
    let full = common::snapshot_api();
    let half = &full[..full.len() / 2];
    let err = run_bytes(half, &BuildOptions::default()).unwrap_err();
    assert!(matches!(err, CatalogError::InvalidJson(_)), "{err}");
}

#[test]
fn shapes_that_are_not_providers_of_models_are_structural_errors() {
    assert!(matches!(
        run(&json!({})).unwrap_err(),
        CatalogError::NoModels
    ));
    assert!(matches!(
        run(&json!([])).unwrap_err(),
        CatalogError::NotAnObject
    ));
    assert!(matches!(
        run(&json!("x")).unwrap_err(),
        CatalogError::NotAnObject
    ));
    assert!(matches!(
        run(&json!({"p": 1})).unwrap_err(),
        CatalogError::ProviderNotObject { .. }
    ));
    assert!(matches!(
        run(&json!({"p": {}})).unwrap_err(),
        CatalogError::MissingModels { .. }
    ));
    assert!(matches!(
        run(&json!({"p": {"models": []}})).unwrap_err(),
        CatalogError::MissingModels { .. }
    ));
    assert!(matches!(
        run(&json!({"p": {"models": {}}})).unwrap_err(),
        CatalogError::NoModels
    ));
    assert!(matches!(
        run(&json!({"p": {"models": {"m": 3}}})).unwrap_err(),
        CatalogError::ModelNotObject { .. }
    ));
    assert!(matches!(
        run_bytes(b"", &BuildOptions::default()).unwrap_err(),
        CatalogError::InvalidJson(_)
    ));
}

#[test]
fn min_models_not_reached_is_a_structural_error() {
    let doc = one(json!({}));
    let at = BuildOptions {
        min_models: Some(1),
    };
    assert!(run_bytes(doc.to_string().as_bytes(), &at).is_ok());
    let over = BuildOptions {
        min_models: Some(2),
    };
    assert!(matches!(
        run_bytes(doc.to_string().as_bytes(), &over).unwrap_err(),
        CatalogError::BelowMinModels { found: 1, min: 2 }
    ));
}

// ---- rejected values ---------------------------------------------------------------------------

#[test]
fn effort_outside_the_enum_is_removed_and_reported() {
    let out = run(&one(json!({"reasoning": true,
        "reasoning_options": [{"type": "effort", "values": ["none", "default", "high"]}]})))
    .unwrap();
    let f = fields(&out);
    assert_eq!(f["thinking"]["efforts"], json!(["off", "high"]));
    let [d] = out.report.rejections.as_slice() else {
        panic!("{:?}", out.report.rejections)
    };
    assert_eq!(
        serde_json::to_value(d).unwrap(),
        json!({"sourceId": "models.dev", "provider": "p", "id": "m", "field": "thinking.efforts",
               "value": "default", "reason": "effort outside the contract enum"})
    );
    assert!(out.report.sentinels.is_empty());
}

#[test]
fn negative_prices_are_removed_and_reported() {
    let out = run(&one(json!({"cost": {"input": -1, "output": 2.5}}))).unwrap();
    let f = fields(&out);
    assert_eq!(
        f["cost"],
        json!({"status": "known", "input": null, "output": 2.5, "cacheRead": null, "cacheWrite": null})
    );
    let [d] = out.report.rejections.as_slice() else {
        panic!()
    };
    assert_eq!((d.field.as_str(), &d.value), ("cost.input", &json!(-1)));
}

#[test]
fn a_tier_with_a_negative_price_is_dropped_whole_and_reported() {
    let out = run(&one(json!({"cost": {"input": 1, "output": 2, "tiers": [
        {"tier": {"type": "context", "size": 100000}, "input": -3, "output": 4},
        {"tier": {"type": "context", "size": 200000}, "input": 5, "output": 6}]}})))
    .unwrap();
    let f = fields(&out);
    let tiers = f["cost"]["tiers"].as_array().unwrap();
    assert_eq!(tiers.len(), 1);
    assert_eq!(tiers[0]["promptTokensAbove"], 200_000);
    let [d] = out.report.rejections.as_slice() else {
        panic!()
    };
    assert_eq!(
        (d.field.as_str(), &d.value),
        ("cost.tiers[0].input", &json!(-3))
    );
}

#[test]
fn context_over_200k_still_becomes_a_tier_when_there_are_no_explicit_tiers() {
    let out = run(&one(json!({"cost": {"input": 1, "output": 2,
        "context_over_200k": {"input": 3, "output": 4, "cache_read": 0.5}}})))
    .unwrap();
    let f = fields(&out);
    assert_eq!(
        f["cost"]["tiers"],
        json!([{"promptTokensAbove": 200000, "input": 3.0, "output": 4.0, "cacheRead": 0.5, "cacheWrite": null}])
    );
    assert!(out.report.rejections.is_empty());
}

#[test]
fn negative_and_malformed_limits_are_rejected() {
    let out = run(&one(
        json!({"limit": {"context": -5, "input": 1.5, "output": "big"}}),
    ))
    .unwrap();
    let f = fields(&out);
    for k in ["contextWindow", "maxInputTokens", "maxTokens"] {
        assert_eq!(f[k], Value::Null, "{k}");
    }
    let mut fields_rejected: Vec<_> = out
        .report
        .rejections
        .iter()
        .map(|d| d.field.as_str())
        .collect();
    fields_rejected.sort();
    assert_eq!(
        fields_rejected,
        ["contextWindow", "maxInputTokens", "maxTokens"]
    );
}

#[test]
fn a_zero_limit_is_models_dev_for_no_limit_published_not_a_rejection() {
    let out = run(&one(json!({"limit": {"context": 0, "output": 100}}))).unwrap();
    let f = fields(&out);
    assert_eq!(
        (&f["contextWindow"], &f["maxTokens"]),
        (&Value::Null, &json!(100))
    );
    assert!(out.report.rejections.is_empty());
    assert_eq!(out.report.counts.zero_limits_mapped_to_unknown, 1);
    assert_eq!(out.report.counts.sentinels, 1);
    let [s] = out.report.sentinels.as_slice() else {
        panic!("{:?}", out.report.sentinels)
    };
    assert_eq!(
        serde_json::to_value(s).unwrap(),
        json!({"sourceId": "models.dev", "provider": "p", "id": "m", "field": "contextWindow",
               "value": 0, "mappedTo": "unknown",
               "reason": "models.dev publishes 0 when no token limit applies"})
    );
}

#[test]
fn negative_budget_bounds_other_than_the_sentinel_are_rejected() {
    let out = run(&one(json!({"reasoning": true,
        "reasoning_options": [{"type": "budget_tokens", "min": -7, "max": -1}]})))
    .unwrap();
    let f = fields(&out);
    assert_eq!(f["thinking"]["minBudgetTokens"], Value::Null);
    assert_eq!(f["thinking"]["dynamicBudget"], Value::Null);
    assert_eq!(out.report.rejections.len(), 2);
    assert!(out.report.sentinels.is_empty());
}

#[test]
fn strict_turns_any_rejection_into_failure() {
    let clean = run(&one(json!({}))).unwrap();
    assert!(clean.ensure_no_rejections().is_ok());
    let dirty = run(&one(json!({"cost": {"input": -1}}))).unwrap();
    assert!(matches!(
        dirty.ensure_no_rejections().unwrap_err(),
        CatalogError::StrictRejections { count: 1 }
    ));
}

// ---- documented sentinel -----------------------------------------------------------------------

#[test]
fn budget_min_minus_one_maps_to_dynamic_budget_without_a_rejection() {
    let out = run(&one(json!({"reasoning": true,
        "reasoning_options": [{"type": "budget_tokens", "min": -1, "max": 32768}]})))
    .unwrap();
    let f = fields(&out);
    assert_eq!(
        f["thinking"],
        json!({"mode": "budget", "efforts": [], "canDisable": null, "minBudgetTokens": null,
               "maxBudgetTokens": 32768, "dynamicBudget": true})
    );
    assert!(out.report.rejections.is_empty());
    let [s] = out.report.sentinels.as_slice() else {
        panic!()
    };
    assert_eq!(
        (s.field.as_str(), &s.value),
        ("thinking.minBudgetTokens", &json!(-1))
    );
}

// ---- model[] and provenance --------------------------------------------------------------------

#[test]
fn single_source_model_has_provenance_for_every_non_null_field() {
    let out = run(&one(
        json!({"canonical_model_id": "v/m", "cost": {"input": 1, "output": 2}}),
    ))
    .unwrap();
    let model = serde_json::to_value(&out.models[0]).unwrap();
    assert_eq!(model["recordType"], "model");
    assert_eq!(model["key"], "p/m");
    assert_eq!(model["references"], json!([]));
    assert!(model.get("raw").is_none());
    let source = &model["provenance"]["sources"];
    assert_eq!(source.as_array().unwrap().len(), 1);
    assert_eq!(source[0]["sourceId"], "models.dev");
    let prov = model["provenance"]["fields"].as_object().unwrap();
    let mut names: Vec<_> = prov.keys().map(String::as_str).collect();
    names.sort();
    // everything non-null on the observation, nothing for null `maxInputTokens`/`acceptedParams`
    assert_eq!(
        names,
        [
            "canonicalId",
            "contextWindow",
            "cost",
            "input",
            "lifecycle",
            "maxTokens",
            "name",
            "output",
            "reasoning",
            "supportsTools",
            "thinking"
        ]
    );
    for p in prov.values() {
        assert_eq!(
            p,
            &json!({"source": "models.dev", "rule": "single-source", "confidence": "declared"})
        );
    }
}

#[test]
fn model_key_keeps_slashes_in_the_id() {
    let doc = json!({"openrouter": {"models": {"anthropic/claude-x": {"name": "X"}}}});
    let obs = models_dev::adapt(doc.to_string().as_bytes(), T0)
        .unwrap()
        .observations;
    assert_eq!(single_source(&obs[0]).key, "openrouter/anthropic/claude-x");
}

#[test]
fn output_is_sorted_by_provider_then_id() {
    let doc = json!({"b": {"models": {"z": {}, "a": {}}}, "a": {"models": {"m": {}}}});
    let out = run(&doc).unwrap();
    let keys: Vec<_> = out.models.iter().map(|m| m.key.as_str()).collect();
    assert_eq!(keys, ["a/m", "b/a", "b/z"]);
}

#[test]
fn unknown_stays_null_not_false() {
    let doc = json!({"p": {"models": {"m": {}}}});
    let out = run(&doc).unwrap();
    let f = fields(&out);
    assert_eq!(f["reasoning"], Value::Null);
    assert_eq!(f["thinking"], Value::Null);
    assert_eq!(f["supportsTools"], Value::Null);
}

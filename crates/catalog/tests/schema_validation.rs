//! Every emitted observation and model validates against `schema/gateway-model.schema.json`.

mod common;

use tierbridge_catalog::{BuildOptions, SourceInput, build_modelparams, build_models_dev};

#[test]
fn every_observation_and_model_validates_against_the_schema() {
    let bytes = common::snapshot_api();
    let out = build_models_dev(
        SourceInput {
            bytes: &bytes,
            fetched_at_ms: common::fetched_at_ms(),
            fetch: None,
        },
        &BuildOptions::default(),
    )
    .unwrap();
    assert_eq!(out.observations.len(), 10_049);
    assert_eq!(out.models.len(), 10_049);

    let validator = common::schema_validator();
    let mut failures = Vec::new();
    let records = out
        .observations
        .iter()
        .map(|o| serde_json::to_value(o).unwrap())
        .chain(out.models.iter().map(|m| serde_json::to_value(m).unwrap()));
    for record in records {
        if let Some(e) = validator.iter_errors(&record).next() {
            failures.push(format!("{} {}: {e}", record["provider"], record["id"]));
        }
    }
    assert!(
        failures.is_empty(),
        "{} invalid records, first: {}",
        failures.len(),
        failures[0]
    );
}

#[test]
fn the_schema_rejects_what_it_should() {
    // guards the guard: an observation with an effort outside the enum must not validate.
    let validator = common::schema_validator();
    let bytes = common::snapshot_api();
    let out = build_models_dev(
        SourceInput {
            bytes: &bytes,
            fetched_at_ms: common::fetched_at_ms(),
            fetch: None,
        },
        &BuildOptions::default(),
    )
    .unwrap();
    let mut bad = serde_json::to_value(&out.observations[0]).unwrap();
    bad["thinking"] = serde_json::json!({"mode": "effort", "efforts": ["default"]});
    assert!(!validator.is_valid(&bad));
    let mut extra = serde_json::to_value(&out.models[0]).unwrap();
    extra["raw"] = serde_json::json!({});
    assert!(
        !validator.is_valid(&extra),
        "`raw` belongs to observations only"
    );
}

#[test]
fn every_modelparams_observation_and_model_validates_against_the_schema() {
    let bytes = common::modelparams_snapshot();
    let out = build_modelparams(
        SourceInput {
            bytes: &bytes,
            fetched_at_ms: common::fetched_at_ms(),
            fetch: None,
        },
        &BuildOptions::default(),
    )
    .unwrap();
    assert_eq!(out.observations.len(), 510);
    assert_eq!(out.models.len(), 510);

    let validator = common::schema_validator();
    let mut failures = Vec::new();
    let records = out
        .observations
        .iter()
        .map(|o| serde_json::to_value(o).unwrap())
        .chain(out.models.iter().map(|m| serde_json::to_value(m).unwrap()));
    for record in records {
        if let Some(e) = validator.iter_errors(&record).next() {
            failures.push(format!("{} {}: {e}", record["provider"], record["id"]));
        }
    }
    assert!(
        failures.is_empty(),
        "{} invalid records, first: {}",
        failures.len(),
        failures[0]
    );
}

#[test]
fn repeated_model_keys_in_modelparams_are_exactly_the_auth_type_pairs() {
    // `key = provider/id` is not unique per source: modelparams.dev lists api_key and subscription
    // contracts of the same model separately. Merging them is slice 3; until then this is the
    // only way a key may repeat, and the two records stay distinguishable by `authType`.
    let bytes = common::modelparams_snapshot();
    let out = build_modelparams(
        SourceInput {
            bytes: &bytes,
            fetched_at_ms: common::fetched_at_ms(),
            fetch: None,
        },
        &BuildOptions::default(),
    )
    .unwrap();
    let mut by_key = std::collections::BTreeMap::<&str, Vec<_>>::new();
    for m in &out.models {
        by_key.entry(&m.key).or_default().push(m.fields.auth_type);
    }
    let repeated: Vec<_> = by_key.values().filter(|v| v.len() > 1).collect();
    assert_eq!(by_key.len(), 448, "distinct provider/id keys");
    assert_eq!(repeated.len(), 62, "keys listed under both auth types");
    for auths in repeated {
        assert_eq!(auths.len(), 2);
        assert_ne!(auths[0], auths[1]);
    }
}

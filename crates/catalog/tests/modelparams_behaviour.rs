//! modelparams.dev adapter rules on small hand-written snapshots: structural errors, rejected
//! values, documented sentinels and the field mapping.

mod common;

use serde_json::{Value, json};
use tierbridge_catalog::{
    BuildOptions, CatalogError, SourceInput, build_modelparams, modelparams::adapt,
};

const FETCHED_AT: i64 = 1_795_681_976_000;

fn snapshot(models: Value) -> Vec<u8> {
    let count = models.as_array().map_or(0, Vec::len);
    serde_json::to_vec(&json!({
        "generatedAt": "2026-11-24T08:22:46.486Z",
        "count": count,
        "models": models,
    }))
    .unwrap()
}

fn entry(params: Value) -> Value {
    json!({
        "provider": "acme", "model": "m1", "authType": "api_key",
        "apiSurface": "openai-chat-completions", "params": params,
    })
}

fn param(path: &str, ty: &str, group: &str) -> Value {
    json!({"path": path, "type": ty, "group": group, "label": path, "description": path})
}

fn adapt_one(entry: Value) -> tierbridge_catalog::models_dev::Adapted {
    adapt(&snapshot(json!([entry])), FETCHED_AT).unwrap()
}

fn structural(bytes: &[u8]) -> CatalogError {
    adapt(bytes, FETCHED_AT).unwrap_err()
}

// ---- structural errors: the run fails, nothing is produced -----------------------------------

#[test]
fn structural_errors_fail_the_run() {
    let ok = entry(json!([]));
    assert!(matches!(
        structural(b"{\"models\": ["),
        CatalogError::InvalidJson(_)
    ));
    assert!(matches!(structural(b"[]"), CatalogError::Malformed(m) if m.contains("not an object")));
    let no_gen = serde_json::to_vec(&json!({"models": [ok.clone()]})).unwrap();
    assert!(matches!(structural(&no_gen), CatalogError::Malformed(m) if m.contains("generatedAt")));
    let bad_gen =
        serde_json::to_vec(&json!({"generatedAt": "yesterday", "models": [ok.clone()]})).unwrap();
    assert!(matches!(
        structural(&bad_gen),
        CatalogError::InvalidTimestamp { .. }
    ));
    let no_models = serde_json::to_vec(&json!({"generatedAt": "2026-11-24T08:22:46Z"})).unwrap();
    assert!(matches!(structural(&no_models), CatalogError::Malformed(m) if m.contains("`models`")));
    assert!(matches!(
        structural(&snapshot(json!([]))),
        CatalogError::NoModels
    ));
}

#[test]
fn a_truncated_snapshot_is_detected_through_count() {
    let bytes = serde_json::to_vec(&json!({
        "generatedAt": "2026-11-24T08:22:46Z", "count": 3, "models": [entry(json!([]))],
    }))
    .unwrap();
    assert!(matches!(structural(&bytes), CatalogError::Malformed(m) if m.contains("declares 3")));
}

#[test]
fn malformed_entries_fail_the_run() {
    for (bad, needle) in [
        (json!(7), "not an object"),
        (
            json!({"model": "m", "authType": "api_key", "apiSurface": "x", "params": []}),
            "provider",
        ),
        (
            json!({"provider": "p", "authType": "api_key", "apiSurface": "x", "params": []}),
            "`model`",
        ),
        (
            json!({"provider": "p", "model": "m", "apiSurface": "x", "params": []}),
            "authType",
        ),
        (
            json!({"provider": "p", "model": "m", "authType": "api_key", "params": []}),
            "apiSurface",
        ),
        (
            json!({"provider": "p", "model": "m", "authType": "api_key", "apiSurface": "x"}),
            "`params`",
        ),
    ] {
        let err = structural(&snapshot(json!([bad])));
        assert!(
            matches!(&err, CatalogError::Malformed(m) if m.contains(needle)),
            "{needle}: {err}"
        );
    }
    let twin = entry(json!([]));
    let err = structural(&snapshot(json!([twin.clone(), twin])));
    assert!(
        matches!(&err, CatalogError::Malformed(m) if m.contains("repeats")),
        "{err}"
    );
}

#[test]
fn the_same_model_under_both_auth_types_is_not_a_duplicate() {
    let mut sub = entry(json!([]));
    sub["authType"] = json!("subscription");
    let adapted = adapt(&snapshot(json!([sub, entry(json!([]))])), FETCHED_AT).unwrap();
    let auth: Vec<_> = adapted
        .observations
        .iter()
        .map(|o| serde_json::to_value(o).unwrap()["authType"].clone())
        .collect();
    assert_eq!(
        auth,
        [json!("api_key"), json!("subscription")],
        "sorted, api_key first"
    );
}

// ---- rejected values: removed, reported, run continues ---------------------------------------

#[test]
fn out_of_enum_efforts_are_rejected_and_the_rest_is_kept() {
    let mut effort = param("reasoning_effort", "enum", "reasoning");
    effort["values"] = json!(["none", "low", "ultra", "default", 3]);
    let adapted = adapt_one(entry(json!([effort])));
    let obs = serde_json::to_value(&adapted.observations[0]).unwrap();
    assert_eq!(obs["thinking"]["efforts"], json!(["off", "low"]));
    assert_eq!(obs["thinking"]["canDisable"], json!(true));
    let rejected: Vec<_> = adapted
        .findings
        .rejections
        .iter()
        .map(|d| (d.field.as_str(), d.value.clone()))
        .collect();
    assert_eq!(
        rejected,
        [
            ("thinking.efforts", json!("ultra")),
            ("thinking.efforts", json!("default")),
            ("thinking.efforts", json!(3)),
        ]
    );
    // acceptedParams keeps what the source says the endpoint accepts, verbatim.
    assert_eq!(
        obs["acceptedParams"][0]["values"],
        json!(["none", "low", "ultra", "default", 3])
    );
}

#[test]
fn a_param_outside_the_contract_is_dropped_with_a_diagnostic() {
    let bad_type = param("a", "tensor", "sampling");
    let no_path = json!({"type": "integer"});
    let good = param("b", "integer", "sampling");
    let adapted = adapt_one(entry(json!([bad_type, "oops", no_path, good])));
    let obs = serde_json::to_value(&adapted.observations[0]).unwrap();
    let paths: Vec<_> = obs["acceptedParams"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["path"].clone())
        .collect();
    assert_eq!(paths, [json!("b")]);
    let fields: Vec<_> = adapted
        .findings
        .rejections
        .iter()
        .map(|d| d.field.as_str())
        .collect();
    assert_eq!(
        fields,
        [
            "acceptedParams[a].type",
            "acceptedParams[1]",
            "acceptedParams[2].path"
        ]
    );
}

#[test]
fn bad_members_are_removed_one_by_one() {
    let mut p = param("temperature", "number", "sampling");
    p["values"] = json!("hot");
    p["range"] = json!({"min": "zero", "max": 2, "step": 0.1});
    p["group"] = json!(["sampling"]);
    let adapted = adapt_one(entry(json!([p])));
    let obs = serde_json::to_value(&adapted.observations[0]).unwrap();
    let kept = &obs["acceptedParams"][0];
    assert!(kept.get("values").is_none() && kept.get("group").is_none());
    assert_eq!(kept["range"], json!({"max": 2.0, "step": 0.1}));
    let fields: Vec<_> = adapted
        .findings
        .rejections
        .iter()
        .map(|d| d.field.as_str())
        .collect();
    assert_eq!(
        fields,
        [
            "acceptedParams[temperature].group",
            "acceptedParams[temperature].values",
            "acceptedParams[temperature].range.min",
        ]
    );
}

#[test]
fn unknown_auth_type_status_and_dates_are_rejected() {
    let mut e = entry(json!([]));
    e["authType"] = json!("magic");
    e["status"] = json!("sunsetting");
    e["shutdownOn"] = json!("soon");
    e["replacement"] = json!(7);
    let adapted = adapt_one(e);
    let obs = serde_json::to_value(&adapted.observations[0]).unwrap();
    assert_eq!(obs["authType"], Value::Null);
    assert_eq!(
        obs["lifecycle"],
        json!({"status": "unknown", "replacement": null, "shutdownOn": null})
    );
    let mut fields: Vec<_> = adapted
        .findings
        .rejections
        .iter()
        .map(|d| d.field.as_str())
        .collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        [
            "authType",
            "lifecycle.replacement",
            "lifecycle.shutdownOn",
            "lifecycle.status"
        ]
    );
}

#[test]
fn lifecycle_is_mapped_from_status_replacement_and_shutdown() {
    let mut e = entry(json!([]));
    e["status"] = json!("deprecated");
    e["replacement"] = json!("acme/m2");
    e["shutdownOn"] = json!("2026-10-23");
    let adapted = adapt_one(e);
    let obs = serde_json::to_value(&adapted.observations[0]).unwrap();
    assert_eq!(
        obs["lifecycle"],
        json!({"status": "deprecated", "replacement": "acme/m2", "shutdownOn": "2026-10-23"})
    );
    assert!(adapted.findings.rejections.is_empty());
    for (status, wire) in [("active", "live"), ("retired", "retired")] {
        let mut e = entry(json!([]));
        e["status"] = json!(status);
        let o = serde_json::to_value(&adapt_one(e).observations[0]).unwrap();
        assert_eq!(o["lifecycle"]["status"], json!(wire));
    }
    let none = serde_json::to_value(&adapt_one(entry(json!([]))).observations[0]).unwrap();
    assert_eq!(
        none["lifecycle"]["status"],
        json!("unknown"),
        "no status = unknown, not live"
    );
}

// ---- negative bounds: rejected unless the source documents -1 --------------------------------

#[test]
fn an_undocumented_negative_budget_is_rejected_and_removed() {
    let mut p = param("thinking.budget_tokens", "integer", "reasoning");
    p["range"] = json!({"min": -5, "max": 8000});
    p["default"] = json!(-1);
    let adapted = adapt_one(entry(json!([p])));
    let obs = serde_json::to_value(&adapted.observations[0]).unwrap();
    let kept = &obs["acceptedParams"][0];
    assert_eq!(kept["range"], json!({"max": 8000.0}));
    assert!(kept.get("default").is_none());
    let rejected: Vec<_> = adapted
        .findings
        .rejections
        .iter()
        .map(|d| (d.field.as_str(), d.value.clone()))
        .collect();
    assert_eq!(
        rejected,
        [
            (
                "acceptedParams[thinking.budget_tokens].range.min",
                json!(-5)
            ),
            ("acceptedParams[thinking.budget_tokens].default", json!(-1)),
        ],
        "-1 is rejected too when the description does not document it"
    );
    assert!(adapted.findings.sentinels.is_empty());
}

#[test]
fn a_documented_minus_one_is_kept_and_recorded_as_a_sentinel() {
    let mut p = param("thinking_budget", "integer", "reasoning");
    p["description"] = json!("Thinking tokens; -1 lets the model decide.");
    p["range"] = json!({"min": -1, "max": 24576});
    p["default"] = json!(-1);
    let adapted = adapt_one(entry(json!([p])));
    let obs = serde_json::to_value(&adapted.observations[0]).unwrap();
    assert_eq!(
        obs["acceptedParams"][0]["range"],
        json!({"min": -1.0, "max": 24576.0})
    );
    assert_eq!(obs["acceptedParams"][0]["default"], json!(-1));
    assert!(adapted.findings.rejections.is_empty());
    let s = &adapted.findings.sentinels;
    assert_eq!(s.len(), 2);
    assert_eq!(s[0].field, "acceptedParams[thinking_budget].range.min");
    assert_eq!(s[1].field, "acceptedParams[thinking_budget].default");
    assert!(
        s.iter()
            .all(|x| x.value == json!(-1) && x.source_id == "modelparams.dev")
    );
    assert!(s[0].reason.contains("-1 lets the model decide"));
}

#[test]
fn negative_bounds_are_legitimate_outside_count_parameters() {
    let mut p = param("frequency_penalty", "number", "sampling");
    p["range"] = json!({"min": -2, "max": 2});
    let adapted = adapt_one(entry(json!([p])));
    assert!(adapted.findings.rejections.is_empty() && adapted.findings.sentinels.is_empty());
    let obs = serde_json::to_value(&adapted.observations[0]).unwrap();
    assert_eq!(
        obs["acceptedParams"][0]["range"],
        json!({"min": -2.0, "max": 2.0})
    );
}

#[test]
fn a_negative_length_bound_is_rejected_and_never_becomes_max_tokens() {
    let mut p = param("max_tokens", "integer", "generation_length");
    p["range"] = json!({"min": 1, "max": -4096});
    let adapted = adapt_one(entry(json!([p])));
    assert_eq!(adapted.observations[0].fields.max_tokens, None);
    assert_eq!(adapted.findings.rejections.len(), 1);
    assert_eq!(
        adapted.findings.rejections[0].field,
        "acceptedParams[max_tokens].range.max"
    );
}

// ---- field mapping ----------------------------------------------------------------------------

#[test]
fn max_tokens_comes_from_the_last_length_param_with_a_positive_max() {
    let mut a = param("max_tokens", "integer", "generation_length");
    a["range"] = json!({"min": 1, "max": 4096});
    let mut b = param("max_completion_tokens", "integer", "generation_length");
    b["range"] = json!({"min": 1, "max": 8192.9});
    let mut zero = param("max_output_tokens", "integer", "generation_length");
    zero["range"] = json!({"min": 0, "max": 0});
    let mut unrelated = param("n", "integer", "sampling");
    unrelated["range"] = json!({"max": 99999});
    let adapted = adapt_one(entry(json!([a, b, zero, unrelated])));
    assert_eq!(adapted.observations[0].fields.max_tokens, Some(8192));
    assert!(adapted.findings.rejections.is_empty());
}

#[test]
fn reasoning_and_thinking_follow_the_reasoning_params() {
    let none = adapt_one(entry(json!([param("temperature", "number", "sampling")])));
    let f = &none.observations[0].fields;
    assert_eq!(
        (f.reasoning, f.thinking.is_none()),
        (None, true),
        "no reasoning params = unknown"
    );

    let toggle = adapt_one(entry(json!([param(
        "thinking.type",
        "string",
        "reasoning"
    )])));
    let f = &toggle.observations[0].fields;
    assert_eq!(
        (f.reasoning, f.thinking.is_none()),
        (Some(true), true),
        "no efforts = no thinking"
    );

    let mut e = param("reasoning.effort", "enum", "reasoning");
    e["values"] = json!(["high", "minimal", "xhigh"]);
    let f = serde_json::to_value(&adapt_one(entry(json!([e]))).observations[0]).unwrap();
    assert_eq!(
        f["thinking"],
        json!({
            "mode": "effort", "efforts": ["minimal", "high", "xhigh"], "canDisable": null,
            "minBudgetTokens": null, "maxBudgetTokens": null, "dynamicBudget": null,
        })
    );

    // an enum under another group, or whose path is not an effort, is not an effort list
    let mut other = param("verbosity", "enum", "output_format");
    other["values"] = json!(["low", "high"]);
    let mut keep = param("thinking.keep", "enum", "reasoning");
    keep["values"] = json!(["all", null]);
    keep["default"] = json!(null);
    let adapted = adapt_one(entry(json!([other, keep])));
    assert!(adapted.observations[0].fields.thinking.is_none());
    assert!(adapted.findings.rejections.is_empty());
    let f = serde_json::to_value(&adapted.observations[0]).unwrap();
    assert_eq!(
        f["acceptedParams"][1]["default"],
        Value::Null,
        "explicit null default is kept"
    );
    assert_eq!(f["acceptedParams"][1]["values"], json!(["all", null]));
}

#[test]
fn conditional_raw_and_provenance_follow_the_prototype() {
    let mut p = param("top_k", "integer", "sampling");
    p["applicability"] = json!({"only": {"thinking.type": "disabled"}});
    let mut e = entry(json!([p, param("top_p", "number", "sampling")]));
    e["wireId"] = json!("acme-m1-wire");
    let adapted = adapt_one(e);
    let obs = serde_json::to_value(&adapted.observations[0]).unwrap();
    assert_eq!(obs["acceptedParams"][0]["conditional"], json!(true));
    assert_eq!(obs["acceptedParams"][1]["conditional"], json!(false));
    assert_eq!(
        obs["raw"],
        json!({"apiSurface": "openai-chat-completions", "wireId": "acme-m1-wire"})
    );
    assert_eq!(obs["source"]["kind"], "bundled");
    assert_eq!(obs["source"]["confidence"], "declared");
    assert_eq!(obs["source"]["revision"], "2026-11-24T08:22:46.486Z");
    assert_eq!(obs["source"]["observedAtMs"], FETCHED_AT);
    for unknown in [
        "name",
        "canonicalId",
        "kind",
        "contextWindow",
        "maxInputTokens",
        "cost",
    ] {
        assert_eq!(
            obs[unknown],
            Value::Null,
            "{unknown}: this source does not say"
        );
    }
    assert_eq!(obs["authType"], "api_key");
}

// ---- build ------------------------------------------------------------------------------------

#[test]
fn build_derives_one_single_source_model_per_observation() {
    let bytes = snapshot(json!([entry(json!([param(
        "top_p", "number", "sampling"
    )]))]));
    let input = SourceInput {
        bytes: &bytes,
        fetched_at_ms: FETCHED_AT,
        fetch: None,
    };
    let out = build_modelparams(input, &BuildOptions::default()).unwrap();
    assert_eq!(out.models.len(), 1);
    assert_eq!(out.models[0].key, "acme/m1");
    let provenance = serde_json::to_value(&out.models[0].provenance).unwrap();
    assert_eq!(
        provenance["fields"]["acceptedParams"]["source"],
        "modelparams.dev"
    );
    assert_eq!(
        provenance["fields"]["acceptedParams"]["rule"],
        "single-source"
    );
    assert_eq!(out.report.counts.observations, 1);
    assert_eq!(out.report.source.descriptor.source_id, "modelparams.dev");

    let too_few = build_modelparams(
        input,
        &BuildOptions {
            min_models: Some(2),
        },
    );
    assert!(matches!(
        too_few,
        Err(CatalogError::BelowMinModels { found: 1, min: 2 })
    ));
}

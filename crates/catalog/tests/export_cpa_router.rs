//! `export_cpa_router` over hand-written catalogs: ranking, exclusions, quota, determinism and
//! agreement with `export_cpa`.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tierbridge_catalog::{
    CatalogError,
    export_cpa::{CpaOptions, export_cpa},
    export_cpa_router::{
        Profile, RouterExport, RouterOptions, SCHEMA, SkipReason, export_cpa_router, provider_key,
    },
};

const GW: &str = "omp-live:gw";

fn row(gateway: &str, provider: &str, id: &str, extra: Value) -> Value {
    let mut row = json!({
        "recordType": "model",
        "key": format!("{gateway}:{provider}/{id}"),
        "provider": provider,
        "id": id,
        "name": id,
        "canonicalId": null,
        "kind": "chat",
        "contextWindow": null,
        "maxInputTokens": null,
        "maxTokens": null,
        "cost": {"status": "unknown", "input": null, "output": null, "cacheRead": null, "cacheWrite": null},
        "input": null,
        "output": null,
        "supportsTools": null,
        "thinking": null,
        "lifecycle": null,
        "provenance": {"sources": [], "fields": {}},
    });
    row.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    row
}

fn gw_row(id: &str, extra: Value) -> Value {
    row(GW, "gw", id, extra)
}

fn known(input: f64, output: f64) -> Value {
    json!({"status": "known", "input": input, "output": output, "cacheRead": null, "cacheWrite": null})
}

fn free() -> Value {
    json!({"status": "free", "input": 0.0, "output": 0.0, "cacheRead": null, "cacheWrite": null})
}

fn field(confidence: &str) -> Value {
    json!({"source": "models.dev", "rule": "test", "confidence": confidence, "conflict": false, "alternates": []})
}

/// A row whose `cost` and `contextWindow` carry the given confidences.
fn with_confidence(cost: Value, window: Option<u64>, cost_conf: &str, window_conf: &str) -> Value {
    json!({
        "cost": cost,
        "contextWindow": window,
        "provenance": {"sources": [], "fields": {
            "cost": field(cost_conf),
            "contextWindow": field(window_conf),
        }},
    })
}

fn options(gateway: &str) -> RouterOptions<'_> {
    RouterOptions {
        gateway,
        provider_name: "tb-gw",
        alias_prefix: "tb-",
        alias: "tierbridge/auto",
        profile: Profile::FreeFirst,
        freellmapi_observations: None,
    }
}

fn bytes(rows: &[Value]) -> Vec<u8> {
    serde_json::to_vec(rows).unwrap()
}

fn run(rows: &[Value], opts: &RouterOptions<'_>) -> RouterExport {
    export_cpa_router(&bytes(rows), opts).unwrap()
}

fn ids(export: &RouterExport) -> Vec<&str> {
    export.candidates.iter().map(|c| c.id.as_str()).collect()
}

fn document(export: &RouterExport) -> Value {
    serde_json::from_str(&export.json).unwrap()
}

#[test]
fn free_first_puts_known_free_then_the_cheapest_known_cost_then_unknown_cost() {
    let rows = [
        gw_row("paid-dear", json!({"cost": known(3.0, 15.0)})),
        gw_row("paid-cheap", json!({"cost": known(0.1, 0.4)})),
        gw_row("mystery", json!({})),
        gw_row("free-b", json!({"cost": free()})),
        gw_row("free-a", json!({"cost": free()})),
        // a known cost with a missing price cannot be ranked: it is unknown
        gw_row(
            "incomplete",
            json!({"cost": {"status": "known", "input": 1.0, "output": null}}),
        ),
        // the sentinel price some sources publish is not a price
        gw_row(
            "sentinel",
            json!({"cost": known(-1_000_000.0, -1_000_000.0)}),
        ),
    ];
    let out = run(&rows, &options(GW));
    assert_eq!(
        ids(&out),
        [
            "free-a",
            "free-b",
            "paid-cheap",
            "paid-dear",
            "incomplete",
            "mystery",
            "sentinel"
        ]
    );
    let doc = document(&out);
    let candidates = doc["aliases"][0]["candidates"].as_array().unwrap();
    assert_eq!(candidates[0]["reasons"], json!(["known-free"]));
    assert_eq!(
        candidates[0]["cost"],
        json!({"status": "free", "input": 0.0, "output": 0.0})
    );
    assert_eq!(candidates[2]["reasons"], json!(["cheapest-known-cost"]));
    assert_eq!(
        candidates[2]["cost"],
        json!({"status": "known", "input": 0.1, "output": 0.4})
    );
    for unknown in &candidates[4..] {
        assert_eq!(unknown["reasons"], json!(["unknown-cost"]));
        // what was written is what ranking saw: no half price, no negative price
        assert_eq!(
            unknown["cost"],
            json!({"status": "unknown", "input": null, "output": null})
        );
    }
}

#[test]
fn known_costs_rank_by_the_input_plus_output_sum() {
    let rows = [
        // sums: 2.0, 2.0, 1.5; the input alone would put `skewed` first
        gw_row(
            "skewed",
            json!({"cost": known(0.1, 1.9), "contextWindow": 1000}),
        ),
        gw_row(
            "even",
            json!({"cost": known(1.0, 1.0), "contextWindow": 2000}),
        ),
        gw_row("lowest", json!({"cost": known(1.5, 0.0)})),
    ];
    let out = run(&rows, &options(GW));
    // equal sums tie-break on the rest (confidence is equal here, then the bigger window)
    assert_eq!(ids(&out), ["lowest", "even", "skewed"]);
}

#[test]
fn ties_break_by_confidence_then_context_window_descending_null_last_then_id() {
    let rows = [
        gw_row(
            "f-small",
            with_confidence(free(), Some(8_000), "declared", "declared"),
        ),
        gw_row(
            "f-big",
            with_confidence(free(), Some(128_000), "declared", "declared"),
        ),
        gw_row(
            "a-twin",
            with_confidence(free(), Some(8_000), "declared", "declared"),
        ),
        gw_row(
            "b-twin",
            with_confidence(free(), Some(8_000), "declared", "declared"),
        ),
        // no window: only its cost counts toward confidence, and it sorts after every window
        gw_row(
            "f-null",
            with_confidence(free(), None, "declared", "declared"),
        ),
        // one weak link makes the whole row inferred, however big the window
        gw_row(
            "f-inferred",
            with_confidence(free(), Some(1_000_000), "declared", "inferred"),
        ),
        // a better-evidenced row wins over every window
        gw_row(
            "f-verified",
            with_confidence(free(), Some(1_000), "verified", "verified"),
        ),
        // free but nothing says so in the provenance
        gw_row(
            "f-nothing",
            json!({"cost": free(), "contextWindow": 2_000_000}),
        ),
    ];
    let out = run(&rows, &options(GW));
    assert_eq!(
        ids(&out),
        [
            "f-verified",
            "f-big",
            "a-twin",
            "b-twin",
            "f-small",
            "f-null",
            "f-inferred",
            "f-nothing"
        ]
    );
    let confidence: Vec<_> = out
        .candidates
        .iter()
        .map(|c| serde_json::to_value(c.confidence).unwrap())
        .collect();
    assert_eq!(
        confidence,
        [
            "verified", "declared", "declared", "declared", "declared", "declared", "inferred",
            "unknown"
        ]
    );
}

#[test]
fn unknown_cost_rows_still_tie_break_on_the_window_they_have() {
    let rows = [
        gw_row("u-b", json!({"contextWindow": 1_000})),
        gw_row("u-a", json!({"contextWindow": 1_000})),
        gw_row(
            "u-big",
            with_confidence(Value::Null, Some(500), "unknown", "declared"),
        ),
        gw_row("u-null", json!({})),
    ];
    // `cost: null` in the row is unknown too
    let out = run(&rows, &options(GW));
    // u-big has the only evidenced window (declared), u-a/u-b/u-null have no provenance
    assert_eq!(ids(&out), ["u-big", "u-a", "u-b", "u-null"]);
}

/// The provenance of a limit that holds a placeholder only: no winner, the gateway's filler as
/// the one alternate (`confidence: unknown`).
fn placeholder_only() -> Value {
    json!({"source": "none", "rule": "limits: gateway-live > ref-first-party > ref-majority [none]",
           "confidence": "unknown", "conflict": false,
           "alternates": [{"source": GW, "value": 128000, "confidence": "unknown"}]})
}

#[test]
fn leaves_out_non_chat_retired_and_placeholder_only_rows_and_says_why() {
    let rows = [
        gw_row("chat-ok", json!({"contextWindow": 8_000})),
        gw_row("kind-null", json!({"kind": null})),
        gw_row("embedder", json!({"kind": "embedding"})),
        gw_row(
            "gone",
            json!({"lifecycle": {"status": "retired", "replacement": null, "shutdownOn": null}}),
        ),
        gw_row(
            "deprecated",
            json!({"lifecycle": {"status": "deprecated", "replacement": null, "shutdownOn": null}}),
        ),
        // nothing but the gateway's filler: excluded
        gw_row(
            "filler",
            json!({"provenance": {"sources": [], "fields": {
                "contextWindow": placeholder_only(), "maxTokens": placeholder_only()}}}),
        ),
        // the filler lost to a real value: kept
        gw_row(
            "filler-lost",
            json!({"contextWindow": 400_000, "provenance": {"sources": [], "fields": {
                "contextWindow": {"source": "models.dev", "rule": "x", "confidence": "declared",
                    "conflict": false, "alternates": [{"source": GW, "value": 128000, "confidence": "unknown"}]}}}}),
        ),
        // one limit is a filler but another is real: kept
        gw_row(
            "filler-one-real",
            json!({"maxTokens": 4_096, "provenance": {"sources": [], "fields": {
                "contextWindow": placeholder_only()}}}),
        ),
        // no limits and nothing says they were filler: only unknown, kept
        gw_row("unknown-limits", json!({})),
        // references disagree and none won: a conflict, not a placeholder
        gw_row(
            "refs-disagree",
            json!({"provenance": {"sources": [], "fields": {"contextWindow": {
                "source": "none", "rule": "x [none]", "confidence": "unknown", "conflict": true,
                "alternates": [{"source": "models.dev", "value": 1, "confidence": "inferred"},
                               {"source": "models.dev", "value": 2, "confidence": "inferred"}]}}}}),
        ),
        // a filler next to a reference value that lost: not placeholders only
        gw_row(
            "filler-and-ref",
            json!({"provenance": {"sources": [], "fields": {"contextWindow": {
                "source": "none", "rule": "x [none]", "confidence": "unknown", "conflict": true,
                "alternates": [{"source": GW, "value": 128000, "confidence": "unknown"},
                               {"source": "models.dev", "value": 2, "confidence": "inferred"}]}}}}),
        ),
        // another gateway's rows are not this gateway's business
        row("omp-live:other", "other", "foreign", json!({})),
    ];
    let out = run(&rows, &options(GW));
    let mut kept = ids(&out);
    kept.sort_unstable();
    assert_eq!(
        kept,
        [
            "chat-ok",
            "deprecated",
            "filler-and-ref",
            "filler-lost",
            "filler-one-real",
            "kind-null",
            "refs-disagree",
            "unknown-limits"
        ]
    );
    let mut skipped = out.skipped.clone();
    skipped.sort();
    assert_eq!(
        skipped,
        [
            (format!("{GW}:gw/embedder"), SkipReason::NotChat),
            (format!("{GW}:gw/filler"), SkipReason::PlaceholderLimits),
            (format!("{GW}:gw/gone"), SkipReason::Retired),
        ]
    );
    // skipped rows are nowhere in the file, only counted
    assert!(!out.json.contains("embedder"));
    assert!(!out.json.contains("\"gone\""));
    assert!(!out.json.contains("foreign"));
    let doc = document(&out);
    assert_eq!(
        doc["aliases"][0]["skipped"],
        json!({"not-chat": 1, "placeholder-limits": 1, "retired": 1})
    );
}

#[test]
fn the_first_row_in_key_order_owns_an_alias_and_a_retired_row_does_not() {
    let rows = [
        row(GW, "b", "x", json!({"contextWindow": 2})),
        row(GW, "a", "x", json!({"contextWindow": 1})),
        row(
            GW,
            "a",
            "y",
            json!({"lifecycle": {"status": "retired", "replacement": null, "shutdownOn": null}}),
        ),
        row(GW, "b", "y", json!({"contextWindow": 3})),
    ];
    let out = run(&rows, &options(GW));
    let keys: Vec<_> = out.candidates.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(keys, [format!("{GW}:b/y"), format!("{GW}:a/x")]);
    let mut skipped = out.skipped.clone();
    skipped.sort();
    assert_eq!(
        skipped,
        [
            (format!("{GW}:a/y"), SkipReason::Retired),
            (format!("{GW}:b/x"), SkipReason::DuplicateAlias),
        ]
    );
}

#[test]
fn candidate_fields_are_the_ones_the_plugin_reads() {
    let rows = [gw_row(
        "acme/model-1",
        json!({
            "contextWindow": 262_144,
            "maxTokens": 32_768,
            "supportsTools": true,
            "input": ["text", "image", "text"],
            "cost": known(0.25, 2.0),
        }),
    )];
    let out = run(&rows, &options(GW));
    let doc = document(&out);
    assert_eq!(doc["schema"], SCHEMA);
    assert_eq!(doc["gateway"], GW);
    assert_eq!(doc["aliases"][0]["alias"], "tierbridge/auto");
    assert_eq!(doc["aliases"][0]["profile"], "free-first");
    assert_eq!(
        doc["aliases"][0]["candidates"][0],
        json!({
            "model": "tb-acme/model-1",
            "provider": "openai-compatible-tb-gw",
            "id": "acme/model-1",
            "key": format!("{GW}:gw/acme/model-1"),
            "contextWindow": 262_144,
            "maxTokens": 32_768,
            "supportsTools": true,
            "input": ["image", "text"],
            "cost": {"status": "known", "input": 0.25, "output": 2.0},
            "confidence": "unknown",
            "quota": null,
            "reasons": ["cheapest-known-cost"],
        })
    );
}

#[test]
fn the_file_carries_the_sha256_of_the_exact_catalog_bytes() {
    let catalog = bytes(&[gw_row("one", json!({}))]);
    let out = export_cpa_router(&catalog, &options(GW)).unwrap();
    let doc = document(&out);
    assert_eq!(
        doc["catalogSha256"],
        format!("{:x}", Sha256::digest(&catalog))
    );
    // whitespace the parser does not care about still changes the hash: it is the file's, not the data's
    let mut spaced = catalog.clone();
    spaced.push(b'\n');
    let again = export_cpa_router(&spaced, &options(GW)).unwrap();
    assert_ne!(document(&again)["catalogSha256"], doc["catalogSha256"]);
    assert_eq!(
        document(&again)["catalogSha256"],
        format!("{:x}", Sha256::digest(&spaced))
    );
}

#[test]
fn output_is_byte_identical_across_runs_and_row_order() {
    let rows = vec![
        gw_row(
            "c",
            with_confidence(free(), Some(8_000), "declared", "declared"),
        ),
        gw_row("b", json!({"cost": known(1.0, 2.0)})),
        gw_row("a", json!({})),
        gw_row(
            "d",
            with_confidence(free(), Some(8_000), "declared", "declared"),
        ),
        gw_row("e", json!({"kind": "image"})),
    ];
    let catalog = bytes(&rows);
    let first = export_cpa_router(&catalog, &options(GW)).unwrap();
    let second = export_cpa_router(&catalog, &options(GW)).unwrap();
    assert_eq!(first.json, second.json);
    assert!(first.json.ends_with("}\n"));

    // the same rows in another order rank the same (only the file's hash differs)
    let mut reversed = rows.clone();
    reversed.reverse();
    let other = run(&reversed, &options(GW));
    assert_eq!(first.candidates, other.candidates);
    assert_eq!(ids(&first), ["c", "d", "b", "a"]);
}

#[test]
fn no_rows_for_the_gateway_is_an_error_that_lists_the_gateways_there_are() {
    let rows = [row("omp-live:other", "other", "m", json!({}))];
    let err = export_cpa_router(&bytes(&rows), &options(GW)).unwrap_err();
    assert!(matches!(err, CatalogError::Export(_)));
    let message = err.to_string();
    assert!(
        message.contains("no rows from gateway `omp-live:gw`"),
        "{message}"
    );
    assert!(message.contains("omp-live:other"), "{message}");

    let err = export_cpa_router(b"[]", &options(GW)).unwrap_err();
    assert!(err.to_string().contains("gateways present: none"), "{err}");
}

#[test]
fn a_gateway_whose_rows_are_all_excluded_is_an_empty_list_not_an_error() {
    let rows = [gw_row("emb", json!({"kind": "embedding"}))];
    let out = run(&rows, &options(GW));
    assert!(out.candidates.is_empty());
    assert_eq!(document(&out)["aliases"][0]["candidates"], json!([]));
}

#[test]
fn the_alias_must_be_set_and_must_not_be_a_model_alias() {
    let rows = [gw_row("auto", json!({}))];
    let mut opts = options(GW);
    opts.alias = "  ";
    assert!(export_cpa_router(&bytes(&rows), &opts).is_err());

    opts.alias = "tb-auto";
    let err = export_cpa_router(&bytes(&rows), &opts).unwrap_err();
    assert!(err.to_string().contains("also the model alias"), "{err}");

    opts.alias_prefix = "";
    opts.alias = "auto";
    assert!(export_cpa_router(&bytes(&rows), &opts).is_err());
}

#[test]
fn provider_key_follows_cliproxyapi() {
    assert_eq!(provider_key("tb-gw"), "openai-compatible-tb-gw");
    assert_eq!(provider_key("  TB-GW "), "openai-compatible-tb-gw");
    assert_eq!(
        provider_key("tierbridge-omp-live-easycliproxy"),
        "openai-compatible-tierbridge-omp-live-easycliproxy"
    );
    assert_eq!(provider_key("openai-compatible-x"), "openai-compatible-x");
    assert_eq!(provider_key("OpenAI-Compatibility"), "openai-compatibility");
    assert_eq!(provider_key(" "), "openai-compatibility");
}

fn observation(source: &str, provider: &str, id: &str, kind: &str, free_tier: Value) -> String {
    let mut line = json!({
        "recordType": "observation",
        "source": {"sourceId": source, "kind": "bundled"},
        "provider": provider,
        "id": id,
        "kind": kind,
    });
    if !free_tier.is_null() {
        line["raw"] = json!({"freeTier": free_tier});
    }
    line.to_string()
}

fn limits() -> Value {
    json!({"rpm": 30, "rpd": 14_400, "tpm": 6_000, "tpd": 500_000, "monthlyTokenBudget": "~30M"})
}

fn freellmapi_row(provider: &str, id: &str, canonical: &str) -> Value {
    row(
        "freellmapi",
        provider,
        id,
        json!({"canonicalId": canonical, "contextWindow": 131_072}),
    )
}

fn quota_catalog() -> Vec<Value> {
    vec![
        // freellmapi's own rows, as the merge makes them (they carry no limits)
        freellmapi_row("groq", "llama-70b", "meta/llama-3.3-70b"),
        freellmapi_row("nvidia", "meta/llama-70b", "meta/llama-3.3-70b"),
        freellmapi_row("google", "gemini-2.5-flash", "google/gemini-2.5-flash"),
        freellmapi_row("cerebras", "nolimits", "x/nolimits"),
        freellmapi_row("emb", "embedder", "y/embedder"),
        // the gateway under test
        gw_row(
            "llama-70b-instruct",
            json!({"canonicalId": "meta/llama-3.3-70b"}),
        ),
        row(
            GW,
            "google",
            "gemini-2.5-flash",
            json!({"canonicalId": "google/gemini-2.5-flash"}),
        ),
        gw_row("nolimits-twin", json!({"canonicalId": "x/nolimits"})),
        gw_row("embedder-twin", json!({"canonicalId": "y/embedder"})),
        // exact platform/id with no limits stops the search, canonical id or not
        row(
            GW,
            "cerebras",
            "nolimits",
            json!({"canonicalId": "meta/llama-3.3-70b"}),
        ),
        gw_row("other-source", json!({})),
        gw_row("plain", json!({})),
    ]
}

fn quota_observations() -> String {
    [
        observation("freellmapi", "groq", "llama-70b", "chat", limits()),
        observation(
            "freellmapi",
            "nvidia",
            "meta/llama-70b",
            "chat",
            json!({"rpm": 40}),
        ),
        observation(
            "freellmapi",
            "google",
            "gemini-2.5-flash",
            "chat",
            json!({"rpd": 250}),
        ),
        // chat row that states no limit
        observation("freellmapi", "cerebras", "nolimits", "chat", Value::Null),
        // embeddings are not routed to
        observation("freellmapi", "emb", "embedder", "embedding", limits()),
        // a line of another source is not freellmapi's, even with the same platform and id
        observation("models.dev", "gw", "other-source", "chat", limits()),
        String::new(),
    ]
    .join("\n")
}

fn quota_run(observations: Option<&[u8]>, gateway: &str) -> RouterExport {
    let mut opts = options(gateway);
    opts.freellmapi_observations = observations;
    run(&quota_catalog(), &opts)
}

fn quota_of<'a>(
    export: &'a RouterExport,
    id: &str,
) -> Option<&'a tierbridge_catalog::export_cpa_router::Quota> {
    export
        .candidates
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("no candidate {id}"))
        .quota
        .as_ref()
}

#[test]
fn quota_exists_only_for_rows_a_freellmapi_observation_matches() {
    let observations = quota_observations();
    let out = quota_run(Some(observations.as_bytes()), GW);

    // canonical id; two platforms qualify, the first by `<provider>/<id>` is used and counted
    let q = quota_of(&out, "llama-70b-instruct").expect("canonical match");
    assert_eq!(q.matched_by, "canonicalId");
    assert_eq!(q.matches, 2);
    assert_eq!(q.source, "freellmapi:groq/llama-70b");
    assert_eq!(
        (q.rpm, q.rpd, q.tpm, q.tpd),
        (Some(30), Some(14_400), Some(6_000), Some(500_000))
    );
    assert_eq!(q.monthly_token_budget.as_deref(), Some("~30M"));

    // same platform and id: the strongest match
    let google = quota_of(&out, "gemini-2.5-flash").expect("provider-id match");
    assert_eq!(google.matched_by, "provider-id");
    assert_eq!(google.matches, 1);
    assert_eq!(
        (google.rpm, google.rpd, google.tpm, google.tpd),
        (None, Some(250), None, None)
    );
    assert_eq!(google.monthly_token_budget, None);

    // the rest: no quota (no limits, embedding, another source, nothing at all)
    for id in [
        "nolimits-twin",
        "embedder-twin",
        "nolimits",
        "other-source",
        "plain",
    ] {
        assert!(quota_of(&out, id).is_none(), "{id}");
    }

    let with: Vec<_> = out
        .candidates
        .iter()
        .filter(|c| c.reasons.contains(&"freellmapi-limits"))
        .map(|c| c.quota.is_some())
        .collect();
    assert_eq!(with, [true, true]);
    assert_eq!(
        out.candidates.iter().filter(|c| c.quota.is_some()).count(),
        2
    );
}

#[test]
fn an_equal_id_alone_is_not_a_match() {
    let rows = [
        freellmapi_row("google", "gemini-2.5-flash", "google/gemini-2.5-flash"),
        // same id as the freellmapi platform row, but another provider and no shared canonical id
        gw_row("gemini-2.5-flash", json!({})),
    ];
    let observations = observation("freellmapi", "google", "gemini-2.5-flash", "chat", limits());
    let mut opts = options(GW);
    opts.freellmapi_observations = Some(observations.as_bytes());
    let out = run(&rows, &opts);
    assert_eq!(out.candidates.len(), 1);
    assert!(out.candidates[0].quota.is_none());
}

#[test]
fn without_observations_every_quota_is_null() {
    let out = quota_run(None, GW);
    assert!(out.candidates.iter().all(|c| c.quota.is_none()));
    assert!(
        out.candidates
            .iter()
            .all(|c| !c.reasons.contains(&"freellmapi-limits"))
    );
    assert_eq!(document(&out)["quotaSourceSha256"], Value::Null);
}

#[test]
fn the_observations_hash_is_recorded_and_the_quota_serialises_as_the_plugin_reads_it() {
    let observations = quota_observations();
    let out = quota_run(Some(observations.as_bytes()), GW);
    let doc = document(&out);
    assert_eq!(
        doc["quotaSourceSha256"],
        format!("{:x}", Sha256::digest(observations.as_bytes()))
    );
    let candidates = doc["aliases"][0]["candidates"].as_array().unwrap();
    let llama = candidates
        .iter()
        .find(|c| c["id"] == "llama-70b-instruct")
        .unwrap();
    assert_eq!(
        llama["quota"],
        json!({
            "rpm": 30, "rpd": 14_400, "tpm": 6_000, "tpd": 500_000,
            "monthlyTokenBudget": "~30M",
            "source": "freellmapi:groq/llama-70b",
            "matchedBy": "canonicalId",
            "matches": 2,
        })
    );
    assert_eq!(
        llama["reasons"],
        json!(["unknown-cost", "freellmapi-limits"])
    );
    let plain = candidates.iter().find(|c| c["id"] == "plain").unwrap();
    assert_eq!(plain["quota"], Value::Null);
}

#[test]
fn when_the_gateway_is_freellmapi_its_own_observations_match_exactly() {
    let observations = quota_observations();
    let out = quota_run(Some(observations.as_bytes()), "freellmapi");
    let groq = quota_of(&out, "llama-70b").expect("own observation");
    assert_eq!(groq.matched_by, "provider-id");
    assert_eq!(groq.source, "freellmapi:groq/llama-70b");
    assert_eq!(groq.rpm, Some(30));
    // the platform that states no limit has none, not another platform's
    assert!(quota_of(&out, "nolimits").is_none());
    // an observation of kind embedding is ignored, whatever limits it states
    assert!(quota_of(&out, "embedder").is_none());
}

#[test]
fn a_bad_observation_line_names_its_line_number() {
    let observations = format!(
        "{}\n{{not json\n",
        observation("freellmapi", "a", "b", "chat", limits())
    );
    let mut opts = options(GW);
    opts.freellmapi_observations = Some(observations.as_bytes());
    let err = export_cpa_router(&bytes(&quota_catalog()), &opts).unwrap_err();
    assert!(matches!(err, CatalogError::Export(_)));
    assert!(err.to_string().contains("line 2"), "{err}");
}

#[test]
fn every_candidate_is_an_alias_export_cpa_registers_under_the_same_name_and_prefix() {
    let rows = vec![
        gw_row("plain", json!({"contextWindow": 8_000})),
        gw_row("acme/with-slash", json!({"cost": free()})),
        gw_row("emb", json!({"kind": "embedding"})),
        gw_row(
            "gone",
            json!({"lifecycle": {"status": "retired", "replacement": null, "shutdownOn": null}}),
        ),
        gw_row(
            "filler",
            json!({"provenance": {"sources": [], "fields": {"contextWindow": placeholder_only()}}}),
        ),
        row(
            GW,
            "other-provider",
            "plain",
            json!({"cost": known(1.0, 2.0)}),
        ),
        gw_row("UPPER", json!({"cost": known(0.5, 0.5)})),
        row("omp-live:elsewhere", "e", "foreign", json!({})),
    ];
    let catalog = bytes(&rows);
    for (name, prefix) in [
        ("TB-GW", "tb-"),
        ("tierbridge-omp-live-gw", ""),
        ("x", "pre/"),
    ] {
        let mut opts = options(GW);
        opts.provider_name = name;
        opts.alias_prefix = prefix;
        let router = export_cpa_router(&catalog, &opts).unwrap();
        let cpa = export_cpa(
            &catalog,
            &CpaOptions {
                gateway: GW,
                provider_name: name,
                base_url: "http://127.0.0.1:8317/v1",
                api_key: None,
                alias_prefix: prefix,
                model_prefix: None,
            },
        )
        .unwrap();
        let aliases: std::collections::BTreeSet<&str> =
            cpa.models.iter().map(|m| m.alias.as_str()).collect();
        assert!(!router.candidates.is_empty());
        for candidate in &router.candidates {
            assert!(
                aliases.contains(candidate.model.as_str()),
                "{} is not an alias of export cpa ({name:?}, {prefix:?})",
                candidate.model
            );
            assert_eq!(
                candidate.provider,
                format!("openai-compatible-{}", name.to_lowercase())
            );
        }
        // the router only ever drops more than export cpa does
        assert!(router.candidates.len() <= cpa.models.len());
        let mut models: Vec<&str> = router.candidates.iter().map(|c| c.model.as_str()).collect();
        models.sort_unstable();
        models.dedup();
        assert_eq!(
            models.len(),
            router.candidates.len(),
            "one candidate per alias"
        );
    }
}

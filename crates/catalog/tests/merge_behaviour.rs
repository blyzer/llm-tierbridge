//! Precedence rules of the merge on small synthetic sources (`docs/catalog-proof.md` §9):
//! the gateway row defines the model; placeholders never win; first party before a strict
//! majority, never first seen; a gateway's zero is never free; overrides win as `configured`.

use std::collections::BTreeMap;

use tierbridge_catalog::model::{
    AcceptedParam, AuthType, Confidence, Cost, CostStatus, Effort, FieldName, Fields, Lifecycle,
    LifecycleStatus, Model, Observation, ParamType, ProvenanceKind, ProvenanceSource, Thinking,
    ThinkingMode,
};
use tierbridge_catalog::overrides::Overrides;
use tierbridge_catalog::{
    BuildOutput, Counts, Diagnostic, MergeOutput, Report, ReportSource, merge_catalog,
    merge_catalog_with,
};

const GW: &str = "cliproxyapi-cache";

fn descriptor(source_id: &str) -> ProvenanceSource {
    ProvenanceSource {
        source_id: source_id.to_owned(),
        kind: ProvenanceKind::Discovered,
        origin: "test".to_owned(),
        revision: None,
        confidence: Confidence::Declared,
        observed_at_ms: None,
    }
}

fn source(source_id: &str, rows: Vec<Fields>) -> BuildOutput {
    let observations: Vec<Observation> = rows
        .into_iter()
        .map(|f| Observation::new(descriptor(source_id), f))
        .collect();
    BuildOutput {
        report: Report {
            source: ReportSource {
                descriptor: descriptor(source_id),
                sha256: "0".repeat(64),
                bytes: 0,
                fetched_at: String::new(),
                fetched_at_ms: 0,
                http: None,
            },
            counts: Counts {
                providers: 0,
                observations: observations.len(),
                models: observations.len(),
                rejections: 0,
                sentinels: 0,
                sentinels_by_field: BTreeMap::new(),
                zero_limits_mapped_to_unknown: 0,
            },
            rejections: Vec::new(),
            sentinels: Vec::new(),
            unmodelled: Vec::new(),
        },
        models: Vec::new(),
        observations,
    }
}

fn limits(provider: &str, id: &str, ctx: Option<u64>, max: Option<u64>) -> Fields {
    let mut f = Fields::empty(provider, id);
    f.context_window = ctx;
    f.max_tokens = max;
    f
}

/// Gateway rows: `row` plus enough distinct fillers that no limit pair is a placeholder.
fn gateway(row: Fields) -> BuildOutput {
    let mut rows = vec![row];
    rows.extend((0..9u64).map(|i| {
        limits(
            "cliproxyapi",
            &format!("filler-{i}"),
            Some(1000 + i),
            Some(100 + i),
        )
    }));
    source(GW, rows)
}

/// A gateway where `row` and four fillers share one `(ctx, max)` pair: that pair is a placeholder.
fn placeholder_gateway(row: Fields) -> BuildOutput {
    let mut rows = vec![row];
    rows.extend((0..4).map(|i| {
        limits(
            "cliproxyapi",
            &format!("same-{i}"),
            Some(128_000),
            Some(32_768),
        )
    }));
    rows.extend((0..5u64).map(|i| {
        limits(
            "cliproxyapi",
            &format!("filler-{i}"),
            Some(1000 + i),
            Some(100 + i),
        )
    }));
    source(GW, rows)
}

/// The entry is its model's first-party listing: its provider is the vendor in its canonical id.
fn first_party(mut f: Fields) -> Fields {
    f.canonical_id = Some(format!("{}/{}", f.provider, f.id));
    f
}

fn merge(outs: &[&BuildOutput]) -> MergeOutput {
    merge_catalog(outs).unwrap()
}

fn find<'a>(m: &'a MergeOutput, id: &str) -> &'a Model {
    m.models
        .iter()
        .find(|x| x.fields.id == id)
        .unwrap_or_else(|| panic!("no model {id}"))
}

fn prov(m: &Model, f: FieldName) -> &tierbridge_catalog::model::FieldProvenance {
    &m.provenance.fields[&f]
}

#[test]
fn a_non_placeholder_gateway_value_wins_and_a_disagreeing_reference_is_a_conflict() {
    let gw = gateway(limits(
        "cliproxyapi",
        "claude-x",
        Some(200_000),
        Some(8_000),
    ));
    let md = source(
        "models.dev",
        vec![first_party(limits(
            "anthropic",
            "claude-x",
            Some(1_000_000),
            Some(8_000),
        ))],
    );
    let out = merge(&[&gw, &md]);
    let m = find(&out, "claude-x");
    assert_eq!(m.key, "cliproxyapi-cache:cliproxyapi/claude-x");
    assert_eq!(m.fields.context_window, Some(200_000));
    let p = prov(m, FieldName::ContextWindow);
    assert_eq!(p.source, GW);
    assert!(p.rule.ends_with("[gateway-live]"), "{}", p.rule);
    assert_eq!(p.conflict, Some(true));
    assert_eq!(p.alternates.len(), 1);
    assert_eq!(p.alternates[0].source, "models.dev");
    assert_eq!(p.alternates[0].value, 1_000_000);
    // maxTokens agrees: no conflict, no alternate
    let p = prov(m, FieldName::MaxTokens);
    assert_eq!((p.conflict, p.alternates.len()), (Some(false), 0));
    assert_eq!(m.references.len(), 1);
    assert_eq!(m.references[0].first_party, Some(true));
}

#[test]
fn a_placeholder_never_wins_it_goes_to_alternates_and_the_first_party_value_wins() {
    let gw = placeholder_gateway(limits(
        "cliproxyapi",
        "claude-x",
        Some(128_000),
        Some(32_768),
    ));
    let md = source(
        "models.dev",
        vec![first_party(limits(
            "anthropic",
            "claude-x",
            Some(200_000),
            Some(64_000),
        ))],
    );
    let out = merge(&[&gw, &md]);
    let m = find(&out, "claude-x");
    assert_eq!(m.fields.context_window, Some(200_000));
    assert_eq!(m.fields.max_tokens, Some(64_000));
    let p = prov(m, FieldName::ContextWindow);
    assert_eq!(p.source, "models.dev");
    assert!(p.rule.ends_with("[ref-first-party]"), "{}", p.rule);
    assert_eq!(p.confidence, Confidence::Declared);
    assert_eq!(
        p.conflict,
        Some(false),
        "a placeholder is not evidence, so it is no conflict"
    );
    assert_eq!(p.alternates.len(), 1);
    assert_eq!(p.alternates[0].source, GW);
    assert_eq!(p.alternates[0].value, 128_000);
    assert_eq!(p.alternates[0].confidence, Some(Confidence::Unknown));
    assert_eq!(out.report.placeholder_winners.total, 0);
    assert_eq!(
        out.report.gateways[0].placeholder_rows[&FieldName::ContextWindow],
        5
    );
}

#[test]
fn a_placeholder_without_any_reference_leaves_the_field_null_with_the_placeholder_as_alternate() {
    let gw = placeholder_gateway(limits(
        "cliproxyapi",
        "unknown-model",
        Some(128_000),
        Some(32_768),
    ));
    let md = source(
        "models.dev",
        vec![first_party(limits("anthropic", "other", Some(1), Some(1)))],
    );
    let out = merge(&[&gw, &md]);
    let m = find(&out, "unknown-model");
    assert_eq!(m.fields.context_window, None);
    let p = prov(m, FieldName::ContextWindow);
    assert_eq!(p.source, "none");
    assert_eq!(p.confidence, Confidence::Unknown);
    assert!(p.rule.ends_with("[none]"));
    assert_eq!(p.alternates.len(), 1);
    assert_eq!(out.report.fields[&FieldName::ContextWindow].unresolved, 5);
}

#[test]
fn a_strict_majority_of_resellers_wins_a_tie_and_two_disagreeing_first_parties_do_not() {
    // three resellers 100/100/300: strict majority -> 100, inferred
    let gw = placeholder_gateway(limits("cliproxyapi", "resold", Some(128_000), Some(32_768)));
    let md = source(
        "models.dev",
        vec![
            limits("host-a", "resold", Some(100_000), None),
            limits("host-b", "resold", Some(100_000), None),
            limits("host-c", "resold", Some(300_000), None),
        ],
    );
    let m = merge(&[&gw, &md]);
    let m = find(&m, "resold");
    assert_eq!(m.fields.context_window, Some(100_000));
    let p = prov(m, FieldName::ContextWindow);
    assert!(p.rule.ends_with("[ref-majority]"), "{}", p.rule);
    assert_eq!(p.confidence, Confidence::Inferred);
    assert_eq!(
        p.conflict,
        Some(false),
        "the outvoted reseller is an alternate, not a conflict"
    );
    assert_eq!(
        p.alternates.iter().filter(|a| a.value == 300_000).count(),
        1
    );

    // 1 vs 1: nobody has a strict majority -> null, never "first seen"
    let md = source(
        "models.dev",
        vec![
            limits("host-a", "resold", Some(100_000), None),
            limits("host-b", "resold", Some(300_000), None),
        ],
    );
    let gw = placeholder_gateway(limits("cliproxyapi", "resold", Some(128_000), Some(32_768)));
    let out = merge(&[&gw, &md]);
    let m = find(&out, "resold");
    assert_eq!(m.fields.context_window, None);
    let p = prov(m, FieldName::ContextWindow);
    assert_eq!(p.conflict, Some(true));
    assert_eq!(
        p.alternates.len(),
        3,
        "both reseller values and the placeholder are kept"
    );

    // a first party beats any number of resellers
    let md = source(
        "models.dev",
        vec![
            limits("host-a", "claude-x", Some(100_000), None),
            limits("host-b", "claude-x", Some(100_000), None),
            first_party(limits("anthropic", "claude-x", Some(200_000), None)),
        ],
    );
    let gw = placeholder_gateway(limits(
        "cliproxyapi",
        "claude-x",
        Some(128_000),
        Some(32_768),
    ));
    let out = merge(&[&gw, &md]);
    assert_eq!(find(&out, "claude-x").fields.context_window, Some(200_000));

    // two first parties that disagree: ambiguous
    let md = source(
        "models.dev",
        vec![
            first_party(limits("anthropic", "claude-x", Some(200_000), None)),
            first_party(limits("anthropic", "claude-x", Some(500_000), None)),
        ],
    );
    let gw = placeholder_gateway(limits(
        "cliproxyapi",
        "claude-x",
        Some(128_000),
        Some(32_768),
    ));
    let out = merge(&[&gw, &md]);
    assert_eq!(find(&out, "claude-x").fields.context_window, None);
}

fn cost(status: CostStatus, input: Option<f64>, output: Option<f64>) -> Cost {
    Cost {
        status,
        input,
        output,
        cache_read: None,
        cache_write: None,
        tiers: Vec::new(),
    }
}

#[test]
fn cost_is_never_zero_from_a_gateway_and_unknown_is_explicit() {
    let mut known = limits("cliproxyapi", "priced", Some(10), Some(5));
    known.cost = Some(cost(CostStatus::Known, Some(3.0), Some(15.0)));
    let mut zero = limits("cliproxyapi", "zero-priced", Some(11), Some(6));
    zero.cost = Some(cost(CostStatus::Free, Some(0.0), Some(0.0))); // would be a gateway's stand-in 0
    let nothing = limits("cliproxyapi", "no-price", Some(12), Some(7));
    let mut unknown = limits("cliproxyapi", "unknown-price", Some(13), Some(8));
    unknown.cost = Some(cost(CostStatus::Unknown, None, None));
    let mut gw = gateway(known);
    gw.observations
        .extend([zero, nothing, unknown].map(|f| Observation::new(descriptor(GW), f)));
    let mut free_ref = limits("openai", "zero-priced", None, None);
    free_ref.cost = Some(cost(CostStatus::Free, Some(0.0), Some(0.0)));
    let mut paid_ref = limits("openai", "unknown-price", None, None);
    paid_ref.cost = Some(cost(CostStatus::Known, Some(1.0), Some(2.0)));
    let md = source("models.dev", vec![free_ref, paid_ref]);
    let out = merge(&[&gw, &md]);

    let m = find(&out, "priced");
    assert_eq!(m.fields.cost.as_ref().unwrap().status, CostStatus::Known);
    assert_eq!(prov(m, FieldName::Cost).source, GW);

    // a gateway's free/zero is not evidence: the (real, published) reference value decides
    let m = find(&out, "zero-priced");
    let c = m.fields.cost.as_ref().unwrap();
    assert_eq!(prov(m, FieldName::Cost).source, "models.dev");
    assert_eq!(c.status, CostStatus::Free);
    let m = find(&out, "unknown-price");
    assert_eq!(m.fields.cost.as_ref().unwrap().input, Some(1.0));

    // nothing anywhere: explicit unknown, not null and not 0
    let m = find(&out, "no-price");
    let c = m.fields.cost.as_ref().expect("cost is always present");
    assert_eq!(
        (c.status, c.input, c.output),
        (CostStatus::Unknown, None, None)
    );
    assert_eq!(prov(m, FieldName::Cost).source, "none");
    assert_eq!(out.report.cost.gateway_zero_winners, 0);
    assert!(out.report.cost.unknown >= 1);
}

fn lifecycle(status: LifecycleStatus) -> Option<Lifecycle> {
    Some(Lifecycle {
        status,
        replacement: Some("next".into()),
        shutdown_on: None,
    })
}

#[test]
fn a_reference_can_only_degrade_live_and_a_channel_retirement_creates_no_row() {
    let mut retired_row = limits("cliproxyapi", "channel-dropped", Some(1), Some(1));
    retired_row.lifecycle = lifecycle(LifecycleStatus::Retired);
    let mut gw = gateway(limits("cliproxyapi", "old-model", Some(2), Some(2)));
    gw.observations.extend([
        Observation::new(
            descriptor(GW),
            limits("cliproxyapi", "plain", Some(3), Some(3)),
        ),
        Observation::new(descriptor(GW), retired_row),
    ]);
    let mut old = limits("openai", "old-model", None, None);
    old.lifecycle = lifecycle(LifecycleStatus::Deprecated);
    let mut live = limits("openai", "plain", None, None);
    live.lifecycle = lifecycle(LifecycleStatus::Live);
    let mp = source("modelparams.dev", vec![old]);
    let md = source("models.dev", vec![live]);
    let out = merge(&[&gw, &md, &mp]);

    let m = find(&out, "old-model");
    let l = m.fields.lifecycle.as_ref().unwrap();
    assert_eq!(
        (l.status, l.replacement.as_deref()),
        (LifecycleStatus::Deprecated, Some("next"))
    );
    assert_eq!(prov(m, FieldName::Lifecycle).source, "modelparams.dev");
    let m = find(&out, "plain");
    assert_eq!(
        m.fields.lifecycle.as_ref().unwrap().status,
        LifecycleStatus::Live
    );
    assert!(
        prov(m, FieldName::Lifecycle)
            .rule
            .ends_with("[gateway-listed]")
    );
    assert!(out.models.iter().all(|m| m.fields.id != "channel-dropped"));
    assert_eq!(out.report.gateways[0].retired_skipped, 1);
}

#[test]
fn range_max_fills_max_tokens_only_when_models_dev_has_no_value() {
    let gw = placeholder_gateway(limits("cliproxyapi", "both", Some(128_000), Some(32_768)));
    let mut gw = gw;
    gw.observations.push(Observation::new(
        descriptor(GW),
        limits("cliproxyapi", "only-params", Some(5), None),
    ));
    let md = source(
        "models.dev",
        vec![
            limits("openai", "both", None, Some(16_000)),
            limits("openai", "only-params", None, None),
        ],
    );
    let mp = source(
        "modelparams.dev",
        vec![
            limits("openai", "both", None, Some(99_999)),
            limits("openai", "only-params", None, Some(65_536)),
        ],
    );
    let out = merge(&[&gw, &md, &mp]);
    let m = find(&out, "both");
    assert_eq!(m.fields.max_tokens, Some(16_000), "models.dev first");
    assert_eq!(
        prov(m, FieldName::MaxTokens).conflict,
        Some(true),
        "range.max disagrees: evidence"
    );
    let m = find(&out, "only-params");
    assert_eq!(m.fields.max_tokens, Some(65_536));
    let p = prov(m, FieldName::MaxTokens);
    assert!(p.rule.ends_with("[range-max]"), "{}", p.rule);
    assert_eq!(p.confidence, Confidence::Inferred);
    // study: both -> 99_999 vs 16_000 (greater); only-params has no models.dev output
    let s = out.report.max_tokens_study.unwrap();
    assert_eq!(
        (
            s.modelparams_with_max_tokens,
            s.comparable,
            s.range_max_greater
        ),
        (2, 1, 1)
    );
    assert_eq!(s.models_dev_has_no_output, 1);
}

fn param(path: &str) -> Vec<AcceptedParam> {
    vec![AcceptedParam {
        path: path.to_owned(),
        param_type: ParamType::Number,
        group: None,
        values: None,
        range: None,
        default: None,
        conditional: None,
    }]
}

#[test]
fn modelparams_api_key_variant_is_used_and_the_other_auth_type_is_counted() {
    let gw = gateway(limits("cliproxyapi", "gpt-x", Some(1), Some(1)));
    let mut api = first_party(limits("openai", "gpt-x", None, None));
    api.auth_type = Some(AuthType::ApiKey);
    api.accepted_params = Some(param("temperature"));
    let mut sub = first_party(limits("openai", "gpt-x", None, None));
    sub.auth_type = Some(AuthType::Subscription);
    sub.accepted_params = Some(param("subscription_only"));
    let mp = source("modelparams.dev", vec![sub, api]);
    let out = merge(&[&gw, &mp]);
    let m = find(&out, "gpt-x");
    assert_eq!(
        m.fields.accepted_params.as_ref().unwrap()[0].path,
        "temperature"
    );
    assert_eq!(m.references.len(), 1, "one reference per provider/id");
    assert_eq!(
        prov(m, FieldName::AcceptedParams).confidence,
        Confidence::Declared
    );
    assert_eq!(out.report.reference_auth_variants_skipped, 1);
}

fn thinking(efforts: &[Effort]) -> Option<Thinking> {
    Some(Thinking {
        mode: ThinkingMode::Effort,
        efforts: efforts.to_vec(),
        options: None,
        default_level: None,
        effort_map: None,
    })
}

#[test]
fn a_placeholder_effort_ladder_never_wins_and_the_first_party_ladder_does() {
    let ladder = [Effort::Low, Effort::Medium, Effort::High];
    let mut rows = Vec::new();
    for i in 0..5u64 {
        let mut f = limits("cliproxyapi", &format!("m{i}"), Some(10 + i), Some(1 + i));
        f.thinking = thinking(&ladder);
        rows.push(f);
    }
    let gw = source(GW, rows);
    let mut r = first_party(limits("anthropic", "m0", None, None));
    r.thinking = thinking(&[Effort::Off, Effort::High]);
    let md = source("models.dev", vec![r]);
    let out = merge(&[&gw, &md]);
    let m = find(&out, "m0");
    assert_eq!(
        m.fields.thinking.as_ref().unwrap().efforts,
        vec![Effort::Off, Effort::High]
    );
    assert!(
        prov(m, FieldName::Thinking)
            .rule
            .ends_with("[ref-first-party]")
    );
    let m = find(&out, "m1");
    assert!(
        m.fields.thinking.is_none(),
        "placeholder ladder, no reference: unknown"
    );
    assert_eq!(prov(m, FieldName::Thinking).source, "none");
    assert_eq!(out.report.placeholder_winners.total, 0);
}

#[test]
fn rows_of_one_provider_and_id_under_several_auth_types_get_distinct_keys() {
    let mut a = limits("dsh", "m", Some(1), Some(1));
    a.auth_type = Some(AuthType::ApiKey);
    let mut b = limits("dsh", "m", Some(2), Some(2));
    b.auth_type = Some(AuthType::Oauth);
    let gw = source("dsh", vec![a, b, limits("dsh", "single", Some(3), Some(3))]);
    let out = merge(&[&gw]);
    let keys: Vec<&str> = out.models.iter().map(|m| m.key.as_str()).collect();
    assert_eq!(
        keys,
        ["dsh:dsh/m#api_key", "dsh:dsh/m#oauth", "dsh:dsh/single"]
    );
}

#[test]
fn the_probe_source_is_never_used_and_no_gateway_is_an_error() {
    let gw = gateway(limits("cliproxyapi", "claude-x", None, None));
    let bundled = source(
        "omp-bundled",
        vec![limits("openrouter", "claude-x", Some(777), Some(7))],
    );
    let out = merge(&[&gw, &bundled]);
    let m = find(&out, "claude-x");
    assert_eq!(m.fields.context_window, None);
    assert!(m.references.is_empty());
    assert_eq!(out.report.sources.len(), 2);
    assert!(merge_catalog(&[&bundled]).is_err());
}

#[test]
fn an_override_wins_as_configured_and_keeps_what_it_replaced() {
    let gw = gateway(limits(
        "cliproxyapi",
        "claude-x",
        Some(200_000),
        Some(8_000),
    ));
    let md = source(
        "models.dev",
        vec![first_party(limits(
            "anthropic",
            "claude-x",
            Some(1_000_000),
            Some(8_000),
        ))],
    );
    let ov = Overrides::parse(
        br#"{"cliproxyapi-cache:cliproxyapi/claude-x": {"contextWindow": 150000, "name": "Pinned",
              "cost": {"status": "known", "input": 2, "output": 4}},
             "cliproxyapi-cache:cliproxyapi/typo": {"maxTokens": 1}}"#,
        "overrides.json",
    )
    .unwrap();
    let out = merge_catalog_with(&[&gw, &md], Some(&ov)).unwrap();
    let m = find(&out, "claude-x");
    assert_eq!(m.fields.context_window, Some(150_000));
    assert_eq!(m.fields.name.as_deref(), Some("Pinned"));
    assert_eq!(m.fields.cost.as_ref().unwrap().input, Some(2.0));
    let p = prov(m, FieldName::ContextWindow);
    assert_eq!(
        (p.rule.as_str(), p.source.as_str()),
        ("configured", "overrides")
    );
    assert_eq!(p.conflict, Some(true));
    let alt: Vec<u64> = p
        .alternates
        .iter()
        .map(|a| a.value.as_u64().unwrap())
        .collect();
    assert!(
        alt.contains(&200_000) && alt.contains(&1_000_000),
        "{alt:?}"
    );
    // a field the override does not set is merged as usual
    assert!(
        prov(m, FieldName::MaxTokens)
            .rule
            .ends_with("[gateway-live]")
    );
    let src = m
        .provenance
        .sources
        .iter()
        .find(|s| s.source_id == "overrides")
        .unwrap();
    assert_eq!(src.kind, ProvenanceKind::Configured);
    assert!(src.origin.starts_with("overrides.json#sha256="));
    let r = out.report.overrides.as_ref().unwrap();
    assert_eq!((r.entries, r.models_overridden), (2, 1));
    assert_eq!(
        out.report.fields[&FieldName::ContextWindow].winner_rules["configured"],
        1
    );
    // an override whose key matches nothing is reported, not ignored
    let d: Vec<_> = out
        .report
        .diagnostics
        .iter()
        .filter(|d| d.kind == "unmatched-override")
        .collect();
    assert_eq!(d.len(), 1);
    assert_eq!(
        d[0].key.as_deref(),
        Some("cliproxyapi-cache:cliproxyapi/typo")
    );
}

fn diag(
    source_id: &str,
    provider: &str,
    id: &str,
    field: &str,
    value: serde_json::Value,
) -> Diagnostic {
    Diagnostic {
        source_id: source_id.to_owned(),
        provider: provider.to_owned(),
        id: id.to_owned(),
        field: field.to_owned(),
        value,
        reason: "test".to_owned(),
    }
}

#[test]
fn prices_the_contract_cannot_carry_are_reported_per_row_when_the_reference_price_wins() {
    let mut paid = first_party(limits("openai", "reasoner", None, None));
    paid.cost = Some(cost(CostStatus::Known, Some(1.0), Some(2.0)));
    let mut priced_by_gateway = first_party(limits("openai", "gw-priced", None, None));
    priced_by_gateway.cost = Some(cost(CostStatus::Known, Some(9.0), Some(9.0)));
    let mut md = source("models.dev", vec![paid, priced_by_gateway]);
    md.report.unmodelled = vec![
        diag(
            "models.dev",
            "openai",
            "reasoner",
            "cost.reasoning",
            serde_json::json!(5.0),
        ),
        diag(
            "models.dev",
            "openai",
            "gw-priced",
            "cost.input_audio",
            serde_json::json!(7.0),
        ),
    ];
    let mut gw_row = limits("cliproxyapi", "gw-priced", Some(1), Some(1));
    gw_row.cost = Some(cost(CostStatus::Known, Some(3.0), Some(4.0)));
    let mut gw = gateway(limits("cliproxyapi", "reasoner", Some(2), Some(2)));
    gw.observations
        .push(Observation::new(descriptor(GW), gw_row));
    let out = merge(&[&gw, &md]);
    let d: Vec<_> = out
        .report
        .diagnostics
        .iter()
        .filter(|d| d.kind == "unmodelled")
        .collect();
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(
        d[0].key.as_deref(),
        Some("cliproxyapi-cache:cliproxyapi/reasoner")
    );
    assert_eq!(
        (d[0].field.as_str(), d[0].source_id.as_str()),
        ("cost.reasoning", "models.dev")
    );
    assert_eq!(
        find(&out, "gw-priced").fields.cost.as_ref().unwrap().input,
        Some(3.0)
    );
}

#[test]
fn an_effort_list_left_with_only_off_after_rejecting_default_is_a_toggle() {
    let mut on_off = first_party(limits("groq", "qwen-switch", None, None));
    on_off.thinking = thinking(&[Effort::Off]);
    let mut only_none = first_party(limits("groq", "ling-none", None, None));
    only_none.thinking = thinking(&[Effort::Off]);
    let mut mp = source("modelparams.dev", vec![on_off, only_none]);
    // `default` was rejected for the first entry only (reasoning_effort in {none, default})
    mp.report.rejections = vec![diag(
        "modelparams.dev",
        "groq",
        "qwen-switch",
        "thinking.efforts",
        serde_json::json!("default"),
    )];
    let mut gw = gateway(limits("cliproxyapi", "qwen-switch", Some(1), Some(1)));
    gw.observations.push(Observation::new(
        descriptor(GW),
        limits("cliproxyapi", "ling-none", Some(2), Some(2)),
    ));
    let out = merge(&[&gw, &mp]);

    let t = find(&out, "qwen-switch").fields.thinking.as_ref().unwrap();
    assert_eq!(t.mode, ThinkingMode::Toggle);
    assert!(t.efforts.is_empty());
    assert_eq!(t.options.as_ref().unwrap().can_disable, Some(true));
    // only `none` accepted: not a switch, kept as the source says
    let t = find(&out, "ling-none").fields.thinking.as_ref().unwrap();
    assert_eq!(
        (t.mode, t.efforts.as_slice()),
        (ThinkingMode::Effort, &[Effort::Off][..])
    );
    let notes: Vec<_> = out
        .report
        .diagnostics
        .iter()
        .filter(|d| d.kind == "thinking-toggle")
        .collect();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].id, "qwen-switch");
}

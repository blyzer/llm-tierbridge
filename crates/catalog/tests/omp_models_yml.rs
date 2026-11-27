//! Text-level edit of `models.yml` over synthetic files with comments, odd spacing and secrets.

use tierbridge_catalog::omp_models_yml::{
    MANAGED_END, MANAGED_START, apply, parse_overrides, revert, strip_managed,
};

const OVERRIDES: &str = "\
# tierbridge export omp: generated
# catalog sha256:abc (3 models)
providers:
  alpha:
    # catalog sha256:abc; 2 of 3 models
    modelOverrides:
      m1:
        contextWindow: 1000
        cost:
          input: 1
          output: 2.5
      \"weird:id\":
        maxTokens: 5
  beta:
    modelOverrides:
      b/x:
        input: [text, image]
";

const MODELS: &str = "\
# my omp models
providers:
  alpha:
    baseUrl: http://127.0.0.1:8317/v1
    apiKey: sk-not-a-real-key # keep
    discovery:
      type: openai-models-list

  # beta is the second gateway
  beta:
    baseUrl: http://127.0.0.1:8326/v1
    models:
      - id: custom
        name: Custom   # trailing comment
  gamma:
    baseUrl: https://example.invalid/v1
";

fn doc() -> tierbridge_catalog::omp_models_yml::OverridesDoc {
    parse_overrides(OVERRIDES).unwrap()
}

fn yaml(text: &str) -> serde_json::Value {
    serde_json::to_value(serde_yaml::from_str::<serde_yaml::Value>(text).unwrap()).unwrap()
}

#[test]
fn inserts_inside_each_provider_and_keeps_every_other_line() {
    let plan = apply(MODELS, &doc()).unwrap();
    assert!(plan.changed);
    assert_eq!(plan.inserted.len(), 2);
    // everything outside the managed blocks is untouched, byte for byte
    let (stripped, blocks) = strip_managed(&plan.text).unwrap();
    assert_eq!((stripped.as_str(), blocks), (MODELS, 2));

    // the block follows the provider's last line, before the blank line and next comment
    let want = "\
      type: openai-models-list
    # >>> tierbridge-managed modelOverrides (do not edit) >>>
    # catalog sha256:abc; 2 of 3 models
    modelOverrides:
      m1:
        contextWindow: 1000
        cost:
          input: 1
          output: 2.5
      \"weird:id\":
        maxTokens: 5
    # <<< tierbridge-managed modelOverrides <<<

  # beta is the second gateway
  beta:";
    assert!(plan.text.contains(want), "{}", plan.text);
    // beta's block follows its `models:` list, before gamma
    assert!(
        plan.text.contains(&format!(
            "        name: Custom   # trailing comment\n    {MANAGED_START}\n    modelOverrides:\n      b/x:\n        input: [text, image]\n    {MANAGED_END}\n  gamma:"
        )),
        "{}",
        plan.text
    );
    let parsed = yaml(&plan.text);
    assert_eq!(
        parsed["providers"]["alpha"]["modelOverrides"]["weird:id"]["maxTokens"],
        5
    );
    assert_eq!(
        parsed["providers"]["gamma"]["baseUrl"],
        "https://example.invalid/v1"
    );
}

#[test]
fn applying_twice_gives_the_same_bytes_and_a_new_file_replaces_the_blocks() {
    let once = apply(MODELS, &doc()).unwrap();
    let twice = apply(&once.text, &doc()).unwrap();
    assert_eq!(twice.text, once.text);
    assert!(!twice.changed);
    assert_eq!(twice.removed_blocks, 2);

    let only_beta = parse_overrides(
        "providers:\n  beta:\n    modelOverrides:\n      b/y:\n        maxTokens: 9\n",
    )
    .unwrap();
    let third = apply(&once.text, &only_beta).unwrap();
    assert_eq!(third.removed_blocks, 2);
    assert!(!third.text.contains("weird:id"), "alpha's block is gone");
    assert!(third.text.contains("b/y:") && !third.text.contains("b/x:"));
}

#[test]
fn revert_restores_the_original_bytes() {
    let applied = apply(MODELS, &doc()).unwrap();
    let back = revert(&applied.text).unwrap();
    assert_eq!(back.text, MODELS);
    assert!(back.changed);
    assert!(!revert(MODELS).unwrap().changed, "nothing to revert");
}

#[test]
fn no_final_newline_and_crlf_round_trip() {
    let no_eol = MODELS.trim_end_matches('\n'); // the last provider is the last line
    let last = parse_overrides(
        "providers:\n  gamma:\n    modelOverrides:\n      g:\n        maxTokens: 1\n",
    )
    .unwrap();
    let applied = apply(no_eol, &last).unwrap();
    assert!(
        applied.text.ends_with(MANAGED_END),
        "ends like the original: no newline"
    );
    yaml(&applied.text);
    assert_eq!(revert(&applied.text).unwrap().text, no_eol);

    let crlf = MODELS.replace('\n', "\r\n");
    let applied = apply(&crlf, &doc()).unwrap();
    assert!(
        !applied.text.replace("\r\n", "").contains('\n'),
        "only CRLF line ends"
    );
    assert_eq!(revert(&applied.text).unwrap().text, crlf);
}

#[test]
fn follows_the_files_own_indentation() {
    let four = "providers:\n    alpha:\n        baseUrl: http://x/v1\n    beta:\n        baseUrl: http://y/v1\n";
    let applied = apply(four, &doc()).unwrap();
    assert!(
        applied
            .text
            .contains("        modelOverrides:\n          m1:\n            contextWindow: 1000")
    );
    let parsed = yaml(&applied.text);
    assert_eq!(
        parsed["providers"]["alpha"]["modelOverrides"]["m1"]["cost"]["output"],
        2.5
    );
    assert_eq!(revert(&applied.text).unwrap().text, four);
}

#[test]
fn refuses_what_it_cannot_edit_safely() {
    let own = "providers:\n  alpha:\n    baseUrl: http://x/v1\n    modelOverrides:\n      m1:\n        name: mine\n  beta:\n    baseUrl: http://y/v1\n";
    let e = apply(own, &doc()).unwrap_err().to_string();
    assert!(e.contains("already has a modelOverrides"), "{e}");

    let e = apply("providers:\n  alpha:\n    baseUrl: x\n", &doc())
        .unwrap_err()
        .to_string();
    assert!(e.contains("`beta` is not defined"), "{e}");

    let inline = "providers:\n  alpha: {baseUrl: x}\n  beta:\n    baseUrl: y\n";
    let e = apply(inline, &doc()).unwrap_err().to_string();
    assert!(e.contains("not a block mapping"), "{e}");

    assert!(apply("models: []\n", &doc()).is_err(), "no `providers:`");
    assert!(
        apply("providers:\n  alpha:\n  - broken: [\n", &doc()).is_err(),
        "not YAML"
    );

    let open = format!("providers:\n  alpha:\n    a: 1\n    {MANAGED_START}\n");
    assert!(strip_managed(&open).is_err());
    let stray = format!("providers:\n  alpha:\n    a: 1\n    {MANAGED_END}\n");
    assert!(strip_managed(&stray).is_err());
}

#[test]
fn overrides_files_in_another_shape_are_refused() {
    for (text, why) in [
        ("providers:\n  a:\n    models: []\n", "no `modelOverrides`"),
        ("providers:\n  a:\n    modelOverrides: []\n", "mapping"),
        ("other: 1\nproviders: {}\n", "top level"),
        ("providers:\n  a:\n\tmodelOverrides:\n", "tab"),
        ("not: [valid\n", "YAML"),
    ] {
        let e = parse_overrides(text).unwrap_err().to_string();
        assert!(e.contains(why), "{text:?}: {e}");
    }
    assert!(
        parse_overrides("providers: {}\n")
            .unwrap()
            .providers
            .is_empty()
    );
}

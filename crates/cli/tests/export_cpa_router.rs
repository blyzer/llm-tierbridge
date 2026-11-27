//! `tierbridge export cpa-router` end to end, on the public easycliproxy sample and on a small
//! hand-written catalog. No private fixture is needed.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::{Value, json};

const GATEWAY: &str = "omp-live:easycliproxy";

/// Scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "tierbridge-export-router-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn tierbridge(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tierbridge"))
        .args(args)
        .output()
        .unwrap()
}

fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `catalog.json` merged from the public easycliproxy sample (no reference catalogs: every cost is
/// unknown, which is a legitimate input).
fn merged_sample_catalog(scratch: &Scratch) -> PathBuf {
    let sample = common::fixture_path("samples/omp-models-easycliproxy.sample.json");
    let out_dir = scratch.path("out");
    let out = tierbridge(&[
        "catalog",
        "build",
        "--source",
        GATEWAY,
        "--input",
        sample.to_str().unwrap(),
        "--fetched-at",
        "2026-11-26T08:32:56Z",
        "--merge",
        "--out-dir",
        out_dir.to_str().unwrap(),
    ]);
    ok(&out);
    out_dir.join("catalog.json")
}

/// The `alias:` values of an `export cpa` fragment (they are JSON strings).
fn cpa_aliases(yaml: &str) -> BTreeSet<String> {
    yaml.lines()
        .filter_map(|l| l.strip_prefix("          alias: "))
        .map(|v| serde_json::from_str::<String>(v).unwrap())
        .collect()
}

#[test]
fn every_candidate_is_an_alias_export_cpa_writes_and_the_provider_is_the_cpa_key() {
    let scratch = Scratch::new("agree");
    let catalog = merged_sample_catalog(&scratch);
    let catalog_arg = catalog.to_str().unwrap();
    let catalog_bytes = fs::read(&catalog).unwrap();

    // the default name, then a mixed-case one CLIProxyAPI lowercases
    for (name, provider) in [
        (None, "openai-compatible-tierbridge-omp-live-easycliproxy"),
        (Some("Mixed-Case-Gw"), "openai-compatible-mixed-case-gw"),
    ] {
        for prefix in ["tb-", "x/"] {
            let mut cpa_args = vec![
                "export",
                "cpa",
                "--catalog",
                catalog_arg,
                "--gateway",
                GATEWAY,
                "--base-url",
                "http://127.0.0.1:8317/v1",
                "--alias-prefix",
                prefix,
            ];
            let mut router_args = vec![
                "export",
                "cpa-router",
                "--catalog",
                catalog_arg,
                "--gateway",
                GATEWAY,
                "--alias-prefix",
                prefix,
                "--profile",
                "free-first",
            ];
            if let Some(name) = name {
                cpa_args.extend(["--name", name]);
                router_args.extend(["--name", name]);
            }
            let cpa = tierbridge(&cpa_args);
            ok(&cpa);
            let router = tierbridge(&router_args);
            ok(&router);

            let aliases = cpa_aliases(&String::from_utf8(cpa.stdout).unwrap());
            assert!(aliases.len() > 20, "only {} aliases", aliases.len());
            let doc: Value = serde_json::from_slice(&router.stdout).unwrap();
            let entry = &doc["aliases"][0];
            assert_eq!(entry["alias"], "tierbridge/auto");
            let candidates = entry["candidates"].as_array().unwrap();
            // 21 of the sample's 31 rows carry only the gateway's filler limits: left out
            assert_eq!(candidates.len(), 10);
            assert!(candidates.len() < aliases.len());
            for candidate in candidates {
                let model = candidate["model"].as_str().unwrap();
                assert!(
                    aliases.contains(model),
                    "{model} is not an alias of export cpa"
                );
                assert!(model.starts_with(prefix));
                assert_eq!(candidate["provider"], provider);
            }

            // the hash is the one of the file that was read, byte for byte
            assert_eq!(
                doc["catalogSha256"],
                tierbridge_catalog::models_dev::sha256_hex(&catalog_bytes)
            );
            let stderr = String::from_utf8_lossy(&router.stderr);
            assert!(
                stderr.contains(&format!("ranked {} candidates", candidates.len())),
                "{stderr}"
            );
            assert!(
                stderr.contains(&format!("provider `{provider}`")),
                "{stderr}"
            );
            assert!(
                stderr.contains("note: no --freellmapi-observations"),
                "{stderr}"
            );
            assert!(
                stderr.contains("(skipped: placeholder-limits 21)"),
                "{stderr}"
            );
        }
    }
}

#[test]
fn a_second_run_is_byte_identical() {
    let scratch = Scratch::new("determinism");
    let catalog = merged_sample_catalog(&scratch);
    let args = [
        "export",
        "cpa-router",
        "--catalog",
        catalog.to_str().unwrap(),
        "--gateway",
        GATEWAY,
    ];
    let first = tierbridge(&args);
    let second = tierbridge(&args);
    ok(&first);
    ok(&second);
    assert!(!first.stdout.is_empty());
    assert_eq!(first.stdout, second.stdout);
}

fn model(gateway: &str, provider: &str, id: &str, extra: Value) -> Value {
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
        "supportsTools": null,
        "lifecycle": null,
        "provenance": {"sources": [], "fields": {}},
    });
    row.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    row
}

#[test]
fn quota_comes_from_the_observations_file_and_the_output_file_is_private() {
    let scratch = Scratch::new("quota");
    let catalog = scratch.path("catalog.json");
    let rows = [
        model(
            "freellmapi",
            "groq",
            "llama-70b",
            json!({"canonicalId": "meta/llama-70b"}),
        ),
        model(
            "omp-live:gw",
            "gw",
            "llama-70b-instruct",
            json!({"canonicalId": "meta/llama-70b", "contextWindow": 131_072,
                   "cost": {"status": "free", "input": 0.0, "output": 0.0}}),
        ),
        model(
            "omp-live:gw",
            "gw",
            "plain",
            json!({"contextWindow": 8_000}),
        ),
        model(
            "omp-live:gw",
            "gw",
            "embedder",
            json!({"kind": "embedding"}),
        ),
    ];
    fs::write(&catalog, serde_json::to_vec(&rows).unwrap()).unwrap();
    let observations = scratch.path("observations.freellmapi.jsonl");
    fs::write(
        &observations,
        format!(
            "{}\n",
            json!({"recordType": "observation", "source": {"sourceId": "freellmapi"},
                   "provider": "groq", "id": "llama-70b", "kind": "chat",
                   "raw": {"freeTier": {"rpm": 30, "rpd": 14_400, "tpm": 6_000, "tpd": 500_000,
                                        "monthlyTokenBudget": "~30M"}}})
        ),
    )
    .unwrap();
    let target = scratch.path("tierbridge-router.json");
    let out = tierbridge(&[
        "export",
        "cpa-router",
        "--catalog",
        catalog.to_str().unwrap(),
        "--gateway",
        "omp-live:gw",
        "--name",
        "gw",
        "--alias",
        "tierbridge/fast",
        "--freellmapi-observations",
        observations.to_str().unwrap(),
        "--out",
        target.to_str().unwrap(),
    ]);
    ok(&out);
    assert!(out.stdout.is_empty(), "--out writes the file, not stdout");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("ranked 2 candidates for alias `tierbridge/fast`"),
        "{stderr}"
    );
    assert!(stderr.contains("(skipped: not-chat 1)"), "{stderr}");
    assert!(
        stderr.contains("quota: 1 of 2 candidates matched a freellmapi observation"),
        "{stderr}"
    );

    let doc: Value = serde_json::from_slice(&fs::read(&target).unwrap()).unwrap();
    let entry = &doc["aliases"][0];
    assert_eq!(entry["alias"], "tierbridge/fast");
    let candidates = entry["candidates"].as_array().unwrap();
    let models: Vec<&str> = candidates
        .iter()
        .map(|c| c["model"].as_str().unwrap())
        .collect();
    // known-free first
    assert_eq!(models, ["tb-llama-70b-instruct", "tb-plain"]);
    assert_eq!(
        candidates[0]["quota"],
        json!({"rpm": 30, "rpd": 14_400, "tpm": 6_000, "tpd": 500_000,
               "monthlyTokenBudget": "~30M", "source": "freellmapi:groq/llama-70b",
               "matchedBy": "canonicalId", "matches": 1})
    );
    assert_eq!(
        candidates[0]["reasons"],
        json!(["known-free", "freellmapi-limits"])
    );
    assert_eq!(candidates[1]["quota"], Value::Null);
    assert_eq!(
        doc["quotaSourceSha256"],
        tierbridge_catalog::models_dev::sha256_hex(&fs::read(&observations).unwrap())
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn a_gateway_without_rows_fails_with_the_gateways_there_are() {
    let scratch = Scratch::new("norows");
    let catalog = scratch.path("catalog.json");
    fs::write(
        &catalog,
        serde_json::to_vec(&[model("omp-live:other", "o", "m", json!({}))]).unwrap(),
    )
    .unwrap();
    let out = tierbridge(&[
        "export",
        "cpa-router",
        "--catalog",
        catalog.to_str().unwrap(),
        "--gateway",
        "omp-live:gw",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no rows from gateway `omp-live:gw`"),
        "{stderr}"
    );
    assert!(stderr.contains("omp-live:other"), "{stderr}");
}

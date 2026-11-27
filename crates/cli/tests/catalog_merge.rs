//! `tierbridge catalog build ... --merge` over the committed snapshots of every source.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const FETCHED_AT: &str = "2026-11-26T08:32:56Z";

/// `(--source, fixture)`
const SOURCES: [(&str, &str); 9] = [
    ("models.dev", "models.dev.api.2026-11-26.json.zst"),
    ("modelparams.dev", "modelparams.models.2026-11-26.json.zst"),
    (
        "cliproxyapi-cache",
        "cliproxyapi-models.2026-11-26.json.zst",
    ),
    (
        "omp-live:easycliproxy",
        "omp-models-easycliproxy.2026-11-26.json.zst",
    ),
    ("omp-live:dsh", "omp-models-dsh.2026-11-26.json.zst"),
    ("omp-bundled", "omp-oracle-models.2026-11-26.json.zst"),
    (
        "router-for-me.models",
        "router-for-me.models.d02fed832.snapshot.json.zst",
    ),
    ("dsh", "dsh.b1142bd.snapshot.json.zst"),
    ("freellmapi", "freellmapi.0d64e07.snapshot.json.zst"),
];

/// Scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("tierbridge-merge-{}-{name}", std::process::id()));
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

fn fixture(name: &str) -> Vec<u8> {
    let path = common::fixture_path(name);
    zstd::stream::decode_all(fs::File::open(path).unwrap()).unwrap()
}

/// `catalog build --merge` of `sources` (a subset of [`SOURCES`]) into `scratch/out`.
fn build_merge(scratch: &Scratch, sources: &[&str], extra: &[&str]) -> Output {
    let mut args: Vec<String> = vec!["catalog".into(), "build".into()];
    for (source, file) in SOURCES.iter().filter(|(s, _)| sources.contains(s)) {
        let input = scratch.path(&format!("input-{}.json", source.replace(':', "_")));
        fs::write(&input, fixture(file)).unwrap();
        args.extend(["--source".into(), (*source).into()]);
        args.extend(["--input".into(), input.to_str().unwrap().into()]);
    }
    args.extend([
        "--out-dir".into(),
        scratch.path("out").to_str().unwrap().into(),
        "--fetched-at".into(),
        FETCHED_AT.into(),
        "--merge".into(),
    ]);
    args.extend(extra.iter().map(|s| (*s).to_owned()));
    Command::new(env!("CARGO_BIN_EXE_tierbridge"))
        .args(&args)
        .output()
        .unwrap()
}

fn json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn merge_writes_catalog_and_report_for_every_source() {
    let scratch = Scratch::new("all");
    let all: Vec<&str> = SOURCES.iter().map(|(s, _)| *s).collect();
    let out = build_merge(&scratch, &all, &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("merged  4355 models"), "{stdout}");
    assert!(stdout.contains("placeholder winners: 0"), "{stdout}");

    let catalog = json(&scratch.path("out/catalog.json"));
    let models = catalog.as_array().unwrap();
    assert_eq!(models.len(), 4355);
    assert!(models.iter().all(|m| m["recordType"] == "model"));
    // single-source catalogs keep their own file names
    assert!(scratch.path("out/catalog.models.dev.json").exists());

    let report = json(&scratch.path("out/report.merge.json"));
    assert_eq!(report["models"], 4355);
    assert_eq!(report["placeholderWinners"]["total"], 0);
    assert_eq!(report["cost"]["gatewayZeroWinners"], 0);
    assert!(
        report["cost"]["unknown"].as_u64().unwrap() > 0,
        "unknown cost is explicit"
    );
    assert_eq!(report["gateways"].as_array().unwrap().len(), 6);
    assert!(report["maxTokensStudy"]["comparable"].as_u64().unwrap() > 0);
    let probe: Vec<&Value> = report["sources"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["role"] == "probe")
        .collect();
    assert_eq!(probe.len(), 1);
    assert_eq!(probe[0]["sourceId"], "omp-bundled");
}

#[test]
fn merge_without_a_gateway_source_fails_and_writes_nothing() {
    let scratch = Scratch::new("nogateway");
    let out = build_merge(&scratch, &["models.dev", "modelparams.dev"], &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no gateway source"));
    assert!(
        !scratch.path("out").exists(),
        "nothing is written when the merge fails"
    );
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn merge_with_gateways_only_leaves_every_field_to_the_gateway() {
    let scratch = Scratch::new("gwonly");
    let out = build_merge(&scratch, &["cliproxyapi-cache"], &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report = json(&scratch.path("out/report.merge.json"));
    assert_eq!(report["models"], 1704);
    assert_eq!(report["placeholderWinners"]["total"], 0);
    assert!(
        report["maxTokensStudy"].is_null(),
        "no references, no study"
    );
    let catalog = json(&scratch.path("out/catalog.json"));
    // the gateway's placeholder limits are kept only as alternates; with no reference the limit is unknown
    let placeholder = catalog
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["provenance"]["fields"]["contextWindow"]["source"] == "none")
        .expect("a row whose placeholder context window found no reference");
    assert!(placeholder["contextWindow"].is_null());
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn overrides_win_as_configured_and_a_bad_file_fails_before_anything_is_written() {
    let scratch = Scratch::new("overrides");
    let overrides = scratch.path("overrides.json");
    fs::write(
        &overrides,
        r#"{"cliproxyapi-cache:cliproxyapi/gpt-5": {"contextWindow": 123456},
            "cliproxyapi-cache:cliproxyapi/not-a-model": {"maxTokens": 1}}"#,
    )
    .unwrap();
    let out = build_merge(
        &scratch,
        &["models.dev", "cliproxyapi-cache"],
        &["--overrides", overrides.to_str().unwrap()],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("overrides: 2 entries, 1 models overridden"),
        "{stdout}"
    );
    assert!(stdout.contains("unmatched-override"), "{stdout}");
    let catalog = json(&scratch.path("out/catalog.json"));
    let model = catalog
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["key"] == "cliproxyapi-cache:cliproxyapi/gpt-5")
        .expect("gpt-5 is in the cache snapshot");
    assert_eq!(model["contextWindow"], 123_456);
    assert_eq!(
        model["provenance"]["fields"]["contextWindow"]["rule"],
        "configured"
    );
    let report = json(&scratch.path("out/report.merge.json"));
    assert_eq!(report["overrides"]["modelsOverridden"], 1);

    // a typo in a field name is a structural error: exit 1, nothing written
    let bad = Scratch::new("overrides-bad");
    fs::write(bad.path("o.json"), r#"{"k": {"contextWindo": 1}}"#).unwrap();
    let out = build_merge(
        &bad,
        &["cliproxyapi-cache"],
        &["--overrides", bad.path("o.json").to_str().unwrap()],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(!bad.path("out").exists());
}

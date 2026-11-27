//! End-to-end `tierbridge catalog build` runs of the gateway sources against the committed
//! snapshots, and `tierbridge catalog match` over their observations.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const FETCHED_AT: &str = "2026-11-26T08:32:56Z";

/// Scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("tierbridge-gw-{}-{name}", std::process::id()));
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

fn tierbridge(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tierbridge"))
        .args(args)
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `catalog build --source <source> --input <fixture>` into `out`; returns the output.
fn build(source: &str, fixture_name: &str, scratch: &Scratch, out: &str) -> Output {
    let input = scratch.path(&format!("input-{source}.json"));
    fs::write(&input, fixture(fixture_name)).unwrap();
    tierbridge(&[
        "catalog",
        "build",
        "--source",
        source,
        "--input",
        input.to_str().unwrap(),
        "--out-dir",
        scratch.path(out).to_str().unwrap(),
        "--fetched-at",
        FETCHED_AT,
    ])
}

fn lines(path: &Path) -> usize {
    fs::read_to_string(path).unwrap().lines().count()
}

fn report(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn cliproxyapi_cache_builds_three_files() {
    let scratch = Scratch::new("cache");
    let out = build(
        "cliproxyapi-cache",
        "cliproxyapi-models.2026-11-26.json.zst",
        &scratch,
        "out",
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let dir = scratch.path("out");
    assert_eq!(
        lines(&dir.join("observations.cliproxyapi-cache.jsonl")),
        1704
    );
    assert_eq!(lines(&dir.join("catalog.cliproxyapi-cache.json")), 1);
    let r = report(&dir.join("report.cliproxyapi-cache.json"));
    assert_eq!(r["counts"]["observations"], 1704);
    assert_eq!(r["counts"]["rejections"], 8);
    assert_eq!(r["source"]["sourceId"], "cliproxyapi-cache");
    assert_eq!(r["source"]["observedAtMs"], 1_795_670_401_936_i64);
    assert!(
        !dir.join("catalog.json").exists(),
        "catalog.json is reserved for the merge"
    );
}

#[test]
fn a_source_without_a_download_url_cannot_be_fetched() {
    let scratch = Scratch::new("nofetch");
    let out = tierbridge(&[
        "catalog",
        "build",
        "--source",
        "cliproxyapi-cache",
        "--fetch",
        "--out-dir",
        scratch.path("out").to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("no download URL"),
        "{}",
        text(&out.stderr)
    );
    assert!(
        !scratch.path("out").exists(),
        "nothing is written on failure"
    );
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn strict_does_not_fail_on_a_clean_gateway_snapshot_but_does_on_rejections() {
    let scratch = Scratch::new("strict");
    let input = scratch.path("cache.json");
    fs::write(&input, fixture("cliproxyapi-models.2026-11-26.json.zst")).unwrap();
    let out = tierbridge(&[
        "catalog",
        "build",
        "--source",
        "cliproxyapi-cache",
        "--input",
        input.to_str().unwrap(),
        "--out-dir",
        scratch.path("out").to_str().unwrap(),
        "--strict",
    ]);
    // the snapshot has 8 `ultra` levels outside the contract
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
    assert!(!scratch.path("out").exists());
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn omp_live_builds_one_set_of_files_per_provider() {
    let scratch = Scratch::new("live");
    let easy = scratch.path("easy.json");
    let dsh = scratch.path("dsh.json");
    fs::write(
        &easy,
        fixture("omp-models-easycliproxy.2026-11-26.json.zst"),
    )
    .unwrap();
    fs::write(&dsh, fixture("omp-models-dsh.2026-11-26.json.zst")).unwrap();
    let out_dir = scratch.path("out");
    let out = tierbridge(&[
        "catalog",
        "build",
        "--source",
        "omp-live:easycliproxy",
        "--input",
        easy.to_str().unwrap(),
        "--source",
        "omp-live:dsh",
        "--input",
        dsh.to_str().unwrap(),
        "--out-dir",
        out_dir.to_str().unwrap(),
        "--fetched-at",
        FETCHED_AT,
        "--strict",
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        lines(&out_dir.join("observations.omp-live:easycliproxy.jsonl")),
        1699
    );
    assert_eq!(lines(&out_dir.join("observations.omp-live:dsh.jsonl")), 51);
    let r = report(&out_dir.join("report.omp-live:dsh.json"));
    assert_eq!(r["counts"]["observations"], 51);
    assert_eq!(r["counts"]["rejections"], 0);
    assert_eq!(
        r["source"]["origin"],
        "omp models dsh --json --no-extensions"
    );
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn match_reproduces_the_headline_counts_from_built_observations() {
    let scratch = Scratch::new("match");
    let out_dir = scratch.path("out");
    let sources = [
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
    ];
    let mut args: Vec<String> = [
        "catalog",
        "build",
        "--out-dir",
        out_dir.to_str().unwrap(),
        "--fetched-at",
        FETCHED_AT,
    ]
    .map(String::from)
    .to_vec();
    for (i, (source, name)) in sources.iter().enumerate() {
        let input = scratch.path(&format!("in{i}.json"));
        fs::write(&input, fixture(name)).unwrap();
        args.extend([
            "--source".into(),
            (*source).into(),
            "--input".into(),
            input.to_str().unwrap().into(),
        ]);
    }
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let built = tierbridge(&argv);
    assert!(built.status.success(), "{}", text(&built.stderr));

    let obs = |s: &str| {
        out_dir
            .join(format!("observations.{s}.jsonl"))
            .to_str()
            .unwrap()
            .to_owned()
    };
    let out = tierbridge(&[
        "catalog",
        "match",
        "--gateway",
        &obs("cliproxyapi-cache"),
        "--gateway",
        &obs("omp-live:easycliproxy"),
        "--gateway",
        &obs("omp-live:dsh"),
        "--reference",
        &obs("models.dev"),
        "--reference",
        &obs("modelparams.dev"),
        "--probe",
        &obs("omp-bundled"),
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let table = text(&out.stdout);
    assert!(
        table.contains("models.dev: T1 810  T2 248  T3 152  T4 3  unmatched 491"),
        "{table}"
    );
    assert!(
        table.contains("either 1213  both 635  neither 491  exact only (T1+T2) 1060"),
        "{table}"
    );
    assert!(table.contains("union  1762 unique ids"), "{table}");
    assert!(
        table.contains("either 1264 of 1762 (neither 498)"),
        "{table}"
    );
    assert!(table.contains("omp-bundled (probe)"), "{table}");

    let json = tierbridge(&[
        "catalog",
        "match",
        "--json",
        "--gateway",
        &obs("omp-live:dsh"),
        "--reference",
        &obs("models.dev"),
    ]);
    let v: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(
        v["gateways"][0]["perReference"]["models.dev"]["T2-exact-leaf"],
        47
    );
}

#[test]
fn match_rejects_a_file_that_is_not_observations() {
    let scratch = Scratch::new("matchbad");
    let bad = scratch.path("bad.jsonl");
    fs::write(&bad, "{\"x\":1}\n").unwrap();
    let out = tierbridge(&[
        "catalog",
        "match",
        "--gateway",
        bad.to_str().unwrap(),
        "--reference",
        bad.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("not an observation"),
        "{}",
        text(&out.stderr)
    );
}

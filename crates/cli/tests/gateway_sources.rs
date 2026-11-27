//! `tierbridge catalog build` over the git-checkout sources (snapshot envelopes).

mod common;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;
use tierbridge_catalog::{catalog_file, observations_file, report_file};

const FETCHED_AT: &str = "2026-11-26T08:32:56Z";

/// `(--source id, fixture file, observations, rejections)` for each snapshot source.
const SOURCES: [(&str, &str, usize, usize); 3] = [
    (
        "router-for-me.models",
        "router-for-me.models.d02fed832.snapshot.json.zst",
        520,
        54,
    ),
    ("dsh", "dsh.b1142bd.snapshot.json.zst", 511, 9),
    ("freellmapi", "freellmapi.0d64e07.snapshot.json.zst", 118, 0),
];

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("tierbridge-gw-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture(scratch: &Scratch, name: &str) -> PathBuf {
    let src = common::fixture_path(name);
    let bytes = zstd::stream::decode_all(fs::File::open(src).unwrap()).unwrap();
    let path = scratch.0.join(name.trim_end_matches(".zst"));
    fs::write(&path, bytes).unwrap();
    path
}

fn tierbridge(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tierbridge"))
        .args(args)
        .output()
        .unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn builds_observations_catalog_and_report_per_source() {
    for (source, name, observations, rejections) in SOURCES {
        let scratch = Scratch::new(&source.replace(['.', '/'], "-"));
        let input = fixture(&scratch, name);
        let out_dir = scratch.0.join("out");
        let out = tierbridge(&[
            "catalog",
            "build",
            "--source",
            source,
            "--input",
            input.to_str().unwrap(),
            "--fetched-at",
            FETCHED_AT,
            "--out-dir",
            out_dir.to_str().unwrap(),
        ]);
        assert!(out.status.success(), "{source}: {}", stderr(&out));

        let lines = fs::read_to_string(out_dir.join(observations_file(source))).unwrap();
        assert_eq!(lines.lines().count(), observations, "{source}");
        let catalog: Value =
            serde_json::from_slice(&fs::read(out_dir.join(catalog_file(source))).unwrap()).unwrap();
        assert_eq!(
            catalog.as_array().map(Vec::len),
            Some(observations),
            "{source}"
        );
        let report: Value =
            serde_json::from_slice(&fs::read(out_dir.join(report_file(source))).unwrap()).unwrap();
        assert_eq!(report["counts"]["rejections"], rejections, "{source}");
        assert_eq!(report["source"]["sourceId"], source);
        assert!(
            report["source"]["revision"].as_str().unwrap().contains('@'),
            "commit sha + date"
        );
        assert!(
            !out_dir.join("catalog.json").exists(),
            "catalog.json is reserved for the merge"
        );
    }
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn strict_fails_on_rejections_and_writes_nothing() {
    for (source, name, _, rejections) in SOURCES {
        let scratch = Scratch::new(&format!("strict-{}", source.replace(['.', '/'], "-")));
        let input = fixture(&scratch, name);
        let out_dir = scratch.0.join("out");
        let out = tierbridge(&[
            "catalog",
            "build",
            "--source",
            source,
            "--input",
            input.to_str().unwrap(),
            "--fetched-at",
            FETCHED_AT,
            "--out-dir",
            out_dir.to_str().unwrap(),
            "--strict",
        ]);
        if rejections == 0 {
            assert!(out.status.success(), "{source}: {}", stderr(&out));
        } else {
            assert_eq!(out.status.code(), Some(3), "{source}: {}", stderr(&out));
            assert!(!out_dir.exists(), "{source}: nothing written");
        }
    }
}

#[test]
fn fetch_is_refused_for_snapshot_sources() {
    for (source, ..) in SOURCES {
        let scratch = Scratch::new(&format!("fetch-{}", source.replace(['.', '/'], "-")));
        let out_dir = scratch.0.join("out");
        let out = tierbridge(&[
            "catalog",
            "build",
            "--source",
            source,
            "--fetch",
            "--out-dir",
            out_dir.to_str().unwrap(),
        ]);
        assert_eq!(out.status.code(), Some(1), "{source}");
        assert!(
            stderr(&out).contains("has no download URL"),
            "{}",
            stderr(&out)
        );
        assert!(!out_dir.exists());
    }
}

#[test]
fn shape_failure_aborts_with_a_structural_error() {
    let scratch = Scratch::new("shape");
    let input = scratch.0.join("bad.json");
    fs::write(&input, br#"{"format":"tierbridge-snapshot/1"}"#).unwrap();
    let out_dir = scratch.0.join("out");
    for (source, ..) in SOURCES {
        let out = tierbridge(&[
            "catalog",
            "build",
            "--source",
            source,
            "--input",
            input.to_str().unwrap(),
            "--out-dir",
            out_dir.to_str().unwrap(),
        ]);
        assert_eq!(out.status.code(), Some(1), "{source}");
        assert!(
            stderr(&out).contains("malformed source"),
            "{}",
            stderr(&out)
        );
        assert!(!out_dir.exists());
    }
}

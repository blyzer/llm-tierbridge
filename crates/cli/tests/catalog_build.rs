//! End-to-end `tierbridge catalog build` runs against the committed models.dev snapshot.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

const FETCHED_AT: &str = "2026-11-26T08:32:56Z";

/// Scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("tierbridge-cli-{}-{name}", std::process::id()));
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

fn snapshot() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../catalog/tests/fixtures/models.dev.api.2026-11-26.json.zst");
    zstd::stream::decode_all(fs::File::open(path).unwrap()).unwrap()
}

fn mp_snapshot() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../catalog/tests/fixtures/modelparams.models.2026-11-26.json.zst");
    zstd::stream::decode_all(fs::File::open(path).unwrap()).unwrap()
}

/// `catalog build` over `(source, input)` pairs, in the order given.
fn build_many(pairs: &[(&str, &Path)], out_dir: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["catalog", "build"];
    for (source, input) in pairs {
        args.extend_from_slice(&["--source", source, "--input", input.to_str().unwrap()]);
    }
    args.extend_from_slice(&[
        "--out-dir",
        out_dir.to_str().unwrap(),
        "--fetched-at",
        FETCHED_AT,
    ]);
    args.extend_from_slice(extra);
    tierbridge(&args)
}

fn tierbridge(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tierbridge"))
        .args(args)
        .output()
        .unwrap()
}

fn build(input: &Path, out_dir: &Path, extra: &[&str]) -> Output {
    let mut args = vec![
        "catalog",
        "build",
        "--source",
        "models.dev",
        "--input",
        input.to_str().unwrap(),
        "--out-dir",
        out_dir.to_str().unwrap(),
        "--fetched-at",
        FETCHED_AT,
    ];
    args.extend_from_slice(extra);
    tierbridge(&args)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn builds_the_snapshot_and_reports_rejections_and_sentinels() {
    let tmp = Scratch::new("ok");
    fs::write(tmp.path("api.json"), snapshot()).unwrap();
    let out_dir = tmp.path("out");
    let run = build(&tmp.path("api.json"), &out_dir, &[]);
    assert!(run.status.success(), "{}", text(&run.stderr));
    let stdout = text(&run.stdout);
    assert!(stdout.contains("rejected values: 2"), "{stdout}");
    assert!(stdout.contains("sentinels mapped: 424"), "{stdout}");
    assert!(stdout.contains("thinking.minBudgetTokens: 2"), "{stdout}");
    assert!(stdout.contains("maxTokens: 263"), "{stdout}");
    assert!(
        stdout.lines().count() <= 20,
        "summary must stay one screen:\n{stdout}"
    );
    assert!(stdout.contains("observations 10049"), "{stdout}");

    assert_eq!(
        files_in(&out_dir),
        [
            "catalog.models.dev.json",
            "observations.models.dev.jsonl",
            "report.models.dev.json"
        ],
        "no temp files left behind"
    );
    let observations = fs::read_to_string(out_dir.join("observations.models.dev.jsonl")).unwrap();
    assert_eq!(observations.lines().count(), 10_049);
    let catalog: Value =
        serde_json::from_slice(&fs::read(out_dir.join("catalog.models.dev.json")).unwrap())
            .unwrap();
    assert_eq!(catalog.as_array().unwrap().len(), 10_049);
    assert_eq!(catalog[0]["recordType"], "model");

    let report: Value =
        serde_json::from_slice(&fs::read(out_dir.join("report.models.dev.json")).unwrap()).unwrap();
    assert_eq!(report["counts"]["rejections"], 2);
    assert_eq!(report["counts"]["sentinels"], 424);
    assert_eq!(report["counts"]["zeroLimitsMappedToUnknown"], 422);
    assert_eq!(
        report["counts"]["sentinelsByField"],
        json!({"contextWindow": 155, "maxInputTokens": 4, "maxTokens": 263,
               "thinking.minBudgetTokens": 2})
    );
    assert_eq!(report["sentinels"].as_array().unwrap().len(), 424);
    assert_eq!(report["counts"]["models"], 10_049);
    assert_eq!(report["source"]["sha256"].as_str().unwrap().len(), 64);
    assert_eq!(report["source"]["fetchedAt"], FETCHED_AT);
    assert_eq!(report["source"]["revision"], FETCHED_AT);
    assert!(
        report["source"].get("http").is_none(),
        "no HTTP metadata for --input runs"
    );
    assert_eq!(report["rejections"][0]["field"], "thinking.efforts");
    let zero = report["sentinels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["field"] == "maxTokens")
        .unwrap();
    assert_eq!(zero["value"], 0);
    assert_eq!(zero["mappedTo"], "unknown");
    assert_eq!(
        zero["reason"],
        "models.dev publishes 0 when no token limit applies"
    );
}

#[test]
fn truncated_input_fails_and_leaves_previous_outputs_untouched() {
    let tmp = Scratch::new("truncated");
    let full = snapshot();
    fs::write(tmp.path("api.json"), &full[..full.len() / 2]).unwrap();
    let out_dir = tmp.path("out");
    fs::create_dir_all(&out_dir).unwrap();
    let previous = [
        ("observations.models.dev.jsonl", "previous observations\n"),
        ("catalog.models.dev.json", "[\"previous\"]"),
        ("report.models.dev.json", "{\"previous\":true}"),
    ];
    for (name, body) in previous {
        fs::write(out_dir.join(name), body).unwrap();
    }

    let run = build(&tmp.path("api.json"), &out_dir, &[]);
    assert_eq!(run.status.code(), Some(1), "{}", text(&run.stderr));
    assert!(
        text(&run.stderr).contains("not valid JSON"),
        "{}",
        text(&run.stderr)
    );
    for (name, body) in previous {
        assert_eq!(
            fs::read_to_string(out_dir.join(name)).unwrap(),
            body,
            "{name} was touched"
        );
    }
    assert_eq!(files_in(&out_dir).len(), 3, "no temp files left behind");
}

#[test]
fn empty_object_fails_without_creating_the_out_dir() {
    let tmp = Scratch::new("empty");
    fs::write(tmp.path("api.json"), "{}").unwrap();
    let out_dir = tmp.path("out");
    let run = build(&tmp.path("api.json"), &out_dir, &[]);
    assert_eq!(run.status.code(), Some(1));
    assert!(
        text(&run.stderr).contains("zero models"),
        "{}",
        text(&run.stderr)
    );
    assert!(!out_dir.exists());
}

#[test]
fn strict_fails_on_rejections_and_writes_nothing() {
    let tmp = Scratch::new("strict");
    fs::write(tmp.path("api.json"), snapshot()).unwrap();
    let out_dir = tmp.path("out");
    let run = build(&tmp.path("api.json"), &out_dir, &["--strict"]);
    assert_eq!(run.status.code(), Some(3), "{}", text(&run.stderr));
    let stderr = text(&run.stderr);
    assert!(
        stderr.contains("thinking.efforts") && stderr.contains("2 value(s) were rejected"),
        "{stderr}"
    );
    assert!(!out_dir.exists());
}

#[test]
fn strict_passes_when_nothing_is_rejected() {
    let tmp = Scratch::new("strict-clean");
    let doc = json!({"p": {"models": {"m": {"name": "M", "reasoning": false}}}});
    fs::write(tmp.path("api.json"), doc.to_string()).unwrap();
    let run = build(&tmp.path("api.json"), &tmp.path("out"), &["--strict"]);
    assert!(run.status.success(), "{}", text(&run.stderr));
}

#[test]
fn min_models_not_reached_fails() {
    let tmp = Scratch::new("min");
    fs::write(tmp.path("api.json"), snapshot()).unwrap();
    let out_dir = tmp.path("out");
    let run = build(&tmp.path("api.json"), &out_dir, &["--min-models", "10050"]);
    assert_eq!(run.status.code(), Some(1));
    assert!(
        text(&run.stderr).contains("10049 models"),
        "{}",
        text(&run.stderr)
    );
    assert!(!out_dir.exists());
    let ok = build(&tmp.path("api.json"), &out_dir, &["--min-models", "10049"]);
    assert!(ok.status.success());
}

#[test]
fn usage_errors() {
    let tmp = Scratch::new("usage");
    let out = tmp.path("out");
    let out = out.to_str().unwrap();
    // neither --input nor --fetch
    assert_eq!(
        tierbridge(&[
            "catalog",
            "build",
            "--source",
            "models.dev",
            "--out-dir",
            out
        ])
        .status
        .code(),
        Some(2)
    );
    // both
    let both = [
        "catalog",
        "build",
        "--source",
        "models.dev",
        "--input",
        "x",
        "--fetch",
        "--out-dir",
        out,
    ];
    assert_eq!(tierbridge(&both).status.code(), Some(2));
    // unknown source
    let unknown = [
        "catalog",
        "build",
        "--source",
        "nope",
        "--input",
        "x",
        "--out-dir",
        out,
    ];
    assert_eq!(tierbridge(&unknown).status.code(), Some(2));
    // unreadable input is a plain failure
    let missing = [
        "catalog",
        "build",
        "--source",
        "models.dev",
        "--input",
        "/nonexistent/api.json",
        "--out-dir",
        out,
    ];
    assert_eq!(tierbridge(&missing).status.code(), Some(1));
}

// ---- modelparams.dev and several sources ------------------------------------------------------

fn json_file(path: PathBuf) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn builds_modelparams_alone_and_reports_what_it_rejected_and_mapped() {
    let tmp = Scratch::new("mp");
    fs::write(tmp.path("mp.json"), mp_snapshot()).unwrap();
    let out_dir = tmp.path("out");
    let run = build_many(&[("modelparams.dev", &tmp.path("mp.json"))], &out_dir, &[]);
    assert!(run.status.success(), "{}", text(&run.stderr));
    let stdout = text(&run.stdout);
    assert!(
        stdout.starts_with("modelparams.dev  sha256 46b4ffc76a3cf3f8"),
        "{stdout}"
    );
    assert!(
        stdout.contains("providers 22  observations 510  models 510"),
        "{stdout}"
    );
    assert!(stdout.contains("rejected values: 5"), "{stdout}");
    assert!(
        stdout.contains(
            "groq/qwen3-32b  thinking.efforts = \"default\"  (effort outside the contract enum)"
        ),
        "rejections are listed with their message:\n{stdout}"
    );
    assert!(stdout.contains("sentinels mapped: 10"), "{stdout}");
    assert!(
        stdout.lines().count() <= 20,
        "summary must stay one screen:\n{stdout}"
    );
    assert_eq!(
        files_in(&out_dir),
        [
            "catalog.modelparams.dev.json",
            "observations.modelparams.dev.jsonl",
            "report.modelparams.dev.json"
        ]
    );
    let obs = fs::read_to_string(out_dir.join("observations.modelparams.dev.jsonl")).unwrap();
    assert_eq!(obs.lines().count(), 510);
    let report = json_file(out_dir.join("report.modelparams.dev.json"));
    assert_eq!(report["source"]["sourceId"], "modelparams.dev");
    assert_eq!(report["source"]["revision"], "2026-11-24T08:22:46.486Z");
    assert_eq!(report["counts"]["rejections"], 5);
    assert_eq!(report["counts"]["zeroLimitsMappedToUnknown"], 0);
    assert_eq!(
        report["counts"]["sentinelsByField"],
        json!({
            "acceptedParams[generationConfig.thinkingConfig.thinkingBudget].default": 2,
            "acceptedParams[generationConfig.thinkingConfig.thinkingBudget].range.min": 2,
            "acceptedParams[reasoning_budget].range.min": 6,
        })
    );
    assert_eq!(
        json_file(out_dir.join("catalog.modelparams.dev.json"))
            .as_array()
            .unwrap()
            .len(),
        510
    );
}

#[test]
fn builds_both_sources_in_one_run_without_merging_them() {
    let tmp = Scratch::new("both");
    fs::write(tmp.path("md.json"), snapshot()).unwrap();
    fs::write(tmp.path("mp.json"), mp_snapshot()).unwrap();
    let out_dir = tmp.path("out");
    let pairs = [
        ("models.dev", tmp.path("md.json")),
        ("modelparams.dev", tmp.path("mp.json")),
    ];
    let pairs: Vec<_> = pairs.iter().map(|(s, p)| (*s, p.as_path())).collect();
    let run = build_many(&pairs, &out_dir, &[]);
    assert!(run.status.success(), "{}", text(&run.stderr));
    let stdout = text(&run.stdout);
    let md = stdout
        .find("models.dev  sha256")
        .expect("models.dev summary");
    let mp = stdout
        .find("modelparams.dev  sha256")
        .expect("modelparams.dev summary");
    assert!(md < mp, "summaries follow the --source order:\n{stdout}");
    assert!(stdout.contains("observations 10049") && stdout.contains("observations 510"));
    assert_eq!(
        files_in(&out_dir),
        [
            "catalog.modelparams.dev.json",
            "catalog.models.dev.json",
            "observations.modelparams.dev.jsonl",
            "observations.models.dev.jsonl",
            "report.modelparams.dev.json",
            "report.models.dev.json",
        ],
        "per-source files only: catalog.json is reserved for the merged catalog"
    );
    assert_eq!(
        json_file(out_dir.join("catalog.models.dev.json"))
            .as_array()
            .unwrap()
            .len(),
        10_049
    );
    assert_eq!(
        json_file(out_dir.join("catalog.modelparams.dev.json"))
            .as_array()
            .unwrap()
            .len(),
        510
    );
}

#[test]
fn inputs_are_paired_with_sources_by_position() {
    let tmp = Scratch::new("pairing");
    fs::write(tmp.path("md.json"), snapshot()).unwrap();
    fs::write(tmp.path("mp.json"), mp_snapshot()).unwrap();
    let swapped = [
        ("modelparams.dev", tmp.path("mp.json")),
        ("models.dev", tmp.path("md.json")),
    ];
    let pairs: Vec<_> = swapped.iter().map(|(s, p)| (*s, p.as_path())).collect();
    assert!(build_many(&pairs, &tmp.path("out"), &[]).status.success());
    // the wrong file for a source is a structural failure of that adapter, not a silent mix-up
    let crossed = [
        ("models.dev", tmp.path("mp.json")),
        ("modelparams.dev", tmp.path("md.json")),
    ];
    let pairs: Vec<_> = crossed.iter().map(|(s, p)| (*s, p.as_path())).collect();
    let run = build_many(&pairs, &tmp.path("out2"), &[]);
    assert_eq!(run.status.code(), Some(1), "{}", text(&run.stderr));
    assert!(!tmp.path("out2").exists());
}

#[test]
fn a_structural_failure_in_one_source_writes_nothing_for_any() {
    let tmp = Scratch::new("one-bad");
    fs::write(tmp.path("md.json"), snapshot()).unwrap();
    let mp = mp_snapshot();
    fs::write(tmp.path("mp.json"), &mp[..mp.len() / 2]).unwrap();
    let out_dir = tmp.path("out");
    let pairs = [
        ("models.dev", tmp.path("md.json")),
        ("modelparams.dev", tmp.path("mp.json")),
    ];
    let pairs: Vec<_> = pairs.iter().map(|(s, p)| (*s, p.as_path())).collect();
    let run = build_many(&pairs, &out_dir, &[]);
    assert_eq!(run.status.code(), Some(1));
    assert!(
        text(&run.stderr).contains("not valid JSON"),
        "{}",
        text(&run.stderr)
    );
    assert!(
        !out_dir.exists(),
        "the good source must not be written either"
    );
}

#[test]
fn strict_counts_rejections_over_all_sources() {
    let tmp = Scratch::new("strict-many");
    fs::write(tmp.path("md.json"), snapshot()).unwrap();
    fs::write(tmp.path("mp.json"), mp_snapshot()).unwrap();
    let out_dir = tmp.path("out");
    let pairs = [
        ("models.dev", tmp.path("md.json")),
        ("modelparams.dev", tmp.path("mp.json")),
    ];
    let pairs: Vec<_> = pairs.iter().map(|(s, p)| (*s, p.as_path())).collect();
    let run = build_many(&pairs, &out_dir, &["--strict"]);
    assert_eq!(run.status.code(), Some(3), "{}", text(&run.stderr));
    assert!(
        text(&run.stderr).contains("7 value(s) were rejected"),
        "{}",
        text(&run.stderr)
    );
    assert!(!out_dir.exists());
}

#[test]
fn min_models_can_be_set_per_source() {
    let tmp = Scratch::new("min-many");
    fs::write(tmp.path("md.json"), snapshot()).unwrap();
    fs::write(tmp.path("mp.json"), mp_snapshot()).unwrap();
    let pairs = [
        ("models.dev", tmp.path("md.json")),
        ("modelparams.dev", tmp.path("mp.json")),
    ];
    let pairs: Vec<_> = pairs.iter().map(|(s, p)| (*s, p.as_path())).collect();
    let out_dir = tmp.path("out");

    let run = build_many(&pairs, &out_dir, &["--min-models", "modelparams.dev=511"]);
    assert_eq!(run.status.code(), Some(1));
    assert!(
        text(&run.stderr).contains("510 models"),
        "{}",
        text(&run.stderr)
    );
    assert!(!out_dir.exists());

    // a source-specific minimum beats the bare one; the bare one covers the rest
    let ok = build_many(
        &pairs,
        &out_dir,
        &[
            "--min-models",
            "10049",
            "--min-models",
            "modelparams.dev=510",
        ],
    );
    assert!(ok.status.success(), "{}", text(&ok.stderr));
    let bare = build_many(&pairs, &tmp.path("out3"), &["--min-models", "511"]);
    assert_eq!(
        bare.status.code(),
        Some(1),
        "a bare minimum applies to every source"
    );

    for bad in ["nope=3", "modelparams.dev=many", "-1"] {
        let run = build_many(&pairs, &tmp.path("out4"), &["--min-models", bad]);
        assert_ne!(run.status.code(), Some(0), "{bad}");
    }
}

#[test]
fn source_and_input_counts_must_match_and_sources_must_be_distinct() {
    let tmp = Scratch::new("pairs");
    fs::write(tmp.path("md.json"), snapshot()).unwrap();
    let out = tmp.path("out");
    let md = tmp.path("md.json");
    let (out_s, md_s) = (out.to_str().unwrap(), md.to_str().unwrap());
    let fewer_inputs = [
        "catalog",
        "build",
        "--source",
        "models.dev",
        "--source",
        "modelparams.dev",
        "--input",
        md_s,
        "--out-dir",
        out_s,
    ];
    let run = tierbridge(&fewer_inputs);
    assert_eq!(run.status.code(), Some(1));
    assert!(
        text(&run.stderr).contains("one --input per --source"),
        "{}",
        text(&run.stderr)
    );
    let twice = [
        "catalog",
        "build",
        "--source",
        "models.dev",
        "--source",
        "models.dev",
        "--input",
        md_s,
        "--input",
        md_s,
        "--out-dir",
        out_s,
    ];
    let run = tierbridge(&twice);
    assert_eq!(run.status.code(), Some(1));
    assert!(
        text(&run.stderr).contains("more than once"),
        "{}",
        text(&run.stderr)
    );
    assert!(!out.exists());
    // no --source at all is a usage error
    let none = ["catalog", "build", "--input", md_s, "--out-dir", out_s];
    assert_eq!(tierbridge(&none).status.code(), Some(2));
}

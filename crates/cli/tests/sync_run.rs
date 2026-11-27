//! `tierbridge sync init` / `sync run` over the committed public fixtures: no network, every
//! source comes from `--input`. The data repo is a real git repo, its remote a bare repo.

#![cfg(unix)]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_tierbridge");
const NOW: &str = "2026-11-27T06:00:00Z";

/// `(source id, fixture, compressed)`: every source `sync run` tracks, from public data.
const SOURCES: [(&str, &str, bool); 8] = [
    ("models.dev", "models.dev.api.2026-11-26.json.zst", true),
    (
        "modelparams.dev",
        "modelparams.models.2026-11-26.json.zst",
        true,
    ),
    (
        "cliproxyapi-cache",
        "samples/cliproxyapi-cache.sample.json",
        false,
    ),
    (
        "omp-live:easycliproxy",
        "samples/omp-models-easycliproxy.sample.json",
        false,
    ),
    ("omp-live:dsh", "samples/omp-models-dsh.sample.json", false),
    (
        "router-for-me.models",
        "samples/router-models.sample.snapshot.json",
        false,
    ),
    ("dsh", "dsh.b1142bd.snapshot.json.zst", true),
    ("freellmapi", "freellmapi.0d64e07.snapshot.json.zst", true),
];

fn fixture(name: &str, compressed: bool) -> Vec<u8> {
    let path = common::fixture_path(name);
    if compressed {
        zstd::stream::decode_all(fs::File::open(path).unwrap()).unwrap()
    } else {
        fs::read(path).unwrap()
    }
}

/// Everything one test needs, removed on drop.
struct Env {
    dir: PathBuf,
}

impl Env {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("tierbridge-sync-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for d in ["inputs", "cache", "agent"] {
            fs::create_dir_all(dir.join(d)).unwrap();
        }
        let env = Self { dir };
        for (id, name, compressed) in SOURCES {
            fs::write(env.input(id), fixture(name, compressed)).unwrap();
        }
        env
    }

    fn input(&self, source: &str) -> PathBuf {
        self.dir.join("inputs").join(source.replace(':', "_"))
    }

    fn data(&self) -> PathBuf {
        self.dir.join("data")
    }

    fn remote(&self) -> PathBuf {
        self.dir.join("remote.git")
    }

    fn init(&self) {
        let out = Command::new(BIN)
            .args(["sync", "init", "--data-repo"])
            .arg(self.data())
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
    }

    /// A bare repo as `origin`, with `main` already pushed.
    fn init_with_remote(&self) {
        let out = Command::new("git")
            .args(["init", "--bare", "--quiet", "-b", "main"])
            .arg(self.remote())
            .output()
            .unwrap();
        assert!(out.status.success());
        let out = Command::new(BIN)
            .args(["sync", "init", "--data-repo"])
            .arg(self.data())
            .arg("--remote")
            .arg(self.remote())
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
        self.git(&["push", "--quiet", "origin", "main"]);
    }

    /// `sync run` with every source given as `--input`.
    fn run(&self, extra: &[&str]) -> Output {
        self.run_with(true, extra)
    }

    fn run_with(&self, inputs: bool, extra: &[&str]) -> Output {
        self.run_cmd(inputs, extra).output().unwrap()
    }

    fn run_cmd(&self, inputs: bool, extra: &[&str]) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.args(["sync", "run", "--no-notify", "--data-repo"])
            .arg(self.data())
            .arg("--cache-dir")
            .arg(self.dir.join("cache"))
            .arg("--agent-dir")
            .arg(self.dir.join("agent"));
        if inputs {
            for (id, _, _) in SOURCES {
                cmd.arg("--input")
                    .arg(format!("{id}={}", self.input(id).display()));
            }
        }
        cmd.args(extra);
        cmd
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(self.data())
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", text(&out.stderr));
        text(&out.stdout).trim().to_owned()
    }

    fn branches(&self) -> Vec<String> {
        self.git(&["branch", "--format=%(refname:short)"])
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// Accepts a sync branch the way merging its PR would.
    fn accept(&self, branch: &str) {
        self.git(&["merge", "--quiet", "--ff-only", branch]);
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn models(v: &mut Value) -> Vec<(String, String)> {
    let mut all = Vec::new();
    for (provider, p) in v.as_object().unwrap() {
        if let Some(m) = p.get("models").and_then(Value::as_object) {
            all.extend(m.keys().map(|id| (provider.clone(), id.clone())));
        }
    }
    all.sort();
    all
}

/// models.dev with the first `fraction` of its models removed.
fn shrunk_models_dev(bytes: &[u8], fraction: f64) -> Vec<u8> {
    let mut v: Value = serde_json::from_slice(bytes).unwrap();
    let all = models(&mut v);
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let cut = (all.len() as f64 * fraction) as usize;
    for (provider, id) in all.into_iter().take(cut) {
        v[&provider]["models"].as_object_mut().unwrap().remove(&id);
    }
    serde_json::to_vec(&v).unwrap()
}

/// models.dev with one model's limit and another's price changed, one added and one removed.
/// Returns the new bytes and the four `provider/id` keys `(limit, cost, added, removed)`.
fn tweaked_models_dev(bytes: &[u8]) -> (Vec<u8>, [String; 4]) {
    let mut v: Value = serde_json::from_slice(bytes).unwrap();
    let candidates: Vec<(String, String)> = models(&mut v)
        .into_iter()
        .filter(|(p, id)| {
            let m = &v[p]["models"][id];
            m["limit"]["context"].as_u64().is_some()
                && m["cost"]["input"].as_f64().is_some()
                && m["modalities"]["output"] == serde_json::json!(["text"])
        })
        .take(4)
        .collect();
    let [limit, cost, template, removed] = candidates.try_into().unwrap();
    let ctx = v[&limit.0]["models"][&limit.1]["limit"]["context"]
        .as_u64()
        .unwrap();
    v[&limit.0]["models"][&limit.1]["limit"]["context"] = (ctx + 1000).into();
    let price = v[&cost.0]["models"][&cost.1]["cost"]["input"]
        .as_f64()
        .unwrap();
    v[&cost.0]["models"][&cost.1]["cost"]["input"] = (price + 1.0).into();
    let mut added = v[&template.0]["models"][&template.1].clone();
    added["id"] = "tb-added-model".into();
    v[&template.0]["models"]["tb-added-model"] = added;
    v[&removed.0]["models"]
        .as_object_mut()
        .unwrap()
        .remove(&removed.1);
    let key = |(p, id): &(String, String)| format!("{p}/{id}");
    (
        serde_json::to_vec(&v).unwrap(),
        [
            key(&limit),
            key(&cost),
            format!("{}/tb-added-model", template.0),
            key(&removed),
        ],
    )
}

fn tree_state(env: &Env) -> (String, Vec<String>, String) {
    (
        env.git(&["rev-parse", "HEAD"]),
        env.branches(),
        env.git(&["status", "--porcelain", "--ignored"]),
    )
}

#[test]
fn init_lays_out_the_data_repo_and_refuses_to_run_twice() {
    let env = Env::new("init");
    env.init();
    for f in ["sync-state.json", "overrides.json", "README.md"] {
        assert!(env.data().join(f).is_file(), "{f}");
    }
    assert_eq!(
        fs::read_to_string(env.data().join("overrides.json"))
            .unwrap()
            .trim(),
        "{}"
    );
    assert_eq!(env.git(&["rev-list", "--count", "HEAD"]), "1");
    let again = Command::new(BIN)
        .args(["sync", "init", "--data-repo"])
        .arg(env.data())
        .output()
        .unwrap();
    assert!(!again.status.success());
    assert!(text(&again.stderr).contains("already initialized"));
}

#[test]
fn a_format_change_fails_loud_and_leaves_the_data_repo_untouched() {
    let env = Env::new("format");
    env.init();
    let before = tree_state(&env);
    let good = fs::read(env.input("models.dev")).unwrap();
    // A truncated download and a document of another shape: both must stop the run.
    for (name, bytes) in [
        ("truncated", good[..good.len() / 2].to_vec()),
        ("reshaped", br#"["not", "the", "api"]"#.to_vec()),
        ("empty object", b"{}".to_vec()),
    ] {
        fs::write(env.input("models.dev"), bytes).unwrap();
        let out = env.run(&["--no-push", "--now", NOW]);
        assert!(!out.status.success(), "{name}: the run must fail");
        let err = text(&out.stderr);
        assert!(
            err.contains("sync run FAILED"),
            "{name}: no log line: {err}"
        );
        assert_eq!(tree_state(&env), before, "{name}: the data repo changed");
        assert!(
            !env.branches().iter().any(|b| b.starts_with("sync/")),
            "{name}"
        );
    }
}

#[test]
fn a_source_that_shrinks_past_the_threshold_fails_loud() {
    let env = Env::new("shrink");
    env.init();
    let first = env.run(&["--no-push", "--now", NOW]);
    assert!(first.status.success(), "{}", text(&first.stderr));
    env.accept("sync/2026-11-27");
    let before = tree_state(&env);

    let good = fs::read(env.input("models.dev")).unwrap();
    fs::write(env.input("models.dev"), shrunk_models_dev(&good, 0.35)).unwrap();
    let out = env.run(&["--no-push", "--now", "2026-11-28T06:00:00Z"]);
    assert!(!out.status.success(), "a 35% shrink must fail");
    let err = text(&out.stderr);
    assert!(
        err.contains("shrink guard") && err.contains("models.dev"),
        "{err}"
    );
    assert_eq!(tree_state(&env), before);

    // The threshold is configurable.
    let lenient = env.run(&[
        "--no-push",
        "--now",
        "2026-11-28T06:00:00Z",
        "--max-shrink-percent",
        "50",
    ]);
    assert!(lenient.status.success(), "{}", text(&lenient.stderr));
    assert!(env.branches().iter().any(|b| b == "sync/2026-11-28"));
}

/// A `gh` that records its arguments and the PR body.
fn fake_gh(dir: &Path) -> PathBuf {
    let path = dir.join("gh");
    fs::write(
        &path,
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "gh fake"; exit 0; fi
echo "$@" > "$FAKE_GH_RECORD/args"
pwd > "$FAKE_GH_RECORD/cwd"
while [ $# -gt 0 ]; do
  if [ "$1" = "--body-file" ]; then cp "$2" "$FAKE_GH_RECORD/body.md"; fi
  shift
done
echo "https://example.invalid/blyzer/llm-tierbridge-data/pull/1"
"#,
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn init_run_merge_rerun_and_a_changed_input_end_to_end() {
    let env = Env::new("e2e");
    env.init_with_remote();
    let main_before = env.git(&["rev-parse", "main"]);

    // 1. first run: everything is new; branch + report + index + overrides, pushed, no PR.
    let out = env.run(&["--no-pr", "--now", NOW]);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{}\n{stdout}", text(&out.stderr));
    assert!(
        stdout.contains("pushed sync/2026-11-27 to origin"),
        "{stdout}"
    );
    assert_eq!(
        env.git(&["rev-parse", "main"]),
        main_before,
        "main must not move"
    );
    assert_eq!(env.git(&["branch", "--show-current"]), "main");
    assert!(env.git(&["status", "--porcelain"]).is_empty());
    let on_branch = |path: &str| env.git(&["show", &format!("sync/2026-11-27:{path}")]);
    let state: Value = serde_json::from_str(&on_branch("sync-state.json")).unwrap();
    assert_eq!(state["sources"].as_object().unwrap().len(), 8);
    assert_eq!(state["sources"]["dsh"]["cadence"], "on-change");
    assert_eq!(state["sources"]["modelparams.dev"]["cadence"], "weekly");
    let files = env.git(&["ls-tree", "-r", "--name-only", "sync/2026-11-27"]);
    for expected in [
        "index/models.dev.jsonl",
        "index/omp-live_easycliproxy.jsonl",
        "index/omp-live_dsh.jsonl",
        "index/router-for-me.models.jsonl",
        "overrides/omp.yml",
        "reports/2026-11-27.md",
        "reports/2026-11-27.json",
    ] {
        assert!(
            files.lines().any(|l| l == expected),
            "{expected} missing in\n{files}"
        );
    }
    let report = on_branch("reports/2026-11-27.md");
    assert!(
        report.contains("## Classified diff") && report.contains("**added ("),
        "{report}"
    );
    assert!(on_branch("overrides/omp.yml").contains("tierbridge export omp"));
    let bare = Command::new("git")
        .arg("--git-dir")
        .arg(env.remote())
        .args(["branch", "--list", "sync/*"])
        .output()
        .unwrap();
    assert!(
        text(&bare.stdout).contains("sync/2026-11-27"),
        "branch not on the remote"
    );

    // 2. same inputs, PR not merged yet: nothing new.
    let again = env.run(&["--no-pr", "--now", NOW]);
    assert!(again.status.success(), "{}", text(&again.stderr));
    assert!(text(&again.stdout).contains("already proposed in branch sync/2026-11-27"));
    assert_eq!(env.branches().len(), 2);

    // 3. accept it (the PR merge): same inputs again, nothing to sync, no branch.
    env.accept("sync/2026-11-27");
    let quiet = env.run(&["--now", "2026-11-28T06:00:00Z", "--no-pr"]);
    assert!(quiet.status.success(), "{}", text(&quiet.stderr));
    assert!(
        text(&quiet.stdout).contains("nothing to sync"),
        "{}",
        text(&quiet.stdout)
    );
    assert_eq!(env.branches().len(), 2);

    // ... and without --input every source is reused from the cache (nothing is due yet).
    let reused = env.run_with(false, &["--no-push", "--source", "models.dev"]);
    let reused_out = text(&reused.stdout);
    assert!(
        reused.status.success(),
        "{}\n{reused_out}",
        text(&reused.stderr)
    );
    assert!(reused_out.contains("nothing to sync"));
    assert_eq!(reused_out.matches(" reused ").count(), 8, "{reused_out}");

    // 4. a changed input: a classified diff, a second branch the same day, and the PR.
    let md = fs::read(env.input("models.dev")).unwrap();
    let (changed, [limit, cost, added, removed]) = tweaked_models_dev(&md);
    fs::write(env.input("models.dev"), changed).unwrap();
    let rec = env.dir.join("gh-record");
    fs::create_dir_all(&rec).unwrap();
    let gh = fake_gh(&env.dir);
    let out = env
        .run_cmd(true, &["--now", NOW])
        .env("TIERBRIDGE_GH", &gh)
        .env("FAKE_GH_RECORD", &rec)
        .output()
        .unwrap();
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{}\n{stdout}", text(&out.stderr));
    assert!(stdout.contains("PR https://example.invalid/"), "{stdout}");
    assert!(
        env.branches().iter().any(|b| b == "sync/2026-11-27-2"),
        "{:?}",
        env.branches()
    );
    let report = env.git(&["show", "sync/2026-11-27-2:reports/2026-11-27-2.md"]);
    for needle in [
        "**added (1)**",
        "**removed (1)**",
        "**limit changed (1)**",
        "**cost changed (1)**",
        &format!("`{limit}`: contextWindow"),
        &format!("`{cost}`: cost"),
        &format!("`{added}`"),
        &format!("`{removed}`"),
    ] {
        assert!(report.contains(needle), "{needle} missing in\n{report}");
    }
    let json: Value =
        serde_json::from_str(&env.git(&["show", "sync/2026-11-27-2:reports/2026-11-27-2.json"]))
            .unwrap();
    let models_dev = json["diffs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["source"] == "models.dev")
        .unwrap();
    assert_eq!(models_dev["counts"]["limit-changed"], 1);
    // The commit only touched what the change touched.
    let touched = env.git(&["diff", "--name-only", "main", "sync/2026-11-27-2"]);
    assert!(touched.lines().any(|l| l == "index/models.dev.jsonl"));
    assert!(
        !touched.lines().any(|l| l == "index/dsh.jsonl"),
        "{touched}"
    );
    // gh got the title, the branch and the report as body.
    let args = fs::read_to_string(rec.join("args")).unwrap();
    assert!(
        args.contains(
            "pr create --base main --head sync/2026-11-27-2 --title sync 2026-11-27: +1 −1 ~2"
        ),
        "{args}"
    );
    assert_eq!(
        fs::read_to_string(rec.join("body.md")).unwrap().trim_end(),
        report
    );
    assert!(
        fs::read_to_string(rec.join("cwd"))
            .unwrap()
            .contains("data")
    );
    assert_eq!(env.git(&["branch", "--show-current"]), "main");
}

#[test]
fn bad_arguments_fail_before_anything_happens() {
    let env = Env::new("args");
    env.init();
    let before = tree_state(&env);
    for (extra, message) in [
        (
            &["--no-push", "--source", "nope"][..],
            "not a synced source",
        ),
        (&["--no-push", "--input", "dsh"], "expected SOURCE=FILE"),
        (
            &["--no-push", "--source", "omp-bundled"],
            "not a synced source",
        ),
    ] {
        let out = env.run_with(false, extra);
        assert!(!out.status.success(), "{extra:?}");
        assert!(
            text(&out.stderr).contains(message),
            "{extra:?}: {}",
            text(&out.stderr)
        );
    }
    // pushing without an origin is refused up front.
    let out = env.run(&["--no-pr"]);
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("no `origin` remote"));
    assert_eq!(tree_state(&env), before);
    // a dirty data repo is refused too.
    fs::write(env.data().join("stray.txt"), "x").unwrap();
    let out = env.run(&["--no-push"]);
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("uncommitted changes"));
}

const MODELS_YML: &str = "\
# comments survive
providers:
  easycliproxy:
    baseUrl: http://127.0.0.1:8317/v1
    apiKey: sk-not-a-real-key
    discovery:
      type: openai-models-list

  dsh:
    baseUrl: http://127.0.0.1:8326/v1
";

fn apply_cmd(data: &Path, models: &Path, extra: &[&str]) -> Output {
    Command::new(BIN)
        .args(["sync", "apply", "--data-repo"])
        .arg(data)
        .arg("--models-yml")
        .arg(models)
        .args(extra)
        .output()
        .unwrap()
}

#[test]
fn apply_pulls_the_accepted_overrides_and_writes_them_into_models_yml() {
    let env = Env::new("apply");
    env.init_with_remote();
    // A second clone, the one on the machine that runs omp: it starts without any overrides.
    let clone = env.dir.join("clone");
    let out = Command::new("git")
        .args(["clone", "--quiet"])
        .arg(env.remote())
        .arg(&clone)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    let models = env.dir.join("models.yml");
    fs::write(&models, MODELS_YML).unwrap();

    let none = apply_cmd(&clone, &models, &[]);
    assert!(!none.status.success());
    assert!(
        text(&none.stderr).contains("merge its PR first"),
        "{}",
        text(&none.stderr)
    );

    // The sync machine proposes, the PR is merged and main pushed.
    let run = env.run(&["--no-pr", "--now", NOW]);
    assert!(run.status.success(), "{}", text(&run.stderr));
    env.accept("sync/2026-11-27");
    env.git(&["push", "--quiet", "origin", "main"]);

    let before = fs::read_to_string(&models).unwrap();
    let dry = apply_cmd(&clone, &models, &["--dry-run"]);
    assert!(dry.status.success(), "{}", text(&dry.stderr));
    assert!(
        text(&dry.stdout).contains("dry run"),
        "{}",
        text(&dry.stdout)
    );
    assert_eq!(
        fs::read_to_string(&models).unwrap(),
        before,
        "dry run wrote"
    );
    assert!(
        clone.join("overrides/omp.yml").is_file(),
        "the dry run pulls the data repo"
    );

    let out = apply_cmd(&clone, &models, &[]);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{}\n{stdout}", text(&out.stderr));
    assert!(
        stdout.contains("git pull --ff-only origin main"),
        "{stdout}"
    );
    assert!(clone.join("overrides/omp.yml").is_file());
    let after = fs::read_to_string(&models).unwrap();
    assert!(
        after.contains("tierbridge-managed modelOverrides"),
        "{after}"
    );
    assert!(after.starts_with("# comments survive"));
    let backups = fs::read_dir(&env.dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("models.yml."))
        .count();
    assert_eq!(backups, 1, "one backup next to models.yml");

    let again = apply_cmd(&clone, &models, &[]);
    assert!(again.status.success());
    assert!(
        text(&again.stdout).contains("already up to date"),
        "{}",
        text(&again.stdout)
    );
    assert_eq!(fs::read_to_string(&models).unwrap(), after);

    // Only accepted overrides: a feature branch is refused.
    let out = Command::new("git")
        .arg("-C")
        .arg(&clone)
        .args(["checkout", "--quiet", "-b", "scratch"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let refused = apply_cmd(&clone, &models, &[]);
    assert!(!refused.status.success());
    assert!(text(&refused.stderr).contains("not `main`"));
}

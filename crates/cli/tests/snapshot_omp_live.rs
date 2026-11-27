//! `tierbridge snapshot omp-live` against a fake `omp` script: it must hand omp a scratch agent
//! dir whose `models.yml` has no managed blocks, never touch the real one, hide secrets and clean up.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Command, Output};

const MODELS: &str = "\
providers:
  gw:
    apiKey: sk-not-a-real-key-0123456789abcdef
    # >>> tierbridge-managed modelOverrides (do not edit) >>>
    modelOverrides:
      m:
        contextWindow: 1
    # <<< tierbridge-managed modelOverrides <<<
";

const FAKE_OMP: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "omp/19.2.1"; exit 0; fi
echo "$PI_CODING_AGENT_DIR" > "$FAKE_RECORD/dir"
ls -A "$PI_CODING_AGENT_DIR" > "$FAKE_RECORD/files"
cp "$PI_CODING_AGENT_DIR/models.yml" "$FAKE_RECORD/models.yml"
echo "$@" > "$FAKE_RECORD/args"
if [ -n "$FAKE_FAIL" ]; then
  echo "boom: auth token sk-live-0123456789abcdefghijkl rejected" >&2
  exit 7
fi
echo "Warning: grok model discovery failed" >&2
printf '{"models":[{"provider":"%s","id":"m","contextWindow":1}]}' "$2"
"#;

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("tierbridge-snap-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for d in ["agent", "record"] {
            fs::create_dir_all(dir.join(d)).unwrap();
        }
        fs::write(dir.join("agent/models.yml"), MODELS).unwrap();
        fs::write(dir.join("omp"), FAKE_OMP).unwrap();
        fs::set_permissions(dir.join("omp"), fs::Permissions::from_mode(0o755)).unwrap();
        Self(dir)
    }

    fn run(&self, fail: bool) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tierbridge"));
        cmd.args(["snapshot", "omp-live", "--provider", "gw", "--omp"])
            .arg(self.0.join("omp"))
            .arg("--agent-dir")
            .arg(self.0.join("agent"))
            .env("FAKE_RECORD", self.0.join("record"));
        if fail {
            cmd.env("FAKE_FAIL", "1");
        }
        cmd.output().unwrap()
    }

    fn record(&self, name: &str) -> String {
        fs::read_to_string(self.0.join("record").join(name)).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn runs_omp_on_a_stripped_copy_and_removes_it() {
    let s = Scratch::new("ok");
    let out = s.run(false);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    // stdout is the roster, ready for `catalog build --source omp-live:gw --input`
    let roster: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(roster["models"][0]["provider"], "gw");
    assert!(stderr.contains("1 managed block(s) stripped"), "{stderr}");

    // omp saw a scratch dir holding only models.yml, without the block, and the right command
    let seen = s.record("models.yml");
    assert!(
        !seen.contains("tierbridge-managed") && !seen.contains("modelOverrides"),
        "{seen}"
    );
    assert!(
        seen.contains("apiKey: sk-not-a-real-key"),
        "everything else is copied"
    );
    assert_eq!(s.record("files").trim(), "models.yml");
    assert_eq!(s.record("args").trim(), "models gw --json --no-extensions");
    let tmp = PathBuf::from(s.record("dir").trim());
    assert!(!tmp.exists(), "{} was not removed", tmp.display());
    assert_ne!(tmp, s.0.join("agent"));

    // the real agent dir is untouched
    assert_eq!(
        fs::read_to_string(s.0.join("agent/models.yml")).unwrap(),
        MODELS
    );
    assert_eq!(fs::read_dir(s.0.join("agent")).unwrap().count(), 1);
}

#[test]
fn a_failing_omp_is_an_error_without_secrets_and_still_cleans_up() {
    let s = Scratch::new("fail");
    let out = s.run(true);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("failed"), "{stderr}");
    assert!(
        stderr.contains("<redacted>") && !stderr.contains("sk-live-"),
        "{stderr}"
    );
    assert!(!stderr.contains("not-a-real-key"), "{stderr}");
    assert!(out.stdout.is_empty());
    assert!(!PathBuf::from(s.record("dir").trim()).exists());
}

#[test]
fn refuses_a_missing_omp_and_odd_provider_names() {
    let s = Scratch::new("bad");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tierbridge"))
            .args(["snapshot", "omp-live"])
            .args(args)
            .arg("--agent-dir")
            .arg(s.0.join("agent"))
            .output()
            .unwrap()
    };
    let out = run(&["--provider", "gw", "--omp", "/nonexistent/omp"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("not found"));
    let omp = s.0.join("omp");
    for bad in ["refresh", "-x", "a b", ""] {
        let out = run(&["--provider", bad, "--omp", omp.to_str().unwrap()]);
        assert!(!out.status.success(), "{bad:?}");
    }
}

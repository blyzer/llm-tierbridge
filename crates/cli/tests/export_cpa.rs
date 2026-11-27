//! `tierbridge export cpa` end to end: merge the committed snapshots, then export a gateway.

mod common;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

/// Scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("tierbridge-export-{}-{name}", std::process::id()));
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

fn tierbridge(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tierbridge"));
    command.args(args).env_remove("TB_TEST_KEY");
    for (k, v) in env {
        command.env(k, v);
    }
    command.output().unwrap()
}

/// `catalog.json` merged from models.dev and the easycliproxy `omp models` snapshot.
fn merged_catalog(scratch: &Scratch) -> PathBuf {
    let md = scratch.path("md.json");
    let easy = scratch.path("easy.json");
    fs::write(&md, fixture("models.dev.api.2026-11-26.json.zst")).unwrap();
    fs::write(
        &easy,
        fixture("omp-models-easycliproxy.2026-11-26.json.zst"),
    )
    .unwrap();
    let out_dir = scratch.path("out");
    let out = tierbridge(
        &[
            "catalog",
            "build",
            "--source",
            "models.dev",
            "--input",
            md.to_str().unwrap(),
            "--source",
            "omp-live:easycliproxy",
            "--input",
            easy.to_str().unwrap(),
            "--fetched-at",
            "2026-11-26T08:32:56Z",
            "--merge",
            "--out-dir",
            out_dir.to_str().unwrap(),
        ],
        &[],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out_dir.join("catalog.json")
}

fn export_args<'a>(catalog: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "export",
        "cpa",
        "--catalog",
        catalog,
        "--gateway",
        "omp-live:easycliproxy",
        "--base-url",
        "http://127.0.0.1:8317/v1",
    ];
    args.extend_from_slice(extra);
    args
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn exports_every_live_row_of_the_gateway_and_reports_what_it_skipped() {
    let scratch = Scratch::new("stdout");
    let catalog = merged_catalog(&scratch);
    let catalog = catalog.to_str().unwrap();
    let out = tierbridge(&export_args(catalog, &[]), &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let yaml = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);

    let items = yaml.matches("        - name: ").count();
    assert!(items > 100, "only {items} models");
    assert_eq!(items, yaml.matches("          alias: \"tb-").count());
    assert!(
        stderr.contains(&format!("exported {items} models")),
        "{stderr}"
    );
    assert!(stderr.contains("warning: no --api-key-env"), "{stderr}");
    assert!(yaml.contains("      base-url: \"http://127.0.0.1:8317/v1\"\n"));
    assert!(yaml.contains("    - name: \"tierbridge-omp-live-easycliproxy\"\n"));
    // the point of the export: the limit the merge resolved (models.dev's 512000), not the 128000
    // filler the gateway reports for this model
    assert!(yaml.contains(
        "        - name: \"agnes-2.0-flash\"\n          alias: \"tb-agnes-2.0-flash\"\n          max-context-length: 512000\n"
    ));
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn api_key_comes_from_the_environment_into_a_private_file() {
    let scratch = Scratch::new("file");
    let catalog = merged_catalog(&scratch);
    let catalog = catalog.to_str().unwrap();
    let target = scratch.path("cpa.yaml");
    let target_str = target.to_str().unwrap();
    let out = tierbridge(
        &export_args(
            catalog,
            &[
                "--api-key-env",
                "TB_TEST_KEY",
                "--out",
                target_str,
                "--prefix",
                "tb",
                "--name",
                "mine",
            ],
        ),
        &[("TB_TEST_KEY", "sk-secret-value")],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("sk-secret-value"));
    assert!(!stderr.contains("warning"), "{stderr}");
    assert!(out.stdout.is_empty());
    let yaml = fs::read_to_string(&target).unwrap();
    assert!(yaml.contains("        - api-key: \"sk-secret-value\"\n"));
    assert!(yaml.contains("    - name: \"mine\"\n      prefix: \"tb\"\n"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn an_existing_world_readable_output_file_is_tightened() {
    let scratch = Scratch::new("tighten");
    let catalog = merged_catalog(&scratch);
    let target = scratch.path("cpa.yaml");
    fs::write(&target, "old").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    }
    let out = tierbridge(
        &export_args(
            catalog.to_str().unwrap(),
            &["--out", target.to_str().unwrap()],
        ),
        &[],
    );
    assert!(out.status.success());
    assert!(
        fs::read_to_string(&target)
            .unwrap()
            .starts_with("# Generated")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn key_variable_must_be_set_and_needs_an_output_file() {
    let scratch = Scratch::new("errors");
    let catalog = merged_catalog(&scratch);
    let catalog = catalog.to_str().unwrap();
    let target = scratch.path("cpa.yaml");

    let unset = tierbridge(
        &export_args(
            catalog,
            &[
                "--api-key-env",
                "TB_TEST_KEY",
                "--out",
                target.to_str().unwrap(),
            ],
        ),
        &[],
    );
    assert_eq!(unset.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&unset.stderr).contains("`TB_TEST_KEY` is unset or empty"));
    assert!(!target.exists());

    // the key is never printed: without --out the flag is a usage error
    let to_stdout = tierbridge(
        &export_args(catalog, &["--api-key-env", "TB_TEST_KEY"]),
        &[("TB_TEST_KEY", "sk-secret-value")],
    );
    assert_eq!(to_stdout.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&to_stdout.stdout).contains("sk-secret-value"));
}

#[test]
#[ignore = "private fixtures: local-fixtures/ (README, \"Pruebas\")"]
fn unknown_gateway_and_missing_catalog_fail_with_exit_code_1() {
    let scratch = Scratch::new("fail");
    let catalog = merged_catalog(&scratch);
    let out = tierbridge(
        &[
            "export",
            "cpa",
            "--catalog",
            catalog.to_str().unwrap(),
            "--gateway",
            "dsh",
            "--base-url",
            "http://x/v1",
        ],
        &[],
    );
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("gateways present: omp-live:easycliproxy"),
        "{stderr}"
    );

    let missing = tierbridge(&export_args("/nonexistent/catalog.json", &[]), &[]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("cannot read"));
}

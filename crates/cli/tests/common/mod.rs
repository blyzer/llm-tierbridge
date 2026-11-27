#![allow(dead_code)]
//! Helpers shared by the end-to-end tests. Include with `mod common;`.

use std::path::{Path, PathBuf};

/// Where the private rosters live (not published): `$TIERBRIDGE_LOCAL_FIXTURES`, else
/// `local-fixtures/` at the workspace root. Tests that need them are `#[ignore]`d and run with
/// `cargo test -- --include-ignored` (README, "Pruebas").
pub fn local_fixtures_dir() -> PathBuf {
    match std::env::var_os("TIERBRIDGE_LOCAL_FIXTURES") {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local-fixtures"),
    }
}

/// A published fixture (`crates/catalog/tests/fixtures/`) or, failing that, a private one
/// (`local-fixtures/`). A missing private fixture fails with a hint, not a bare `NotFound`.
pub fn fixture_path(name: &str) -> PathBuf {
    let public = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../catalog/tests/fixtures")
        .join(name);
    let path = if public.exists() {
        public
    } else {
        local_fixtures_dir().join(name)
    };
    assert!(
        path.exists(),
        "{}: missing\nprivate fixture: put it in local-fixtures/ or point TIERBRIDGE_LOCAL_FIXTURES at it (README, \"Pruebas\")",
        path.display()
    );
    path
}

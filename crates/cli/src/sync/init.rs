//! `tierbridge sync init`: creates the private data repo with its layout.

use std::fs;
use std::path::PathBuf;

use clap::Args;

use super::git::{self, Repo};
use super::state::{STATE_FILE, SyncState};
use crate::{Failure, failure};

/// The hand-written overrides file (design §3); `{}` at init.
pub const OVERRIDES_FILE: &str = "overrides.json";

const DATA_README: &str = "\
# llm-tierbridge data

Private data repo of [llm-tierbridge](https://github.com/blyzer/llm-tierbridge). Written by
`tierbridge sync run`; every change arrives as a reviewable PR (`sync/<date>`). Nothing here applies
itself: `tierbridge sync apply` does, by hand.

- `sync-state.json`: per source, what `main` has accepted (revision, sha256, count, cadence).
- `index/<sourceId>.jsonl`: one sorted line per model a source lists. This is what `git diff` shows.
- `overrides/omp.yml`: the generated `modelOverrides` for omp (`tierbridge export omp`).
- `reports/<date>.md` / `.json`: the classified diff and run summary of each sync (also the PR body).
- `overrides.json`: hand-written `configured` overrides that win over every source (`{}` = none).

Raw downloads and the merged `catalog.json` are never committed; they live in
`~/.cache/llm-tierbridge/`.
";

#[derive(Args)]
pub struct InitArgs {
    /// Directory of the data repo (created; it must not exist or be empty).
    #[arg(long, value_name = "DIR")]
    pub data_repo: PathBuf,
    /// Add this URL as the `origin` remote (nothing is pushed).
    #[arg(long, value_name = "URL")]
    pub remote: Option<String>,
}

pub fn init(args: &InitArgs) -> Result<(), Failure> {
    let path = &args.data_repo;
    if path.exists() {
        let empty = path.is_dir()
            && fs::read_dir(path)
                .map_err(|e| failure(format!("cannot read {}: {e}", path.display())))?
                .next()
                .is_none();
        if !empty {
            let what = if path.join(STATE_FILE).exists() {
                "already initialized"
            } else {
                "exists and is not empty"
            };
            return Err(failure(format!("{}: {what}", path.display())));
        }
    }
    git::init(path, "main")?;
    let write = |name: &str, bytes: &[u8]| {
        fs::write(path.join(name), bytes)
            .map_err(|e| failure(format!("cannot write {}: {e}", path.join(name).display())))
    };
    write(STATE_FILE, &SyncState::default().to_bytes())?;
    write(OVERRIDES_FILE, b"{}\n")?;
    write("README.md", DATA_README.as_bytes())?;
    let repo = Repo::new(path);
    repo.run(&["add", "--", STATE_FILE, OVERRIDES_FILE, "README.md"])?;
    repo.commit("sync init: empty state")?;
    if let Some(url) = &args.remote {
        repo.run(&["remote", "add", "origin", url])?;
    }
    println!("initialized {}", path.display());
    println!("  {STATE_FILE}, {OVERRIDES_FILE}, README.md committed on main");
    match &args.remote {
        Some(url) => println!("  remote origin -> {url} (nothing pushed)"),
        None => {
            println!(
                "  no remote: add one with `git remote add origin <url>` before the first push"
            );
        }
    }
    Ok(())
}

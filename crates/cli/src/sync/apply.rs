//! `tierbridge sync apply`: bring the accepted `overrides/omp.yml` into omp's `models.yml`.
//! Manual by design: nothing in the pipeline applies itself.

use std::path::PathBuf;

use clap::Args;

use super::git::Repo;
use super::run::OMP_OVERRIDES_PATH;
use super::state::STATE_FILE;
use crate::omp::{self, ApplyOmpArgs};
use crate::{Failure, failure};

/// The branch whose content is accepted (a merged sync PR lands here).
const ACCEPTED_BRANCH: &str = "main";

#[derive(Args)]
pub struct ApplyArgs {
    /// The private data repo (a clone is fine: it is pulled).
    #[arg(long, value_name = "DIR")]
    data_repo: PathBuf,
    /// omp's `models.yml` (edited in place; a backup is written next to it first).
    #[arg(long, value_name = "FILE")]
    models_yml: PathBuf,
    /// Show what would change in `models.yml`; write nothing there (no backup either). The data
    /// repo is still pulled (fast-forward only), so the preview is of the latest accepted overrides.
    #[arg(long)]
    dry_run: bool,
}

pub fn apply(args: &ApplyArgs) -> Result<(), Failure> {
    let repo = Repo::new(&args.data_repo);
    if !args.data_repo.join(STATE_FILE).is_file() || !repo.is_toplevel() {
        return Err(failure(format!(
            "{}: not a data repo",
            args.data_repo.display()
        )));
    }
    let branch = repo.current_branch()?;
    if branch != ACCEPTED_BRANCH {
        return Err(failure(format!(
            "{}: on `{branch}`, not `{ACCEPTED_BRANCH}`; only accepted (merged) overrides are applied",
            args.data_repo.display()
        )));
    }
    if repo.has_remote("origin") {
        if !repo.is_clean()? {
            return Err(failure(format!(
                "{}: uncommitted changes; cannot pull",
                args.data_repo.display()
            )));
        }
        repo.run(&["pull", "--quiet", "--ff-only", "origin", ACCEPTED_BRANCH])?;
        println!(
            "git pull --ff-only origin {ACCEPTED_BRANCH}: now at {}",
            repo.run(&["rev-parse", "--short", "HEAD"])?
        );
    } else {
        println!("no `origin` remote: nothing to pull");
    }
    let overrides = args.data_repo.join(OMP_OVERRIDES_PATH);
    if !overrides.is_file() {
        return Err(failure(format!(
            "{} does not exist: run `tierbridge sync run` and merge its PR first",
            overrides.display()
        )));
    }
    omp::apply(&ApplyOmpArgs {
        models_yml: args.models_yml.clone(),
        overrides: Some(overrides),
        revert: false,
        dry_run: args.dry_run,
    })
}

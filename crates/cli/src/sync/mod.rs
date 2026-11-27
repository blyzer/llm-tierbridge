//! `tierbridge sync`: the pipeline that keeps the private data repo up to date (design §3 a–e,
//! §6 item 4). `init` creates the repo, `run` fetches, validates, classifies and opens a
//! reviewable PR, `apply` brings the accepted overrides into omp, `install-launchd` schedules it.

mod apply;
mod cache;
mod diff;
mod fetch;
mod git;
mod index;
mod init;
mod launchd;
mod report;
mod run;
mod sources;
mod state;

use clap::Subcommand;

use crate::Failure;

#[derive(Subcommand)]
pub enum SyncCommand {
    /// Create the private data repo (state, overrides.json, README) with one commit on `main`.
    Init(init::InitArgs),
    /// Fetch the due sources, validate, classify the diff, regenerate the omp overrides and
    /// propose them as a branch `sync/<date>` + PR. Fails loud (data repo untouched) on a format
    /// change or a shrinking source.
    Run(run::RunArgs),
    /// `git pull --ff-only` the data repo, then apply its `overrides/omp.yml` to omp's `models.yml`
    /// (backup, idempotent). The manual step: nothing applies itself.
    Apply(apply::ApplyArgs),
    /// Write the daily launchd job (`~/Library/LaunchAgents/dev.blyzer.llm-tierbridge.sync.plist`)
    /// and its Keychain-loading wrapper, then load it. `--dry-run` only prints them.
    InstallLaunchd(launchd::InstallArgs),
    /// Unload and remove the launchd job and its wrapper (the logs stay).
    UninstallLaunchd(launchd::UninstallArgs),
}

pub fn dispatch(command: &SyncCommand) -> Result<(), Failure> {
    match command {
        SyncCommand::Init(args) => init::init(args),
        SyncCommand::Run(args) => run::run(args),
        SyncCommand::Apply(args) => apply::apply(args),
        SyncCommand::InstallLaunchd(args) => launchd::install(args),
        SyncCommand::UninstallLaunchd(args) => launchd::uninstall(args),
    }
}

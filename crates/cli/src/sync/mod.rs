//! `tierbridge sync`: the pipeline that keeps the private data repo up to date (design §3 a–e,
//! §6 item 4). `init` creates the repo, `run` fetches, validates, classifies and opens a
//! reviewable PR, `apply` brings the accepted overrides into omp, `install-launchd` schedules it.

mod diff;
mod git;
mod index;
mod init;
mod state;

use clap::Subcommand;

use crate::Failure;

#[derive(Subcommand)]
pub enum SyncCommand {
    /// Create the private data repo (state, overrides.json, README) with one commit on `main`.
    Init(init::InitArgs),
}

pub fn dispatch(command: &SyncCommand) -> Result<(), Failure> {
    match command {
        SyncCommand::Init(args) => init::init(args),
    }
}

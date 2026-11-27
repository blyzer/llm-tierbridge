//! Thin wrapper over the `git` binary for the data repo. Every command is `git -C <repo> ...`
//! with prompts disabled, so an unattended run fails instead of hanging.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::{Failure, failure};

pub struct Repo {
    pub path: PathBuf,
}

/// First lines of a command's stderr, for error messages.
pub fn first_lines(bytes: &[u8], n: usize) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(n)
        .collect::<Vec<_>>()
        .join(" | ")
}

impl Repo {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.path)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C");
        cmd
    }

    /// Runs git; only a failure to start it is an `Err`.
    pub fn output(&self, args: &[&str]) -> Result<Output, Failure> {
        self.command(args)
            .output()
            .map_err(|e| failure(format!("could not run git: {e}")))
    }

    /// Runs git and returns its trimmed stdout; a non-zero exit is an error carrying stderr.
    pub fn run(&self, args: &[&str]) -> Result<String, Failure> {
        let out = self.output(args)?;
        if !out.status.success() {
            return Err(failure(format!(
                "git {} failed ({}): {}",
                args.first().copied().unwrap_or(""),
                out.status,
                first_lines(&out.stderr, 3)
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    pub fn succeeds(&self, args: &[&str]) -> bool {
        self.output(args).is_ok_and(|o| o.status.success())
    }

    /// The path is the top level of a git work tree (not just somewhere inside one).
    pub fn is_toplevel(&self) -> bool {
        let Ok(top) = self.run(&["rev-parse", "--show-toplevel"]) else {
            return false;
        };
        match (
            std::fs::canonicalize(top),
            std::fs::canonicalize(&self.path),
        ) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }

    pub fn current_branch(&self) -> Result<String, Failure> {
        self.run(&["symbolic-ref", "--short", "HEAD"])
            .map_err(|_| failure("the data repo is on a detached HEAD: check out a branch first"))
    }

    pub fn is_clean(&self) -> Result<bool, Failure> {
        Ok(self.run(&["status", "--porcelain"])?.is_empty())
    }

    pub fn has_remote(&self, name: &str) -> bool {
        self.run(&["remote"])
            .map(|r| r.lines().any(|l| l == name))
            .unwrap_or(false)
    }

    pub fn branch_exists(&self, name: &str) -> bool {
        self.succeeds(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{name}"),
        ])
    }

    /// `-c` arguments that give a commit an author when none is configured, and never sign.
    fn commit_config(&self) -> Vec<String> {
        let mut args = vec!["-c".to_owned(), "commit.gpgsign=false".to_owned()];
        for (key, fallback) in [
            ("user.name", "llm-tierbridge"),
            ("user.email", "tierbridge@localhost"),
        ] {
            let set = self
                .run(&["config", "--get", key])
                .is_ok_and(|v| !v.is_empty());
            if !set {
                args.push("-c".to_owned());
                args.push(format!("{key}={fallback}"));
            }
        }
        args
    }

    /// Commits what is staged.
    pub fn commit(&self, message: &str) -> Result<(), Failure> {
        let config = self.commit_config();
        let mut args: Vec<&str> = config.iter().map(String::as_str).collect();
        args.extend(["commit", "--quiet", "-m", message]);
        self.run(&args).map(|_| ())
    }

    /// The content of `path` at `rev`, `None` when it does not exist there.
    pub fn show(&self, rev: &str, path: &str) -> Option<Vec<u8>> {
        let out = self.output(&["show", &format!("{rev}:{path}")]).ok()?;
        out.status.success().then_some(out.stdout)
    }
}

/// Runs `git init -b <branch> <path>` (creating the directory).
pub fn init(path: &Path, branch: &str) -> Result<(), Failure> {
    let out = Command::new("git")
        .arg("init")
        .arg("--quiet")
        .arg("-b")
        .arg(branch)
        .arg(path)
        .output()
        .map_err(|e| failure(format!("could not run git: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(failure(format!(
            "git init {} failed: {}",
            path.display(),
            first_lines(&out.stderr, 3)
        )))
    }
}

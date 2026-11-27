//! Taking a fresh snapshot of one source (step a of design §3): an HTTP download, omp's own cache,
//! `snapshot omp-live`, or a shallow clone plus the node script that builds the envelope. Every
//! snapshot is stored in the cache under its sha256 before anything is built from it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use super::cache::{Cache, Latest};
use super::git::first_lines;
use super::sources::{GitSpec, GitTool, Origin, SourceSpec};
use crate::omp::{self, SnapshotOmpLiveArgs};
use crate::{Failure, failure, ms_since_epoch};

const GIT_TIMEOUT_SECS: u64 = 900;
const TOOL_TIMEOUT_SECS: u64 = 900;
const LS_REMOTE_TIMEOUT_SECS: u64 = 120;

/// What the fetchers need from the machine they run on.
pub struct Ctx<'a> {
    pub cache: &'a Cache,
    pub tools_dir: PathBuf,
    pub node: PathBuf,
    pub agent_dir: PathBuf,
    pub omp: Option<PathBuf>,
    pub omp_timeout_secs: u64,
    pub now_ms: i64,
}

/// `--tools-dir`, else `$TIERBRIDGE_TOOLS_DIR`, else the `tools/` of the checkout this binary was
/// built from.
pub fn tools_dir(arg: Option<&Path>) -> PathBuf {
    arg.map(Path::to_path_buf)
        .or_else(|| std::env::var_os("TIERBRIDGE_TOOLS_DIR").map(PathBuf::from))
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools"))
}

/// `$TIERBRIDGE_NODE`, else `node` from `PATH`.
pub fn node_binary() -> PathBuf {
    std::env::var_os("TIERBRIDGE_NODE").map_or_else(|| PathBuf::from("node"), PathBuf::from)
}

/// `--agent-dir`, else `$PI_CODING_AGENT_DIR`, else `~/.omp/agent`. Only read from, never written.
pub fn agent_dir(arg: Option<&Path>) -> Result<PathBuf, Failure> {
    arg.map(Path::to_path_buf)
        .or_else(|| std::env::var_os("PI_CODING_AGENT_DIR").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".omp/agent")))
        .ok_or_else(|| failure("no agent directory: pass --agent-dir"))
}

pub struct Fetched {
    pub bytes: Vec<u8>,
    pub latest: Latest,
}

/// Runs a command to completion; a failure carries the first lines of stderr.
fn run(cmd: Command, what: &str, secs: u64) -> Result<Vec<u8>, Failure> {
    let (status, out, err) = omp::run_with_timeout(cmd, Duration::from_secs(secs))
        .map_err(|e| failure(format!("{what}: {e}")))?;
    if status.success() {
        Ok(out)
    } else {
        Err(failure(format!(
            "{what} failed ({status}): {}",
            first_lines(&err, 3)
        )))
    }
}

fn git_command(dir: Option<&Path>) -> Command {
    let mut cmd = Command::new("git");
    if let Some(dir) = dir {
        cmd.arg("-C").arg(dir);
    }
    cmd.env("GIT_TERMINAL_PROMPT", "0").env("LC_ALL", "C");
    cmd
}

fn short(sha256: &str) -> String {
    format!("sha256:{}", &sha256[..16])
}

/// Stores the bytes and builds their [`Latest`] record.
fn finish(
    cache: &Cache,
    bytes: Vec<u8>,
    revision: Option<String>,
    fetched_at_ms: i64,
) -> Result<Fetched, Failure> {
    let sha256 = cache.store_raw(&bytes)?;
    Ok(Fetched {
        latest: Latest {
            revision: revision.unwrap_or_else(|| short(&sha256)),
            sha256,
            fetched_at_ms,
            url: None,
            etag: None,
            last_modified: None,
        },
        bytes,
    })
}

/// The commit the upstream `HEAD` points at now (one `git ls-remote`, no clone).
pub fn upstream_revision(g: &GitSpec) -> Result<String, Failure> {
    let mut cmd = git_command(None);
    cmd.args(["ls-remote", g.url, "HEAD"]);
    let out = run(
        cmd,
        &format!("git ls-remote {}", g.url),
        LS_REMOTE_TIMEOUT_SECS,
    )?;
    String::from_utf8_lossy(&out)
        .split_whitespace()
        .next()
        .filter(|s| s.len() >= 40)
        .map(str::to_owned)
        .ok_or_else(|| failure(format!("git ls-remote {}: no HEAD", g.url)))
}

/// Clones (or updates) the checkout under `<cache>/repos/` and returns it with its pinned commit.
fn checkout(g: &GitSpec, repos: &Path) -> Result<(PathBuf, String), Failure> {
    let dir = repos.join(g.name);
    let depth = g.depth.map(|d| d.to_string());
    let reference = if dir.join(".git").exists() {
        let mut cmd = git_command(Some(&dir));
        cmd.args(["fetch", "--quiet", "--no-tags"]);
        if let Some(d) = &depth {
            cmd.args(["--depth", d]);
        }
        cmd.args(["origin", "HEAD"]);
        run(cmd, &format!("git fetch {}", g.name), GIT_TIMEOUT_SECS)?;
        "FETCH_HEAD^{commit}"
    } else {
        let mut cmd = git_command(None);
        cmd.args(["clone", "--quiet", "--no-tags"]);
        if let Some(d) = &depth {
            cmd.args(["--depth", d]);
        }
        cmd.arg(g.url).arg(&dir);
        if let Err(e) = run(cmd, &format!("git clone {}", g.url), GIT_TIMEOUT_SECS) {
            let _ = fs::remove_dir_all(&dir);
            return Err(e);
        }
        "HEAD^{commit}"
    };
    let mut cmd = git_command(Some(&dir));
    cmd.args(["rev-parse", "--verify", reference]);
    let out = run(cmd, &format!("git rev-parse in {}", g.name), 60)?;
    Ok((dir, String::from_utf8_lossy(&out).trim().to_owned()))
}

fn git_snapshot(spec: &SourceSpec, g: &GitSpec, ctx: &Ctx<'_>) -> Result<Fetched, Failure> {
    let (dir, rev) = checkout(g, &ctx.cache.repos_dir()?)?;
    let out = ctx
        .cache
        .tmp_dir()?
        .join(format!("{}.snapshot.json", g.name));
    let mut cmd = Command::new(&ctx.node);
    match g.tool {
        GitTool::SnapshotGit => {
            cmd.arg(ctx.tools_dir.join("snapshot-git.mjs"))
                .arg(spec.id());
        }
        GitTool::Freellmapi => {
            cmd.arg("--no-warnings")
                .arg(ctx.tools_dir.join("snapshot-freellmapi.mjs"));
        }
    }
    cmd.arg("--repo")
        .arg(&dir)
        .args(["--rev", &rev])
        .arg("--out")
        .arg(&out);
    run(
        cmd,
        &format!("snapshot of {}", spec.id()),
        TOOL_TIMEOUT_SECS,
    )?;
    let bytes =
        fs::read(&out).map_err(|e| failure(format!("cannot read {}: {e}", out.display())))?;
    let _ = fs::remove_file(&out);
    finish(ctx.cache, bytes, Some(rev), ctx.now_ms)
}

fn omp_live_snapshot(provider: &str, spec: &SourceSpec, ctx: &Ctx<'_>) -> Result<Fetched, Failure> {
    let out = ctx
        .cache
        .tmp_dir()?
        .join(format!("omp-live-{provider}.json"));
    omp::snapshot_omp_live(&SnapshotOmpLiveArgs {
        provider: provider.to_owned(),
        omp: ctx.omp.clone(),
        agent_dir: Some(ctx.agent_dir.clone()),
        out: Some(out.clone()),
        timeout: ctx.omp_timeout_secs,
    })
    .map_err(|f| failure(format!("{}: {}", spec.id(), f.message)))?;
    let bytes =
        fs::read(&out).map_err(|e| failure(format!("cannot read {}: {e}", out.display())))?;
    let _ = fs::remove_file(&out);
    finish(ctx.cache, bytes, None, ctx.now_ms)
}

/// Takes a fresh snapshot of `spec` from where it lives.
pub fn fetch_live(spec: &SourceSpec, ctx: &Ctx<'_>) -> Result<Fetched, Failure> {
    match spec.origin {
        Origin::Http(url) => {
            let snapshot = crate::fetch(url)?;
            let meta = snapshot.fetch;
            let revision = meta
                .as_ref()
                .and_then(|m| m.etag.as_ref().map(|e| format!("etag:{e}")));
            let mut fetched = finish(ctx.cache, snapshot.bytes, revision, snapshot.fetched_at_ms)?;
            if let Some(m) = meta {
                fetched.latest.url = Some(m.url);
                fetched.latest.etag = m.etag;
                fetched.latest.last_modified = m.last_modified;
            }
            Ok(fetched)
        }
        Origin::OmpCache => {
            let path = ctx.agent_dir.join("cliproxyapi-models.json");
            let bytes = fs::read(&path)
                .map_err(|e| failure(format!("cannot read {}: {e}", path.display())))?;
            finish(ctx.cache, bytes, None, ctx.now_ms)
        }
        Origin::OmpLive(provider) => omp_live_snapshot(provider, spec, ctx),
        Origin::Git(g) => git_snapshot(spec, &g, ctx),
    }
}

/// A snapshot the caller saved (`--input SOURCE=FILE`): nothing is downloaded.
pub fn from_input(spec: &SourceSpec, path: &Path, ctx: &Ctx<'_>) -> Result<Fetched, Failure> {
    let bytes = fs::read(path).map_err(|e| {
        failure(format!(
            "{}: cannot read {}: {e}",
            spec.id(),
            path.display()
        ))
    })?;
    let modified = fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::now());
    // A git envelope names its commit; any other file is identified by its hash.
    let revision = matches!(spec.origin, Origin::Git(_))
        .then(|| {
            serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .and_then(|v| v.get("commit")?.as_str().map(str::to_owned))
        })
        .flatten();
    finish(ctx.cache, bytes, revision, ms_since_epoch(modified))
}

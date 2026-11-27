//! `tierbridge sync run`: design §3 a–e. Everything that can fail (fetch, structural validation,
//! the shrink guard, merge, export) happens BEFORE the data repo is touched; only a run that
//! produced a change then creates the branch, commits, pushes and opens the PR.

use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use clap::Args;
use tierbridge_catalog::omp_export::{CurrentRoster, ExportOptions, OmpExport, export_omp};
use tierbridge_catalog::overrides::Overrides;
use tierbridge_catalog::time::{format_rfc3339, parse_rfc3339_ms};
use tierbridge_catalog::{
    BuildOptions, BuildOutput, MERGED_CATALOG_FILE, SourceInput, merge_catalog_with, write_merge,
};

use super::cache::{Cache, Latest};
use super::diff::{Class, classify};
use super::fetch::{self, Ctx};
use super::git::Repo;
use super::index::{SourceIndex, index_path};
use super::init::OVERRIDES_FILE;
use super::report::{OverridesLine, ProviderLine, RunReport, SourceLine};
use super::sources::{Origin, SourceSpec, registry};
use super::state::{STATE_FILE, SourceState, SyncState, due};
use crate::{Failure, failure, ms_since_epoch};

/// Where the generated omp overrides live in the data repo.
pub const OMP_OVERRIDES_PATH: &str = "overrides/omp.yml";
const GH_TIMEOUT_SECS: u64 = 120;

#[derive(Args)]
pub struct RunArgs {
    /// The private data repo (`sync init`).
    #[arg(long, value_name = "DIR")]
    data_repo: PathBuf,
    /// Refresh only this source when it is due (repeatable); the others are reused from the cache.
    /// Default: every source.
    #[arg(long, value_name = "SOURCE")]
    source: Vec<String>,
    /// Refresh the selected sources whether they are due or not.
    #[arg(long)]
    force: bool,
    /// Commit the branch locally; do not push it (so no PR either).
    #[arg(long)]
    no_push: bool,
    /// Push the branch but do not open the PR.
    #[arg(long)]
    no_pr: bool,
    /// Use this saved snapshot instead of fetching the source (repeatable).
    #[arg(long, value_name = "SOURCE=FILE")]
    input: Vec<String>,
    /// Fail loud when a source holds more than this percentage fewer models than the accepted state
    /// (a source with no models always fails).
    #[arg(
        long,
        value_name = "PERCENT",
        default_value_t = 20,
        value_parser = clap::value_parser!(u8).range(0..=100)
    )]
    max_shrink_percent: u8,
    /// Also export `reasoning` and `thinking` to `overrides/omp.yml`.
    #[arg(long)]
    with_thinking: bool,
    /// Do not raise a macOS notification when the run fails (also: `$TIERBRIDGE_NO_NOTIFY`).
    #[arg(long)]
    no_notify: bool,
    /// Raw downloads, clones and the last merged catalog [default: `$TIERBRIDGE_CACHE_DIR`, else
    /// `~/.cache/llm-tierbridge`].
    #[arg(long, value_name = "DIR")]
    cache_dir: Option<PathBuf>,
    /// The `tools/` directory with the node snapshot scripts [default: `$TIERBRIDGE_TOOLS_DIR`, else
    /// the one of the checkout this binary was built from].
    #[arg(long, value_name = "DIR")]
    tools_dir: Option<PathBuf>,
    /// omp agent directory read for `cliproxyapi-models.json` and `models.yml` [default:
    /// `$PI_CODING_AGENT_DIR`, else `~/.omp/agent`]. Never written.
    #[arg(long, value_name = "DIR")]
    agent_dir: Option<PathBuf>,
    /// The omp binary for `snapshot omp-live` (see `tierbridge snapshot omp-live --help`).
    #[arg(long, value_name = "PATH")]
    omp: Option<PathBuf>,
    /// Give up on omp after this many seconds.
    #[arg(long, value_name = "SECS", default_value_t = 120)]
    omp_timeout: u64,
    /// Pretend it is this moment (RFC 3339): cadence, branch name and report date follow it.
    #[arg(long, value_name = "RFC3339", hide = true)]
    now: Option<String>,
}

/// One source taken for this run, with where its bytes came from.
struct Acquired {
    spec: SourceSpec,
    bytes: Vec<u8>,
    latest: Latest,
    action: &'static str,
    reason: String,
}

pub fn run(args: &RunArgs) -> Result<(), Failure> {
    let now_ms = match &args.now {
        Some(v) => parse_rfc3339_ms(v)?,
        None => ms_since_epoch(SystemTime::now()),
    };
    let result = pipeline(args, now_ms);
    if let Err(f) = &result {
        eprintln!(
            "{} sync run FAILED: {}",
            format_rfc3339(now_ms),
            f.message.lines().next().unwrap_or("")
        );
        let silent = args.no_notify || std::env::var_os("TIERBRIDGE_NO_NOTIFY").is_some();
        if !silent {
            notify("llm-tierbridge sync failed", &f.message);
        }
    }
    result
}

/// The AppleScript that shows a notification (quotes and newlines made harmless).
fn applescript(title: &str, message: &str) -> String {
    let esc = |s: &str| {
        s.chars()
            .take(200)
            .collect::<String>()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace(['\n', '\r'], " ")
    };
    format!(
        "display notification \"{}\" with title \"{}\"",
        esc(message),
        esc(title)
    )
}

fn notify(title: &str, message: &str) {
    if cfg!(target_os = "macos") {
        let _ = Command::new("osascript")
            .arg("-e")
            .arg(applescript(title, message))
            .output();
    }
}

fn open_repo(path: &Path) -> Result<(Repo, String), Failure> {
    let repo = Repo::new(path);
    if !path.join(STATE_FILE).is_file() || !repo.is_toplevel() {
        return Err(failure(format!(
            "{}: not a data repo (run `tierbridge sync init --data-repo {}` first)",
            path.display(),
            path.display()
        )));
    }
    if !repo.is_clean()? {
        return Err(failure(format!(
            "{}: uncommitted changes; commit or stash them before a sync",
            path.display()
        )));
    }
    let base = repo.current_branch()?;
    Ok((repo, base))
}

fn spec_named<'a>(
    registry: &'a [SourceSpec],
    id: &str,
    flag: &str,
) -> Result<&'a SourceSpec, Failure> {
    registry.iter().find(|s| s.id() == id).ok_or_else(|| {
        let ids: Vec<&str> = registry.iter().map(SourceSpec::id).collect();
        failure(format!(
            "{flag}: `{id}` is not a synced source (one of: {})",
            ids.join(", ")
        ))
    })
}

fn parse_inputs(
    specs: &[String],
    registry: &[SourceSpec],
) -> Result<BTreeMap<String, PathBuf>, Failure> {
    let mut inputs = BTreeMap::new();
    for spec in specs {
        let (id, path) = spec
            .split_once('=')
            .ok_or_else(|| failure(format!("--input `{spec}`: expected SOURCE=FILE")))?;
        let source = spec_named(registry, id, "--input")?;
        if inputs
            .insert(source.id().to_owned(), PathBuf::from(path))
            .is_some()
        {
            return Err(failure(format!("--input {id} given more than once")));
        }
    }
    Ok(inputs)
}

/// Lines that are not comments: what decides whether `overrides/omp.yml` really changed (its
/// comments cite the catalog hash, which moves with every fetch).
fn significant(yaml: &str) -> Vec<&str> {
    yaml.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect()
}

/// `sync/<date>`, or `sync/<date>-2`, ... when that branch exists already; with its report stem.
fn free_branch(repo: &Repo, date: &str) -> (String, String) {
    let mut stem = date.to_owned();
    let mut n = 1;
    while repo.branch_exists(&format!("sync/{stem}"))
        || repo.path.join(format!("reports/{stem}.md")).exists()
    {
        n += 1;
        stem = format!("{date}-{n}");
    }
    (format!("sync/{stem}"), stem)
}

/// An unmerged `sync/*` branch that already proposes exactly these files.
fn already_proposed(repo: &Repo, base: &str, files: &BTreeMap<String, Vec<u8>>) -> Option<String> {
    let listed = repo
        .run(&[
            "branch",
            "--list",
            "sync/*",
            "--no-merged",
            base,
            "--format=%(refname:short)",
        ])
        .ok()?;
    listed
        .lines()
        .find(|branch| {
            files.iter().all(|(path, bytes)| {
                repo.show(branch, path).is_some_and(|theirs| {
                    if path.ends_with(".yml") {
                        significant(&String::from_utf8_lossy(&theirs))
                            == significant(&String::from_utf8_lossy(bytes))
                    } else {
                        theirs == *bytes
                    }
                })
            })
        })
        .map(str::to_owned)
}

#[allow(clippy::too_many_lines)]
fn pipeline(args: &RunArgs, now_ms: i64) -> Result<(), Failure> {
    let date = format_rfc3339(now_ms)[..10].to_owned();
    let (repo, base) = open_repo(&args.data_repo)?;
    if !args.no_push && !repo.has_remote("origin") {
        return Err(failure(format!(
            "{}: no `origin` remote to push to (add one, or pass --no-push)",
            repo.path.display()
        )));
    }
    let gh = std::env::var_os("TIERBRIDGE_GH").map_or_else(|| PathBuf::from("gh"), PathBuf::from);
    if !args.no_push && !args.no_pr {
        let ok = Command::new(&gh)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !ok {
            return Err(failure(format!(
                "`{}` not found: install the GitHub CLI or pass --no-pr",
                gh.display()
            )));
        }
    }
    let state = SyncState::parse(
        &fs::read(repo.path.join(STATE_FILE)).map_err(|e| failure(format!("{STATE_FILE}: {e}")))?,
    )?;
    let overrides_bytes = match fs::read(repo.path.join(OVERRIDES_FILE)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => b"{}".to_vec(),
        Err(e) => return Err(failure(format!("{OVERRIDES_FILE}: {e}"))),
    };
    let overrides = Overrides::parse(&overrides_bytes, OVERRIDES_FILE)?;

    let cache = Cache::resolve(args.cache_dir.as_deref())?;
    let ctx = Ctx {
        cache: &cache,
        tools_dir: fetch::tools_dir(args.tools_dir.as_deref()),
        node: fetch::node_binary(),
        agent_dir: fetch::agent_dir(args.agent_dir.as_deref())?,
        omp: args.omp.clone(),
        omp_timeout_secs: args.omp_timeout,
        now_ms,
    };
    let registry = registry();
    let inputs = parse_inputs(&args.input, &registry)?;
    for id in &args.source {
        spec_named(&registry, id, "--source")?;
    }

    // a. refresh what is due, reuse what is not.
    let mut latest = cache.latest()?;
    let mut acquired: Vec<Acquired> = Vec::new();
    let mut skipped: Vec<SourceLine> = Vec::new();
    for spec in &registry {
        let id = spec.id();
        if let Some(path) = inputs.get(id) {
            let f = fetch::from_input(spec, path, &ctx)?;
            acquired.push(Acquired {
                spec: *spec,
                bytes: f.bytes,
                latest: f.latest,
                action: "input",
                reason: format!("--input {}", path.display()),
            });
            continue;
        }
        let selected = args.source.is_empty() || args.source.iter().any(|s| s == id);
        let cached = latest
            .get(id)
            .and_then(|l| cache.load_raw(&l.sha256).map(|b| (l.clone(), b)));
        let refresh: Option<String> = match (&cached, selected) {
            (_, true) if args.force => Some("forced".to_owned()),
            (None, false) if !state.sources.contains_key(id) => {
                skipped.push(SourceLine {
                    source: id.to_owned(),
                    action: "skipped",
                    reason: "not selected and never synced".to_owned(),
                    revision: None,
                    count: None,
                    before: None,
                    rejections: None,
                });
                continue;
            }
            (None, _) => Some(
                if state.sources.contains_key(id) {
                    "no cached snapshot"
                } else {
                    "never fetched"
                }
                .to_owned(),
            ),
            (Some((l, _)), true) => {
                let moved = match spec.origin {
                    Origin::Git(g) => fetch::upstream_revision(&g)? != l.revision,
                    _ => false,
                };
                due(spec.cadence, Some(l.fetched_at_ms), now_ms, moved)
                    .map(|r| r.label().to_owned())
            }
            (Some(_), false) => None,
        };
        match (refresh, cached) {
            (Some(reason), _) => {
                let f = fetch::fetch_live(spec, &ctx)?;
                acquired.push(Acquired {
                    spec: *spec,
                    bytes: f.bytes,
                    latest: f.latest,
                    action: "refreshed",
                    reason,
                });
            }
            (None, Some((l, bytes))) => {
                let reason = format!(
                    "{}, fetched {}",
                    spec.cadence.label(),
                    format_rfc3339(l.fetched_at_ms)
                );
                acquired.push(Acquired {
                    spec: *spec,
                    bytes,
                    latest: l,
                    action: "reused",
                    reason,
                });
            }
            (None, None) => unreachable!("reuse needs a cached snapshot"),
        }
    }

    // b. validate: structure (the adapters) and the shrink guard.
    let mut outs: Vec<BuildOutput> = Vec::new();
    for a in &acquired {
        let meta = a.latest.fetch_meta();
        let out = a
            .spec
            .source
            .build(
                SourceInput {
                    bytes: &a.bytes,
                    fetched_at_ms: a.latest.fetched_at_ms,
                    fetch: meta.as_ref(),
                },
                &BuildOptions::default(),
            )
            .map_err(|e| failure(format!("{}: {e}", a.spec.id())))?;
        outs.push(out);
    }
    let mut problems = Vec::new();
    for (a, out) in acquired.iter().zip(&outs) {
        let id = a.spec.id();
        let now_count = out.observations.len();
        if now_count == 0 {
            problems.push(format!("{id}: no models"));
        } else if let Some(prev) = state.sources.get(id).map(|s| s.count).filter(|c| *c > 0)
            && now_count * 100 < prev * (100 - usize::from(args.max_shrink_percent))
        {
            #[allow(clippy::cast_precision_loss)]
            let pct = 100.0 - now_count as f64 * 100.0 / prev as f64;
            problems.push(format!(
                "{id}: {prev} -> {now_count} models (-{pct:.1}%, limit {}%)",
                args.max_shrink_percent
            ));
        }
    }
    if !problems.is_empty() {
        return Err(failure(format!(
            "shrink guard: {}; the data repo was not touched",
            problems.join("; ")
        )));
    }

    // d. merge every source with overrides.json, regenerate the omp overrides.
    let refs: Vec<&BuildOutput> = outs.iter().collect();
    let merged = merge_catalog_with(&refs, (!overrides.is_empty()).then_some(&overrides))?;
    let last = cache.last_dir()?;
    write_merge(&last, &merged)?;
    let catalog_path = last.join(MERGED_CATALOG_FILE);
    let catalog_bytes = fs::read(&catalog_path)
        .map_err(|e| failure(format!("cannot read {}: {e}", catalog_path.display())))?;
    let providers: Vec<String> = acquired
        .iter()
        .filter_map(|a| match a.spec.origin {
            Origin::OmpLive(p) => Some(p.to_owned()),
            _ => None,
        })
        .collect();
    let export: Option<OmpExport> = if providers.is_empty() {
        None
    } else {
        let mut current = Vec::new();
        for a in acquired
            .iter()
            .filter(|a| matches!(a.spec.origin, Origin::OmpLive(_)))
        {
            current.push(
                CurrentRoster::parse(&a.bytes)
                    .map_err(|e| failure(format!("{}: roster not readable: {e}", a.spec.id())))?,
            );
        }
        Some(export_omp(
            &catalog_bytes,
            &ExportOptions {
                providers,
                current,
                with_thinking: args.with_thinking,
            },
        )?)
    };

    // c. classify against the accepted index.
    let mut proposed: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut diffs = Vec::new();
    let mut index_changed = false;
    for (a, out) in acquired.iter().zip(&outs) {
        let path = index_path(a.spec.id());
        let new = SourceIndex::from_observations(&out.observations);
        let old_bytes = fs::read(repo.path.join(&path)).ok();
        let old = match &old_bytes {
            Some(b) => SourceIndex::parse(b, &path)?,
            None => SourceIndex::default(),
        };
        let text = new.to_jsonl().into_bytes();
        index_changed |= old_bytes.as_deref() != Some(text.as_slice());
        diffs.push(classify(a.spec.id(), &old, &new));
        proposed.insert(path, text);
    }
    let overrides_changed = export.as_ref().is_some_and(|e| {
        let old = fs::read_to_string(repo.path.join(OMP_OVERRIDES_PATH)).ok();
        old.as_deref().map(significant) != Some(significant(&e.yaml))
    });
    if let Some(e) = &export {
        proposed.insert(OMP_OVERRIDES_PATH.to_owned(), e.yaml.clone().into_bytes());
    }

    // the report.
    let mut lines: Vec<SourceLine> = acquired
        .iter()
        .zip(&outs)
        .map(|(a, out)| SourceLine {
            source: a.spec.id().to_owned(),
            action: a.action,
            reason: a.reason.clone(),
            revision: Some(a.latest.revision.clone()),
            count: Some(out.observations.len()),
            before: state.sources.get(a.spec.id()).map(|s| s.count),
            rejections: Some(out.report.rejections.len()),
        })
        .collect();
    lines.extend(skipped);
    let report_base = RunReport {
        date: date.clone(),
        generated_at: format_rfc3339(now_ms),
        base: base.clone(),
        merged_models: merged.models.len(),
        catalog_sha256: tierbridge_catalog::models_dev::sha256_hex(&catalog_bytes),
        sources: lines,
        diffs,
        overrides: export.as_ref().map(|e| OverridesLine {
            providers: e
                .stats
                .iter()
                .map(|s| ProviderLine {
                    provider: s.provider.clone(),
                    rows: s.rows,
                    with_overrides: s.with_overrides,
                })
                .collect(),
            changed: overrides_changed,
        }),
    };

    // the newest snapshots are remembered whatever happens next (cadence); the state is not.
    for a in &acquired {
        latest.insert(a.spec.id().to_owned(), a.latest.clone());
    }
    cache.save_latest(&latest)?;
    let mut keep: HashSet<String> = latest.values().map(|l| l.sha256.clone()).collect();
    keep.extend(state.sources.values().map(|s| s.sha256.clone()));
    cache.prune_raw(&keep);

    print_summary(&report_base);
    if !(index_changed || overrides_changed) {
        println!("nothing to sync");
        return Ok(());
    }
    if let Some(branch) = already_proposed(&repo, &base, &proposed) {
        println!("nothing new to sync: already proposed in branch {branch} (not merged yet)");
        return Ok(());
    }

    // e. the state advances only inside the PR.
    let mut new_state = state.clone();
    for (a, out) in acquired.iter().zip(&outs) {
        let id = a.spec.id();
        if state
            .sources
            .get(id)
            .is_some_and(|s| s.sha256 == a.latest.sha256)
        {
            continue;
        }
        new_state.sources.insert(
            id.to_owned(),
            SourceState {
                revision: a.latest.revision.clone(),
                sha256: a.latest.sha256.clone(),
                fetched_at: format_rfc3339(a.latest.fetched_at_ms),
                count: out.observations.len(),
                cadence: a.spec.cadence,
                last_synced_at: format_rfc3339(now_ms),
            },
        );
    }
    let (branch, stem) = free_branch(&repo, &date);
    let mut files = proposed;
    files.insert(STATE_FILE.to_owned(), new_state.to_bytes());
    files.insert(
        format!("reports/{stem}.md"),
        report_base.to_markdown().into_bytes(),
    );
    files.insert(format!("reports/{stem}.json"), report_base.to_json());
    let body_path = cache.tmp_dir()?.join("pr-body.md");
    fs::write(&body_path, report_base.pr_body())
        .map_err(|e| failure(format!("cannot write {}: {e}", body_path.display())))?;

    let title = report_base.title();
    let pushed = !args.no_push;
    let pr_requested = pushed && !args.no_pr;
    let pr_url = publish(
        &repo,
        &base,
        &branch,
        &files,
        &title,
        &report_base,
        pushed,
        pr_requested,
        &gh,
        &body_path,
    )?;
    println!("branch {branch} committed in {}", repo.path.display());
    if pushed {
        println!("pushed {branch} to origin");
    }
    if let Some(url) = pr_url {
        println!("PR {url}");
    }
    Ok(())
}

/// Branch, files, commit, push, PR; always back on `base` afterwards.
#[allow(clippy::too_many_arguments)]
fn publish(
    repo: &Repo,
    base: &str,
    branch: &str,
    files: &BTreeMap<String, Vec<u8>>,
    title: &str,
    report: &RunReport,
    push: bool,
    pr: bool,
    gh: &Path,
    body_path: &Path,
) -> Result<Option<String>, Failure> {
    repo.run(&["checkout", "--quiet", "-b", branch])?;
    let committed = Cell::new(false);
    let outcome = (|| -> Result<Option<String>, Failure> {
        for (path, bytes) in files {
            let target = repo.path.join(path);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| failure(format!("cannot create {}: {e}", parent.display())))?;
            }
            fs::write(&target, bytes)
                .map_err(|e| failure(format!("cannot write {}: {e}", target.display())))?;
        }
        let mut add: Vec<&str> = vec!["add", "--"];
        add.extend(files.keys().map(String::as_str));
        repo.run(&add)?;
        repo.commit(&format!("{title}\n\n{}", commit_body(report)))?;
        committed.set(true);
        let late = |e: Failure| {
            failure(format!(
                "branch {branch} is committed locally in {}; {}",
                repo.path.display(),
                e.message
            ))
        };
        if push {
            repo.run(&["push", "--quiet", "-u", "origin", branch])
                .map_err(late)?;
        }
        if !pr {
            return Ok(None);
        }
        let mut cmd = Command::new(gh);
        cmd.current_dir(&repo.path)
            .args([
                "pr", "create", "--base", base, "--head", branch, "--title", title,
            ])
            .arg("--body-file")
            .arg(body_path);
        let (status, out, err) =
            crate::omp::run_with_timeout(cmd, Duration::from_secs(GH_TIMEOUT_SECS))
                .map_err(|e| late(failure(e)))?;
        if !status.success() {
            return Err(late(failure(format!(
                "gh pr create failed ({status}): {}",
                super::git::first_lines(&err, 3)
            ))));
        }
        Ok(String::from_utf8_lossy(&out)
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .map(|l| l.trim().to_owned()))
    })();
    if !committed.get() {
        let _ = repo.run(&["reset", "--hard", "--quiet"]);
        let _ = repo.run(&["clean", "-fdq"]);
    }
    let back = repo.run(&["checkout", "--quiet", base]);
    if !committed.get() {
        let _ = repo.run(&["branch", "-D", branch]);
    }
    match (outcome, back) {
        (Ok(url), Ok(_)) => Ok(url),
        (Err(e), _) | (Ok(_), Err(e)) => Err(e),
    }
}

fn commit_body(report: &RunReport) -> String {
    let mut out = String::new();
    for d in report.diffs.iter().filter(|d| !d.is_empty()) {
        let parts: Vec<String> = Class::ALL
            .iter()
            .filter(|c| d.count(**c) > 0)
            .map(|c| format!("{} {}", d.count(*c), c.label()))
            .collect();
        out.push_str(&format!("{}: {}\n", d.source, parts.join(", ")));
    }
    out
}

fn print_summary(report: &RunReport) {
    println!("sync {} (base {})", report.date, report.base);
    println!(
        "  {:<24} {:<10} {:>7} {:>7} {:>4}  revision",
        "source", "action", "rows", "before", "rej"
    );
    for s in &report.sources {
        let n = |v: Option<usize>| v.map_or("-".to_owned(), |v| v.to_string());
        println!(
            "  {:<24} {:<10} {:>7} {:>7} {:>4}  {}",
            s.source,
            s.action,
            n(s.count),
            n(s.before),
            n(s.rejections),
            s.revision.as_deref().unwrap_or("-")
        );
    }
    println!("  merged catalog: {} models", report.merged_models);
    for d in &report.diffs {
        let parts: Vec<String> = Class::ALL
            .iter()
            .filter(|c| d.count(**c) > 0)
            .map(|c| format!("{} {}", d.count(*c), c.label()))
            .collect();
        let oos = d.out_of_scope.total();
        let mut line = if parts.is_empty() {
            "no change".to_owned()
        } else {
            parts.join(", ")
        };
        if oos > 0 {
            line.push_str(&format!(", {oos} out of scope"));
        }
        println!("  diff {:<22} {line}", d.source);
    }
    if let Some(o) = &report.overrides {
        println!(
            "  overrides/omp.yml {}",
            if o.changed { "changed" } else { "unchanged" }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_script_escapes_quotes_and_newlines() {
        let s = applescript("t\"itle", "line one\nsaid \"hi\" \\ end");
        assert_eq!(
            s,
            "display notification \"line one said \\\"hi\\\" \\\\ end\" with title \"t\\\"itle\""
        );
    }

    #[test]
    fn comments_do_not_count_as_a_change() {
        let a = "# catalog sha256:aaa\nproviders:\n  dsh:\n    # sha256:aaa\n    x: 1\n";
        let b = "# catalog sha256:bbb\nproviders:\n  dsh:\n    # sha256:bbb\n    x: 1\n";
        let c = "# catalog sha256:bbb\nproviders:\n  dsh:\n    x: 2\n";
        assert_eq!(significant(a), significant(b));
        assert_ne!(significant(a), significant(c));
    }

    #[test]
    fn inputs_need_a_known_source_and_a_file() {
        let reg = registry();
        assert!(parse_inputs(&["models.dev".into()], &reg).is_err());
        assert!(parse_inputs(&["nope=x".into()], &reg).is_err());
        assert!(parse_inputs(&["dsh=a".into(), "dsh=b".into()], &reg).is_err());
        let ok = parse_inputs(&["omp-live:dsh=/tmp/x.json".into()], &reg).unwrap();
        assert_eq!(ok["omp-live:dsh"], PathBuf::from("/tmp/x.json"));
    }
}

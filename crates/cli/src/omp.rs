//! `tierbridge export omp`, `apply omp` and `snapshot omp-live`: the omp side of the integration.

use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Args;
use tierbridge_catalog::OmpError;
use tierbridge_catalog::omp_export::{
    CurrentRoster, ExportOptions, FieldCount, OmpExport, ProviderStats, export_omp,
};
use tierbridge_catalog::omp_live::OMP_VERSION;
use tierbridge_catalog::omp_models_yml::{self, EditPlan};

use crate::{Failure, failure};

impl From<OmpError> for Failure {
    fn from(e: OmpError) -> Self {
        failure(e.to_string())
    }
}

#[derive(Args)]
pub struct ExportOmpArgs {
    /// Merged `catalog.json` (`catalog build --merge`).
    #[arg(long, value_name = "FILE")]
    catalog: PathBuf,
    /// omp provider to export (rows of `omp-live:<provider>`); repeatable.
    #[arg(long, value_name = "NAME", required = true)]
    provider: Vec<String>,
    /// What omp shows now: the output of `omp models <provider> --json`, one file per provider
    /// (the provider is read from its rows); repeatable. Fields that already match are not exported.
    #[arg(long, value_name = "FILE")]
    current: Vec<PathBuf>,
    /// Also export `reasoning` and `thinking` (omp's `ThinkingConfig`).
    #[arg(long)]
    with_thinking: bool,
    /// File to write, in the shape of `providers.<p>.modelOverrides` of `models.yml`.
    #[arg(long, value_name = "FILE")]
    out: PathBuf,
}

fn read(path: &Path) -> Result<Vec<u8>, Failure> {
    fs::read(path).map_err(|e| OmpError::io(path, e).into())
}

pub fn export(args: &ExportOmpArgs) -> Result<(), Failure> {
    let catalog = read(&args.catalog)?;
    let mut current = Vec::new();
    for path in &args.current {
        let roster = CurrentRoster::parse(&read(path)?).map_err(|reason| OmpError::Current {
            path: path.clone(),
            reason,
        })?;
        if current
            .iter()
            .any(|c: &CurrentRoster| c.provider == roster.provider)
        {
            return Err(OmpError::CurrentDuplicate {
                provider: roster.provider,
            }
            .into());
        }
        current.push(roster);
    }
    let opts = ExportOptions {
        providers: args.provider.clone(),
        current,
        with_thinking: args.with_thinking,
    };
    let export = export_omp(&catalog, &opts)?;
    fs::write(&args.out, &export.yaml).map_err(|e| OmpError::io(&args.out, e))?;
    print!("{}", export_summary(&export, &args.out, args.with_thinking));
    Ok(())
}

fn field_row(out: &mut String, name: &str, c: FieldCount) {
    let _ = writeln!(
        out,
        "  {name:<14}{:>8}{:>10}{:>9}{:>7}",
        c.emitted, c.unchanged, c.gateway, c.none
    );
}

fn provider_summary(out: &mut String, s: &ProviderStats, with_thinking: bool) {
    let _ = write!(out, "{}  rows {}", s.provider, s.rows);
    if let Some(roster) = s.roster {
        let _ = write!(
            out,
            "  roster {roster} (rows not in roster {}, roster models not in catalog {})",
            s.not_in_roster, s.roster_not_in_catalog
        );
    }
    let _ = writeln!(
        out,
        "  models with overrides {} of {}",
        s.with_overrides, s.considered
    );
    if s.duplicates > 0 {
        let _ = writeln!(out, "  duplicate ids ignored: {}", s.duplicates);
    }
    let _ = writeln!(
        out,
        "  {:<14}{:>8}{:>10}{:>9}{:>7}",
        "field", "written", "unchanged", "gateway", "none"
    );
    field_row(out, "contextWindow", s.context_window);
    field_row(out, "maxTokens", s.max_tokens);
    field_row(out, "cost", s.cost);
    field_row(out, "input", s.input);
    if with_thinking {
        field_row(out, "reasoning", s.reasoning);
        field_row(out, "thinking", s.thinking);
    }
    let _ = writeln!(
        out,
        "  cost: {} free, {} with price tiers (tiers not exported), {} known without input/output (not exported)",
        s.cost_free, s.cost_tiers_dropped, s.cost_partial
    );
    if with_thinking {
        let _ = writeln!(
            out,
            "  thinking not expressible in omp (toggle, budget, none): {}",
            s.thinking_inexpressible
        );
    }
    let r = &s.remaining;
    let _ = writeln!(
        out,
        "  remaining with placeholders: {} models (contextWindow {}, maxTokens {})",
        r.placeholder_models, r.context_window, r.max_tokens
    );
    for (reason, n) in &r.reasons {
        let _ = writeln!(out, "    {n:>5}  {reason}");
    }
    let _ = writeln!(
        out,
        "  price still unknown (omp keeps zeros with status unknown): {}",
        r.cost_unknown
    );
}

fn export_summary(export: &OmpExport, out_path: &Path, with_thinking: bool) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "catalog sha256:{}", &export.catalog_sha256[..16]);
    for s in &export.stats {
        provider_summary(&mut out, s, with_thinking);
    }
    let total: usize = export.stats.iter().map(|s| s.with_overrides).sum();
    let _ = writeln!(
        out,
        "wrote {}  ({total} models with overrides, {} bytes)",
        out_path.display(),
        export.yaml.len()
    );
    out
}

#[derive(Args)]
pub struct ApplyOmpArgs {
    /// omp's `models.yml` (edited in place; a backup is written next to it first).
    #[arg(long, value_name = "FILE")]
    pub(crate) models_yml: PathBuf,
    /// File written by `export omp`: sets the managed blocks to exactly its providers.
    #[arg(
        long,
        value_name = "FILE",
        required_unless_present = "revert",
        conflicts_with = "revert"
    )]
    pub(crate) overrides: Option<PathBuf>,
    /// Remove every managed block instead (the file goes back to what it was before `apply`).
    #[arg(long)]
    pub(crate) revert: bool,
    /// Show what would change; write nothing (no backup either).
    #[arg(long)]
    pub(crate) dry_run: bool,
}

fn now_ms() -> i64 {
    crate::ms_since_epoch(std::time::SystemTime::now())
}

pub fn apply(args: &ApplyOmpArgs) -> Result<(), Failure> {
    let original = read(&args.models_yml)?;
    let original_text = String::from_utf8(original.clone())
        .map_err(|_| failure(format!("{}: not UTF-8", args.models_yml.display())))?;
    let plan: EditPlan = if args.revert {
        omp_models_yml::revert(&original_text)?
    } else {
        let path = args
            .overrides
            .as_deref()
            .expect("clap: --overrides unless --revert");
        let text = String::from_utf8(read(path)?)
            .map_err(|_| failure(format!("{}: not UTF-8", path.display())))?;
        omp_models_yml::apply(&original_text, &omp_models_yml::parse_overrides(&text)?)?
    };
    print!("{}", plan_summary(&plan, args));
    if !plan.changed {
        println!("models.yml already up to date; nothing written");
        return Ok(());
    }
    if args.dry_run {
        println!("dry run: nothing written");
        return Ok(());
    }
    let backup = omp_models_yml::commit(&args.models_yml, &original, &plan.text, now_ms())?;
    println!("backup {}", backup.display());
    println!("wrote {}", args.models_yml.display());
    Ok(())
}

fn plan_summary(plan: &EditPlan, args: &ApplyOmpArgs) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} {}",
        if args.revert { "revert" } else { "apply" },
        args.models_yml.display()
    );
    let _ = writeln!(out, "  managed blocks removed: {}", plan.removed_blocks);
    for i in &plan.inserted {
        let _ = writeln!(
            out,
            "  {}: block of {} lines, {} models",
            i.provider, i.lines, i.models
        );
    }
    for p in &plan.empty_providers {
        let _ = writeln!(out, "  {p}: no overrides, no block");
    }
    out
}

// ---- snapshot omp-live ----

#[derive(Args)]
pub struct SnapshotOmpLiveArgs {
    /// omp provider to list (`easycliproxy`, `dsh`).
    #[arg(long, value_name = "NAME")]
    pub(crate) provider: String,
    /// The omp binary. Default: `$TIERBRIDGE_OMP`, else the Tern install of omp 19.2.1. The `omp`
    /// on PATH is never used.
    #[arg(long, value_name = "PATH")]
    pub(crate) omp: Option<PathBuf>,
    /// Agent directory whose `models.yml` is read [default: `$PI_CODING_AGENT_DIR`, else
    /// `~/.omp/agent`]. Only `models.yml` is copied; the directory is never written.
    #[arg(long, value_name = "DIR")]
    pub(crate) agent_dir: Option<PathBuf>,
    /// Write the roster here instead of stdout (input of `catalog build --source omp-live:<p>`).
    #[arg(long, value_name = "FILE")]
    pub(crate) out: Option<PathBuf>,
    /// Give up on omp after this many seconds.
    #[arg(long, value_name = "SECS", default_value_t = 120)]
    pub(crate) timeout: u64,
}

/// Scratch agent directory (mode 0700), removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> std::io::Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let path =
            std::env::temp_dir().join(format!("tierbridge-omp-{}-{nanos}", std::process::id()));
        let mut b = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            b.mode(0o700);
        }
        b.create(&path)?;
        Ok(Self(path))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_dir_all(&self.0) {
            eprintln!("warning: could not remove {}: {e}", self.0.display());
        }
    }
}

/// Hides anything that looks like a credential in text that came from another program.
fn redact(text: &str) -> String {
    text.split_inclusive(char::is_whitespace)
        .map(|w| {
            let t = w.trim_end();
            let long = t.len() >= 20
                && t.bytes().any(|b| b.is_ascii_digit())
                && t.bytes().any(|b| b.is_ascii_alphabetic());
            if long || t.starts_with("sk-") || t.starts_with("Bearer") {
                format!("<redacted>{}", &w[t.len()..])
            } else {
                w.to_owned()
            }
        })
        .collect()
}

fn first_lines(text: &str, n: usize) -> String {
    redact(text)
        .lines()
        .take(n)
        .map(|l| l.chars().take(200).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n  ")
}

fn omp_binary(arg: Option<&Path>) -> Result<PathBuf, Failure> {
    let path = arg
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("TIERBRIDGE_OMP").map(PathBuf::from))
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join("Library/Application Support/Tern/omp/bin/omp"))
        })
        .ok_or_else(|| failure("no omp binary: pass --omp"))?;
    if path.is_file() {
        Ok(path)
    } else {
        Err(failure(format!(
            "omp binary {} not found: pass --omp (the `omp` on PATH is not used)",
            path.display()
        )))
    }
}

pub(crate) fn run_with_timeout(
    mut cmd: std::process::Command,
    timeout: Duration,
) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), String> {
    use std::process::Stdio;
    let program = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start {program}: {e}"))?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let started = std::time::Instant::now();
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => {
                let out = out.join().unwrap_or_default();
                let err = err.join().unwrap_or_default();
                return Ok((status, out, err));
            }
            None if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{program} did not finish within {}s; killed",
                    timeout.as_secs()
                ));
            }
            None => std::thread::sleep(Duration::from_millis(25)),
        }
    }
}

pub fn snapshot_omp_live(args: &SnapshotOmpLiveArgs) -> Result<(), Failure> {
    let provider = args.provider.as_str();
    let name_ok = !provider.is_empty()
        && provider
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !provider.starts_with('-')
        && !matches!(provider, "ls" | "find" | "refresh");
    if !name_ok {
        return Err(failure(format!("`{provider}` is not a provider name")));
    }
    let omp = omp_binary(args.omp.as_deref())?;
    let agent_dir = args
        .agent_dir
        .clone()
        .or_else(|| std::env::var_os("PI_CODING_AGENT_DIR").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".omp/agent")))
        .ok_or_else(|| failure("no agent directory: pass --agent-dir"))?;
    let models_yml = agent_dir.join("models.yml");
    let text = String::from_utf8(read(&models_yml)?)
        .map_err(|_| failure(format!("{}: not UTF-8", models_yml.display())))?;
    // The managed overrides must not feed back into the roster the next refresh reads.
    let (stripped, blocks) = tierbridge_catalog::omp_models_yml::strip_managed(&text)?;

    let version = std::process::Command::new(&omp)
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default();
    if !version.contains(OMP_VERSION) {
        eprintln!(
            "warning: omp reports `{}`, the adapter expects {OMP_VERSION}",
            first_lines(&version, 1)
        );
    }

    let tmp = TempDir::new().map_err(|e| failure(format!("temp dir: {e}")))?;
    let copy = tmp.0.join("models.yml");
    {
        use std::io::Write as _;
        let mut o = fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            o.mode(0o600);
        }
        o.open(&copy)
            .and_then(|mut f| f.write_all(stripped.as_bytes()))
            .map_err(|e| OmpError::io(&copy, e))?;
    }
    let mut cmd = std::process::Command::new(&omp);
    cmd.args(["models", provider, "--json", "--no-extensions"])
        .env("PI_CODING_AGENT_DIR", &tmp.0);
    let (status, stdout, stderr) =
        run_with_timeout(cmd, Duration::from_secs(args.timeout)).map_err(failure)?;
    let stderr = String::from_utf8_lossy(&stderr);
    if !status.success() {
        return Err(failure(format!(
            "omp models {provider} failed ({status}); stderr (secrets hidden):\n  {}",
            first_lines(&stderr, 5)
        )));
    }
    let roster: serde_json::Value = serde_json::from_slice(&stdout)
        .map_err(|e| failure(format!("omp printed no JSON roster: {e}")))?;
    let count = roster
        .get("models")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .filter(|n| *n > 0)
        .ok_or_else(|| failure("omp's output has no non-empty `models` array"))?;
    match &args.out {
        Some(path) => fs::write(path, &stdout).map_err(|e| OmpError::io(path, e))?,
        None => {
            use std::io::Write as _;
            std::io::stdout()
                .write_all(&stdout)
                .map_err(|e| failure(e.to_string()))?;
        }
    }
    let warnings = stderr.lines().filter(|l| l.contains("arning")).count();
    eprintln!(
        "snapshot omp-live:{provider}: {count} models; {blocks} managed block(s) stripped from the copy; omp warnings {warnings}; temp dir removed"
    );
    for l in stderr
        .lines()
        .filter(|l| l.contains("odelOverrides") || l.contains("models.yml"))
    {
        eprintln!("  omp: {}", first_lines(l, 1));
    }
    Ok(())
}

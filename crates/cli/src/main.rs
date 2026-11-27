//! `tierbridge` command line.

mod omp;
mod sync;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::{Args, Parser, Subcommand, ValueEnum};
use tierbridge_catalog::{
    BuildOptions, BuildOutput, CatalogError, FetchMeta, MergeOutput, SourceInput,
    export_cpa::{CpaOptions, PLACEHOLDER_KEY, SkipReason, export_cpa},
    merge_catalog_with,
    overrides::Overrides,
    time::parse_rfc3339_ms,
    write_merge, write_outputs,
};

/// Exit code for a structural failure, I/O error or failed fetch (clap itself uses 2 for usage errors).
const EXIT_FAILURE: u8 = 1;
/// Exit code when `--strict` found rejected values.
const EXIT_STRICT: u8 = 3;
/// Rejections listed on screen before pointing at the source's `report.<source>.json`.
const SUMMARY_LIST_LIMIT: usize = 10;
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
const FETCH_MAX_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Parser)]
#[command(name = "tierbridge", version, about = "llm-tierbridge tooling")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Normalized model catalog.
    Catalog {
        #[command(subcommand)]
        action: CatalogCommand,
    },
    /// Generate configuration for other tools from a merged `catalog.json`.
    Export {
        #[command(subcommand)]
        target: ExportCommand,
    },
    /// Edit another tool's configuration file from an export.
    Apply {
        #[command(subcommand)]
        target: ApplyCommand,
    },
    /// Take a snapshot of a live source (runs the tool, never touches its real config).
    Snapshot {
        #[command(subcommand)]
        target: SnapshotCommand,
    },
    /// Keep the private data repo up to date: fetch, validate, classify, open a reviewable PR.
    Sync {
        #[command(subcommand)]
        action: sync::SyncCommand,
    },
}

#[derive(Subcommand)]
enum ExportCommand {
    /// `openai-compatibility` block for CLIProxyAPI (config-version 8).
    Cpa(CpaArgs),
    /// omp `modelOverrides` for the gateway providers (`providers.<p>.modelOverrides`).
    Omp(omp::ExportOmpArgs),
}

#[derive(Subcommand)]
enum ApplyCommand {
    /// Insert or revert the managed `modelOverrides` blocks of omp's `models.yml`.
    Omp(omp::ApplyOmpArgs),
}

#[derive(Subcommand)]
enum SnapshotCommand {
    /// The roster `omp models <provider> --json` prints, from a copy of `models.yml` without
    /// the managed blocks (input of `catalog build --source omp-live:<provider>`).
    #[command(name = "omp-live")]
    OmpLive(omp::SnapshotOmpLiveArgs),
}

#[derive(Args)]
struct CpaArgs {
    /// Merged `catalog.json` (from `catalog build --merge`).
    #[arg(long, value_name = "FILE")]
    catalog: PathBuf,
    /// Gateway whose rows to export, as it prefixes catalog keys (`omp-live:easycliproxy`).
    #[arg(long, value_name = "SOURCE_ID")]
    gateway: String,
    /// OpenAI-compatible base URL the generated provider sends to (`http://127.0.0.1:8317/v1`).
    #[arg(long, value_name = "URL")]
    base_url: String,
    /// `name:` of the provider entry [default: tierbridge-<gateway>].
    #[arg(long, value_name = "NAME")]
    name: Option<String>,
    /// Prepended to each model id to form the alias clients use.
    #[arg(long, value_name = "TEXT", default_value = "tb-")]
    alias_prefix: String,
    /// CLIProxyAPI `prefix:` of the provider (clients then call `prefix/alias`).
    #[arg(long, value_name = "TEXT")]
    prefix: Option<String>,
    /// Environment variable holding the upstream API key; its value is written into the output
    /// file, so `--out` is required. Without it the key is a placeholder.
    #[arg(long, value_name = "VAR", requires = "out")]
    api_key_env: Option<String>,
    /// Write the fragment here (created with mode 0600) instead of stdout.
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
}

#[derive(Subcommand)]
enum CatalogCommand {
    /// Build, per source, observations, a single-source `model[]` and a report from a snapshot.
    Build(BuildArgs),
    /// Match gateway model ids against reference catalogs (tiers T1..T4) and print the counts.
    Match(MatchArgs),
}

#[derive(Args)]
struct MatchArgs {
    /// `observations.<source>.jsonl` of a gateway source; repeatable.
    #[arg(long, value_name = "FILE", required = true)]
    gateway: Vec<PathBuf>,
    /// `observations.<source>.jsonl` of a reference catalog (models.dev, modelparams.dev);
    /// repeatable. Counted in `either`/`both`/`neither` and the union.
    #[arg(long, value_name = "FILE", required = true)]
    reference: Vec<PathBuf>,
    /// A reference shown per gateway but not counted in the headline (omp-bundled: circular).
    #[arg(long, value_name = "FILE")]
    probe: Vec<PathBuf>,
    /// Print the full report as JSON instead of the table.
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Source {
    #[value(name = "models.dev")]
    ModelsDev,
    #[value(name = "modelparams.dev")]
    ModelparamsDev,
    #[value(name = "cliproxyapi-cache")]
    CliproxyapiCache,
    #[value(name = "omp-live:easycliproxy")]
    OmpLiveEasycliproxy,
    #[value(name = "omp-live:dsh")]
    OmpLiveDsh,
    #[value(name = "omp-bundled")]
    OmpBundled,
    #[value(name = "router-for-me.models")]
    RouterModels,
    #[value(name = "dsh")]
    Dsh,
    #[value(name = "freellmapi")]
    Freellmapi,
}

impl Source {
    fn catalog(self) -> tierbridge_catalog::Source {
        match self {
            Self::ModelsDev => tierbridge_catalog::Source::ModelsDev,
            Self::ModelparamsDev => tierbridge_catalog::Source::ModelparamsDev,
            Self::CliproxyapiCache => tierbridge_catalog::Source::CliproxyapiCache,
            Self::OmpLiveEasycliproxy => tierbridge_catalog::Source::OmpLiveEasycliproxy,
            Self::OmpLiveDsh => tierbridge_catalog::Source::OmpLiveDsh,
            Self::OmpBundled => tierbridge_catalog::Source::OmpBundled,
            Self::RouterModels => tierbridge_catalog::Source::RouterModels,
            Self::Dsh => tierbridge_catalog::Source::Dsh,
            Self::Freellmapi => tierbridge_catalog::Source::Freellmapi,
        }
    }
}

#[derive(Args)]
struct BuildArgs {
    /// Source adapter to run; repeat it to build several sources in one run.
    #[arg(long, value_enum, required = true)]
    source: Vec<Source>,
    /// Saved snapshot to read, one `--input` per `--source`, paired in the order given.
    #[arg(
        long,
        value_name = "FILE",
        required_unless_present = "fetch",
        conflicts_with = "fetch"
    )]
    input: Vec<PathBuf>,
    /// Download every snapshot from its source instead.
    #[arg(long)]
    fetch: bool,
    /// Directory for `observations.<source>.jsonl`, `catalog.<source>.json` and
    /// `report.<source>.json` of every source.
    #[arg(long, value_name = "DIR")]
    out_dir: PathBuf,
    /// Fail (exit 3, write nothing) if any value had to be rejected.
    #[arg(long)]
    strict: bool,
    /// Fail if a source holds fewer models than this: `N` for every source, or `SOURCE=N`
    /// (repeatable; a `SOURCE=N` beats a bare `N`).
    #[arg(long, value_name = "[SOURCE=]N")]
    min_models: Vec<String>,
    /// When the `--input` files were downloaded (RFC 3339), for all of them. Defaults to each
    /// file's modification time.
    #[arg(long, value_name = "RFC3339", requires = "input")]
    fetched_at: Option<String>,
    /// After building, merge the gateway sources with the reference catalogs (models.dev,
    /// modelparams.dev) into `catalog.json` and `report.merge.json` in `--out-dir`.
    #[arg(long)]
    merge: bool,
    /// `configured` overrides for the merge: `{ "<model key>": { <shared fields> } }`. A field
    /// set there wins over every source with the rule `configured`.
    #[arg(long, value_name = "FILE", requires = "merge")]
    overrides: Option<PathBuf>,
}

#[derive(Debug)]
struct Failure {
    code: u8,
    message: String,
}

impl From<CatalogError> for Failure {
    fn from(e: CatalogError) -> Self {
        Self {
            code: if matches!(e, CatalogError::StrictRejections { .. }) {
                EXIT_STRICT
            } else {
                EXIT_FAILURE
            },
            message: e.to_string(),
        }
    }
}

fn failure(message: impl Into<String>) -> Failure {
    Failure {
        code: EXIT_FAILURE,
        message: message.into(),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Catalog {
            action: CatalogCommand::Build(args),
        } => build(&args),
        Command::Catalog {
            action: CatalogCommand::Match(args),
        } => match_ids(&args),
        Command::Export {
            target: ExportCommand::Cpa(args),
        } => export_cpa_cmd(&args),
        Command::Export {
            target: ExportCommand::Omp(args),
        } => omp::export(&args),
        Command::Apply {
            target: ApplyCommand::Omp(args),
        } => omp::apply(&args),
        Command::Snapshot {
            target: SnapshotCommand::OmpLive(args),
        } => omp::snapshot_omp_live(&args),
        Command::Sync { action } => sync::dispatch(&action),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(f) => {
            eprintln!("error: {}", f.message);
            ExitCode::from(f.code)
        }
    }
}

/// A snapshot and where it came from.
struct Snapshot {
    bytes: Vec<u8>,
    fetched_at_ms: i64,
    fetch: Option<FetchMeta>,
    provenance: String,
}

fn ms_since_epoch(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn load_snapshot(
    args: &BuildArgs,
    source: Source,
    input: Option<&Path>,
) -> Result<Snapshot, Failure> {
    let Some(path) = input else {
        let id = source.catalog().id();
        return match source.catalog().api_url() {
            Some(url) => fetch(url),
            None => Err(failure(format!(
                "--source {id} has no download URL: save its snapshot and pass it with --input"
            ))),
        };
    };
    let bytes =
        std::fs::read(path).map_err(|e| failure(format!("cannot read {}: {e}", path.display())))?;
    let (fetched_at_ms, provenance) = match &args.fetched_at {
        Some(value) => (parse_rfc3339_ms(value)?, "--fetched-at".to_owned()),
        None => {
            let modified = std::fs::metadata(path)
                .and_then(|m| m.modified())
                .map_err(|e| failure(format!("cannot read mtime of {}: {e}", path.display())))?;
            (
                ms_since_epoch(modified),
                "file modification time".to_owned(),
            )
        }
    };
    Ok(Snapshot {
        bytes,
        fetched_at_ms,
        fetch: None,
        provenance,
    })
}

fn fetch(url: &str) -> Result<Snapshot, Failure> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(FETCH_TIMEOUT))
        .build()
        .into();
    let mut response = agent
        .get(url)
        .call()
        .map_err(|e| failure(format!("fetching {url} failed: {e}")))?;
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let meta = FetchMeta {
        url: url.to_owned(),
        etag: header("etag"),
        last_modified: header("last-modified"),
    };
    let bytes = response
        .body_mut()
        .with_config()
        .limit(FETCH_MAX_BYTES)
        .read_to_vec()
        .map_err(|e| failure(format!("reading {url} failed: {e}")))?;
    Ok(Snapshot {
        bytes,
        fetched_at_ms: ms_since_epoch(SystemTime::now()),
        fetch: Some(meta),
        provenance: "HTTP fetch".to_owned(),
    })
}

/// `--source`/`--input` pairs: no source twice, and one `--input` per `--source` unless fetching.
fn pair_sources(args: &BuildArgs) -> Result<Vec<(Source, Option<&Path>)>, Failure> {
    for (i, source) in args.source.iter().enumerate() {
        if args.source[..i].contains(source) {
            return Err(failure(format!(
                "--source {} given more than once",
                source.catalog().id()
            )));
        }
    }
    if args.fetch {
        return Ok(args.source.iter().map(|&s| (s, None)).collect());
    }
    if args.input.len() != args.source.len() {
        return Err(failure(format!(
            "{} --source but {} --input: give one --input per --source, in the same order",
            args.source.len(),
            args.input.len()
        )));
    }
    Ok(args
        .source
        .iter()
        .zip(&args.input)
        .map(|(&s, p)| (s, Some(p.as_path())))
        .collect())
}

/// `--min-models` values: `(source or all, minimum)`.
fn parse_min_models(specs: &[String]) -> Result<Vec<(Option<Source>, usize)>, Failure> {
    specs
        .iter()
        .map(|spec| {
            let (source, count) = match spec.split_once('=') {
                Some((name, count)) => {
                    let source = Source::from_str(name, true)
                        .map_err(|_| failure(format!("--min-models: unknown source `{name}`")))?;
                    (Some(source), count)
                }
                None => (None, spec.as_str()),
            };
            let count = count.parse::<usize>().map_err(|_| {
                failure(format!(
                    "--min-models: `{count}` is not a non-negative integer"
                ))
            })?;
            Ok((source, count))
        })
        .collect()
}

fn min_models_for(specs: &[(Option<Source>, usize)], source: Source) -> Option<usize> {
    let specific = specs.iter().rfind(|(s, _)| *s == Some(source));
    specific
        .or_else(|| specs.iter().rfind(|(s, _)| s.is_none()))
        .map(|&(_, n)| n)
}

fn build(args: &BuildArgs) -> Result<(), Failure> {
    let pairs = pair_sources(args)?;
    let overrides = args.overrides.as_deref().map(load_overrides).transpose()?;
    let min_models = parse_min_models(&args.min_models)?;
    // Every source is built before anything is written: one structural failure aborts the run.
    let mut runs: Vec<(Snapshot, BuildOutput)> = Vec::new();
    for (source, input) in pairs {
        let snapshot = load_snapshot(args, source, input)?;
        let out = source.catalog().build(
            SourceInput {
                bytes: &snapshot.bytes,
                fetched_at_ms: snapshot.fetched_at_ms,
                fetch: snapshot.fetch.as_ref(),
            },
            &BuildOptions {
                min_models: min_models_for(&min_models, source),
            },
        )?;
        runs.push((snapshot, out));
    }
    let rejections: usize = runs.iter().map(|(_, o)| o.report.rejections.len()).sum();
    if args.strict && rejections > 0 {
        for (snapshot, out) in &runs {
            eprint!("{}", summary(out, snapshot, &[]));
        }
        return Err(CatalogError::StrictRejections { count: rejections }.into());
    }
    let outs: Vec<&BuildOutput> = runs.iter().map(|(_, o)| o).collect();
    // Merging can fail structurally (no gateway source): do it before anything is written.
    let merged = if args.merge {
        Some(merge_catalog_with(&outs, overrides.as_ref())?)
    } else {
        None
    };
    let written = write_outputs(&args.out_dir, &outs)?;
    let mut files = written.iter();
    for (snapshot, out) in &runs {
        // `write_outputs` writes three files per build, in order.
        let own: Vec<(&Path, u64)> = files
            .by_ref()
            .take(3)
            .map(|w| (w.path.as_path(), w.bytes))
            .collect();
        print!("{}", summary(out, snapshot, &own));
    }
    if let Some(merged) = merged {
        let written = write_merge(&args.out_dir, &merged)?;
        let own: Vec<(&Path, u64)> = written
            .iter()
            .map(|w| (w.path.as_path(), w.bytes))
            .collect();
        print!("{}", merge_summary(&merged, &own));
    }
    Ok(())
}

fn load_overrides(path: &Path) -> Result<Overrides, Failure> {
    let bytes =
        std::fs::read(path).map_err(|e| failure(format!("cannot read {}: {e}", path.display())))?;
    Ok(Overrides::parse(&bytes, &path.display().to_string())?)
}

/// Loads one observations file: `(sourceId, keys)`.
fn load_keys(path: &Path) -> Result<(String, Vec<tierbridge_catalog::matching::IdKey>), Failure> {
    let bytes =
        std::fs::read(path).map_err(|e| failure(format!("cannot read {}: {e}", path.display())))?;
    let keys = tierbridge_catalog::matching::read_id_keys(&bytes)
        .map_err(|e| failure(format!("{}: {e}", path.display())))?;
    let source = keys[0].source_id.clone();
    if let Some(other) = keys.iter().find(|k| k.source_id != source) {
        return Err(failure(format!(
            "{}: mixes sources `{source}` and `{}`",
            path.display(),
            other.source_id
        )));
    }
    Ok((source, keys))
}

fn match_ids(args: &MatchArgs) -> Result<(), Failure> {
    use tierbridge_catalog::matching::{Reference, match_report};
    let load = |paths: &[PathBuf]| {
        paths
            .iter()
            .map(|p| load_keys(p))
            .collect::<Result<Vec<_>, _>>()
    };
    let gateways = load(&args.gateway)?;
    let counted = load(&args.reference)?;
    let probes = load(&args.probe)?;
    let gateway_inputs: Vec<(&str, &[_])> = gateways
        .iter()
        .map(|(n, k)| (n.as_str(), k.as_slice()))
        .collect();
    let references: Vec<Reference<'_>> = counted
        .iter()
        .map(|r| (r, false))
        .chain(probes.iter().map(|r| (r, true)))
        .map(|((name, keys), probe)| Reference { name, keys, probe })
        .collect();
    let report = match_report(&gateway_inputs, &references);
    if args.json {
        let text = serde_json::to_string_pretty(&report)
            .map_err(|e| failure(format!("cannot serialise the report: {e}")))?;
        println!("{text}");
        return Ok(());
    }
    for g in &report.gateways {
        println!("{}  ids {}", g.source_id, g.ids);
        for (name, t) in &g.per_reference {
            let probe = if probes.iter().any(|(p, _)| p == name) {
                " (probe)"
            } else {
                ""
            };
            println!(
                "  {name}{probe}: T1 {}  T2 {}  T3 {}  T4 {}  unmatched {}",
                t.t1, t.t2, t.t3, t.t4, t.unmatched
            );
        }
        println!(
            "  either {}  both {}  neither {}  exact only (T1+T2) {}",
            g.either, g.all, g.neither, g.exact_only
        );
    }
    let u = &report.union;
    let refs: Vec<String> = u
        .per_reference
        .iter()
        .map(|(n, c)| format!("{n} {c}"))
        .collect();
    println!(
        "union  {} unique ids ({} in more than one source): {}; either {} of {} (neither {})",
        u.unique_ids,
        u.in_more_than_one_source,
        refs.join(", "),
        u.either,
        u.unique_ids,
        u.neither
    );
    Ok(())
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 3] = ["B", "KiB", "MiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn summary(out: &BuildOutput, snapshot: &Snapshot, files: &[(&Path, u64)]) -> String {
    let r = &out.report;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "{}  sha256 {}  {}  fetched {} ({})",
        r.source.descriptor.source_id,
        r.source.sha256.get(..16).unwrap_or(&r.source.sha256),
        human_bytes(r.source.bytes as u64),
        r.source.fetched_at,
        snapshot.provenance
    );
    let c = &r.counts;
    let _ = writeln!(
        s,
        "providers {}  observations {}  models {}",
        c.providers, c.observations, c.models
    );
    let _ = writeln!(s, "rejected values: {}", c.rejections);
    for d in r.rejections.iter().take(SUMMARY_LIST_LIMIT) {
        let _ = writeln!(
            s,
            "  {}/{}  {} = {}  ({})",
            d.provider, d.id, d.field, d.value, d.reason
        );
    }
    if c.rejections > SUMMARY_LIST_LIMIT {
        let _ = writeln!(
            s,
            "  ... {} more in report.{}.json",
            c.rejections - SUMMARY_LIST_LIMIT,
            r.source.descriptor.source_id
        );
    }
    let _ = writeln!(
        s,
        "sentinels mapped: {}  (of which limits published as 0 -> unknown: {})",
        c.sentinels, c.zero_limits_mapped_to_unknown
    );
    for (field, count) in &c.sentinels_by_field {
        let _ = writeln!(s, "  {field}: {count}");
    }
    for (path, bytes) in files {
        let _ = writeln!(s, "wrote {}  ({})", path.display(), human_bytes(*bytes));
    }
    s
}

fn merge_summary(m: &MergeOutput, files: &[(&Path, u64)]) -> String {
    let r = &m.report;
    let mut s = format!("merged  {} models\n", r.models);
    for g in &r.gateways {
        let _ = write!(
            s,
            "  {}  rows {}  matched {} ({:.1}%)",
            g.source_id, g.rows, g.matched, g.matched_pct
        );
        if g.retired_skipped > 0 {
            let _ = write!(s, "  retired rows skipped {}", g.retired_skipped);
        }
        s.push('\n');
    }
    let _ = writeln!(
        s,
        "placeholder winners: {}  cost known/free/unknown: {}/{}/{}  gateway zero costs: {}",
        r.placeholder_winners.total,
        r.cost.known,
        r.cost.free,
        r.cost.unknown,
        r.cost.gateway_zero_winners
    );
    let conflicts: Vec<String> = r
        .fields
        .iter()
        .filter(|(_, f)| f.conflicts > 0)
        .map(|(n, f)| {
            format!(
                "{} {}",
                serde_json::json!(n).as_str().unwrap_or("?"),
                f.conflicts
            )
        })
        .collect();
    let _ = writeln!(s, "conflicts: {}", conflicts.join(", "));
    if let Some(o) = &r.overrides {
        let _ = writeln!(
            s,
            "overrides: {} entries, {} models overridden",
            o.entries, o.models_overridden
        );
    }
    if !r.diagnostics.is_empty() {
        let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
        for d in &r.diagnostics {
            *kinds.entry(d.kind.as_str()).or_default() += 1;
        }
        let kinds: Vec<String> = kinds.iter().map(|(k, n)| format!("{k} {n}")).collect();
        let _ = writeln!(
            s,
            "merge diagnostics: {} (see report.merge.json)",
            kinds.join(", ")
        );
        // what asks for action is listed; the per-row bulk (unmodelled, toggles) stays in the report
        let actionable = r
            .diagnostics
            .iter()
            .filter(|d| matches!(d.kind.as_str(), "unmatched-override" | "duplicate-key"));
        for d in actionable.take(SUMMARY_LIST_LIMIT) {
            let _ = writeln!(
                s,
                "  {} {}  {}",
                d.kind,
                d.key.as_deref().unwrap_or(&d.id),
                d.reason
            );
        }
    }
    for (path, bytes) in files {
        let _ = writeln!(s, "wrote {}  ({})", path.display(), human_bytes(*bytes));
    }
    s
}

/// Default provider `name:`: `tierbridge-` plus the gateway id with every non-alphanumeric run as `-`.
fn default_provider_name(gateway: &str) -> String {
    let slug: String = gateway
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("tierbridge-{}", slug.trim_matches('-'))
}

fn export_cpa_cmd(args: &CpaArgs) -> Result<(), Failure> {
    let bytes = std::fs::read(&args.catalog)
        .map_err(|e| failure(format!("cannot read {}: {e}", args.catalog.display())))?;
    let api_key = match &args.api_key_env {
        None => None,
        Some(var) => match std::env::var(var) {
            Ok(value) if !value.is_empty() => Some(value),
            _ => return Err(failure(format!("--api-key-env: `{var}` is unset or empty"))),
        },
    };
    let name = args
        .name
        .clone()
        .unwrap_or_else(|| default_provider_name(&args.gateway));
    let export = export_cpa(
        &bytes,
        &CpaOptions {
            gateway: &args.gateway,
            provider_name: &name,
            base_url: &args.base_url,
            api_key: api_key.as_deref(),
            alias_prefix: &args.alias_prefix,
            model_prefix: args.prefix.as_deref(),
        },
    )?;
    match &args.out {
        Some(path) => write_private(path, export.yaml.as_bytes())
            .map_err(|e| failure(format!("cannot write {}: {e}", path.display())))?,
        None => print!("{}", export.yaml),
    }
    let mut counts: BTreeMap<SkipReason, usize> = BTreeMap::new();
    for (_, reason) in &export.skipped {
        *counts.entry(*reason).or_default() += 1;
    }
    let skipped = counts
        .iter()
        .map(|(reason, n)| format!("{} {n}", reason.label()))
        .collect::<Vec<_>>()
        .join(", ");
    eprintln!(
        "exported {} models from {} as provider `{name}`{}",
        export.models.len(),
        args.gateway,
        if skipped.is_empty() {
            String::new()
        } else {
            format!(" (skipped: {skipped})")
        }
    );
    if api_key.is_none() {
        eprintln!("warning: no --api-key-env: the key is the placeholder {PLACEHOLDER_KEY}");
    }
    Ok(())
}

/// Write `bytes` to `path`, owner-only on Unix because the file can hold an upstream key.
#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    // `mode` only applies to a new file; tighten one that already existed.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

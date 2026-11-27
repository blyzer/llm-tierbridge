//! `tierbridge apply omp`: a TEXT-level edit of omp's `models.yml`.
//!
//! The file holds secrets and comments, so it is never re-serialized: every line outside the
//! managed blocks is kept byte for byte. A managed block sits inside a provider, delimited by
//!
//! ```text
//!     # >>> tierbridge-managed modelOverrides (do not edit) >>>
//!     modelOverrides:
//!       <model id>: ...
//!     # <<< tierbridge-managed modelOverrides <<<
//! ```
//!
//! [`apply`] sets the managed state to exactly what an overrides file says (all old managed
//! blocks go, one block per provider of the file comes), so applying the same file twice gives
//! the same bytes; [`revert`] removes every managed block and gives back the original bytes. A
//! provider that carries its own (unmanaged) `modelOverrides` is refused. The edited text is
//! parsed as YAML before anything is written, and must differ from the original only by the
//! managed `modelOverrides`.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_yaml::Value as Yaml;

use crate::omp_error::OmpError;
use crate::time::format_rfc3339;

pub const MANAGED_START: &str = "# >>> tierbridge-managed modelOverrides (do not edit) >>>";
pub const MANAGED_END: &str = "# <<< tierbridge-managed modelOverrides <<<";

/// Where `modelOverrides:` sits in an overrides file (`providers:` > provider > `modelOverrides:`).
const FILE_PROVIDER_INDENT: usize = 2;
const FILE_SECTION_INDENT: usize = 4;

fn bad(msg: impl Into<String>) -> OmpError {
    OmpError::ModelsYml(msg.into())
}

// ---- line helpers (lines keep their terminator) ----

fn body(line: &str) -> &str {
    let l = line.strip_suffix('\n').unwrap_or(line);
    l.strip_suffix('\r').unwrap_or(l)
}
fn eol_of(line: &str) -> &str {
    &line[body(line).len()..]
}
fn indent(line: &str) -> usize {
    body(line).bytes().take_while(|b| *b == b' ').count()
}
fn is_blank(line: &str) -> bool {
    body(line).trim().is_empty()
}
fn is_comment(line: &str) -> bool {
    body(line).trim_start().starts_with('#')
}
fn is_content(line: &str) -> bool {
    !is_blank(line) && !is_comment(line)
}
fn tab_indented(line: &str) -> bool {
    body(line)
        .chars()
        .take_while(|c| c.is_whitespace())
        .any(|c| c == '\t')
}

/// `key: rest` of a block-mapping line; `None` for anything else (list items, scalars).
fn split_key(line: &str) -> Option<(String, &str)> {
    let s = body(line).trim_start();
    if let Some(q) = s.chars().next().filter(|c| *c == '"' || *c == '\'') {
        let mut escaped = false;
        let mut end = None;
        for (i, c) in s.char_indices().skip(1) {
            if q == '"' && escaped {
                escaped = false;
            } else if q == '"' && c == '\\' {
                escaped = true;
            } else if c == q {
                end = Some(i);
                break;
            }
        }
        let end = end?;
        let rest = s[end + 1..].trim_start().strip_prefix(':')?;
        return Some((s[1..end].to_owned(), rest));
    }
    let b = s.as_bytes();
    let i = (0..b.len()).find(|&i| {
        b[i] == b':'
            && b.get(i + 1)
                .is_none_or(|n| matches!(n, b' ' | b'\t' | b'\r'))
    })?;
    let key = s[..i].trim_end();
    if key.is_empty() || key.starts_with('#') || key.starts_with("- ") || key == "-" {
        return None;
    }
    Some((key.to_owned(), &s[i + 1..]))
}

/// What follows the colon, without a trailing comment.
fn value_of(rest: &str) -> &str {
    let r = rest.trim();
    if r.starts_with('#') { "" } else { r }
}

// ---- managed blocks ----

/// `text` without its managed blocks, and how many there were. Unbalanced markers are an error:
/// guessing where a block ends could delete the user's own lines.
pub fn strip_managed(text: &str) -> Result<(String, usize), OmpError> {
    let mut out = String::with_capacity(text.len());
    let mut open_at = None;
    let mut blocks = 0;
    let mut last_removed_had_no_eol = false;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        let t = body(line).trim();
        if t == MANAGED_START {
            if let Some(first) = open_at {
                return Err(bad(format!(
                    "managed block opened at line {first} is not closed before line {}",
                    i + 1
                )));
            }
            open_at = Some(i + 1);
        } else if t == MANAGED_END {
            if open_at.take().is_none() {
                return Err(bad(format!(
                    "line {} closes a managed block that was never opened",
                    i + 1
                )));
            }
            blocks += 1;
            last_removed_had_no_eol = eol_of(line).is_empty();
        } else if open_at.is_none() {
            out.push_str(line);
            last_removed_had_no_eol = false;
        }
    }
    if let Some(first) = open_at {
        return Err(bad(format!(
            "managed block opened at line {first} is never closed"
        )));
    }
    if last_removed_had_no_eol {
        // `apply` ended the file with the block and no final newline: give the file back as it was.
        if out.ends_with("\r\n") {
            out.truncate(out.len() - 2);
        } else if out.ends_with('\n') {
            out.truncate(out.len() - 1);
        }
    }
    Ok((out, blocks))
}

// ---- the overrides file ----

/// One provider of an overrides file: the lines to put between the markers.
#[derive(Debug, Clone)]
pub struct ProviderOverrides {
    pub name: String,
    /// Lines (no terminators), at the indentation `tierbridge export omp` writes: comments and
    /// `modelOverrides:` at 4 spaces.
    pub lines: Vec<String>,
    pub models: usize,
    value: Yaml,
}

#[derive(Debug, Clone, Default)]
pub struct OverridesDoc {
    pub providers: Vec<ProviderOverrides>,
}

/// Reads what `tierbridge export omp` writes; anything else is refused.
pub fn parse_overrides(text: &str) -> Result<OverridesDoc, OmpError> {
    let err = |m: String| OmpError::Overrides(m);
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    if let Some(i) = lines.iter().position(|l| tab_indented(l)) {
        return Err(err(format!("line {} is indented with a tab", i + 1)));
    }
    let root: Yaml = serde_yaml::from_str(text).map_err(|e| err(format!("not valid YAML: {e}")))?;
    let shape = "expected `providers: {<name>: {modelOverrides: {<id>: ...}}}`";
    let Some(root) = root.as_mapping() else {
        return Err(err(shape.into()));
    };
    if root.len() != 1 || root.get("providers").is_none() {
        return Err(err(format!(
            "{shape}; the top level may only hold `providers`"
        )));
    }
    let mut named: Vec<(String, usize, Yaml)> = Vec::new();
    match &root["providers"] {
        Yaml::Mapping(map) => {
            for (k, v) in map {
                let name = k
                    .as_str()
                    .ok_or_else(|| err("a provider name is not a string".into()))?;
                let Some(only) = v.as_mapping().filter(|m| m.len() == 1) else {
                    return Err(err(format!(
                        "provider `{name}` may only hold `modelOverrides`"
                    )));
                };
                let Some(models) = only.get("modelOverrides").and_then(Yaml::as_mapping) else {
                    return Err(err(format!(
                        "provider `{name}` has no `modelOverrides` mapping"
                    )));
                };
                named.push((
                    name.to_owned(),
                    models.len(),
                    only["modelOverrides"].clone(),
                ));
            }
        }
        _ => return Err(err(shape.into())),
    }

    // Text of each provider section, in file order.
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    let mut seen_providers = false;
    for (i, line) in lines.iter().enumerate() {
        if !is_content(line) {
            if let Some((_, acc)) = sections.last_mut() {
                acc.push(body(line).to_owned());
            }
            continue;
        }
        let (n, key) = (i + 1, split_key(line));
        if !seen_providers {
            if indent(line) != 0 || key.is_none_or(|(k, _)| k != "providers") {
                return Err(err(format!("line {n}: expected `providers:`")));
            }
            seen_providers = true;
            continue;
        }
        match indent(line) {
            0 => return Err(err(format!("line {n}: unexpected top-level content"))),
            FILE_PROVIDER_INDENT => {
                let Some((name, _)) = key.filter(|(_, r)| value_of(r).is_empty()) else {
                    return Err(err(format!("line {n}: expected `<provider>:`")));
                };
                sections.push((name, Vec::new()));
            }
            d if d >= FILE_SECTION_INDENT => {
                let Some((_, acc)) = sections.last_mut() else {
                    return Err(err(format!("line {n}: content outside a provider")));
                };
                acc.push(body(line).to_owned());
            }
            _ => return Err(err(format!("line {n}: unexpected indentation"))),
        }
    }
    if sections.len() != named.len() || sections.iter().zip(&named).any(|(s, n)| s.0 != n.0) {
        return Err(err(
            "provider sections do not match the YAML structure".into()
        ));
    }
    let providers = sections
        .into_iter()
        .zip(named)
        .map(|((name, mut lines), (_, models, value))| {
            while lines.last().is_some_and(|l| {
                is_blank(l) || (is_comment(l) && indent(l) <= FILE_PROVIDER_INDENT)
            }) {
                lines.pop();
            }
            let has_key = lines.iter().any(|l| {
                indent(l) == FILE_SECTION_INDENT
                    && split_key(l)
                        .is_some_and(|(k, r)| k == "modelOverrides" && value_of(r).is_empty())
            });
            if has_key {
                Ok(ProviderOverrides {
                    name: name.clone(),
                    lines,
                    models,
                    value,
                })
            } else {
                Err(err(format!(
                    "provider `{name}`: `modelOverrides:` not found at {FILE_SECTION_INDENT} spaces"
                )))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(OverridesDoc { providers })
}

// ---- the plan ----

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inserted {
    pub provider: String,
    pub models: usize,
    /// Lines of the block, markers included.
    pub lines: usize,
}

#[derive(Debug, Clone)]
pub struct EditPlan {
    /// The whole new file.
    pub text: String,
    /// `text` differs from the original.
    pub changed: bool,
    /// Managed blocks the original had (and that are gone from `text`).
    pub removed_blocks: usize,
    pub inserted: Vec<Inserted>,
    /// Providers of the overrides file with no model: nothing to insert.
    pub empty_providers: Vec<String>,
}

struct Section {
    name: String,
    /// Where a new block goes (after the provider's last content or inner comment).
    insert_at: usize,
    child_indent: usize,
}

fn scan_providers(lines: &[&str]) -> Result<Vec<Section>, OmpError> {
    if let Some(i) = lines.iter().position(|l| is_content(l) && tab_indented(l)) {
        return Err(bad(format!("line {} is indented with a tab", i + 1)));
    }
    let top = lines
        .iter()
        .position(|l| {
            is_content(l) && indent(l) == 0 && split_key(l).is_some_and(|(k, _)| k == "providers")
        })
        .ok_or_else(|| bad("no top-level `providers:` key"))?;
    if !value_of(split_key(lines[top]).map_or("", |(_, r)| r)).is_empty() {
        return Err(bad("`providers:` is not a block mapping"));
    }
    let range_end = (top + 1..lines.len())
        .find(|&i| is_content(lines[i]) && indent(lines[i]) == 0)
        .unwrap_or(lines.len());
    let Some(first) = (top + 1..range_end).find(|&i| is_content(lines[i])) else {
        return Err(bad("`providers:` has no providers"));
    };
    let p = indent(lines[first]);
    let mut starts: Vec<(usize, String, String)> = Vec::new();
    for (i, line) in lines.iter().enumerate().take(range_end).skip(top + 1) {
        if !is_content(line) || indent(line) > p {
            continue;
        }
        if indent(line) < p {
            return Err(bad(format!(
                "line {}: inconsistent indentation under `providers:`",
                i + 1
            )));
        }
        let Some((name, rest)) = split_key(line) else {
            return Err(bad(format!("line {}: not a provider entry", i + 1)));
        };
        starts.push((i, name, value_of(rest).to_owned()));
    }
    let mut sections = Vec::with_capacity(starts.len());
    for (n, (key_line, name, value)) in starts.iter().enumerate() {
        if !value.is_empty() {
            return Err(bad(format!(
                "provider `{name}` (line {}) is not a block mapping",
                key_line + 1
            )));
        }
        let end = starts.get(n + 1).map_or(range_end, |s| s.0);
        let child_indent = (key_line + 1..end)
            .find(|&i| is_content(lines[i]))
            .map_or(p + 2, |i| indent(lines[i]));
        for (i, line) in lines.iter().enumerate().take(end).skip(key_line + 1) {
            if is_content(line)
                && indent(line) == child_indent
                && split_key(line).is_some_and(|(k, _)| k == "modelOverrides")
            {
                return Err(bad(format!(
                    "provider `{name}` already has a modelOverrides of its own (line {}) that tierbridge does not manage; merge or remove it first",
                    i + 1
                )));
            }
        }
        let mut insert_at = end;
        while insert_at > key_line + 1
            && (is_blank(lines[insert_at - 1])
                || (is_comment(lines[insert_at - 1]) && indent(lines[insert_at - 1]) <= p))
        {
            insert_at -= 1;
        }
        sections.push(Section {
            name: name.clone(),
            insert_at,
            child_indent,
        });
    }
    Ok(sections)
}

fn parse_yaml(text: &str, what: &str) -> Result<Yaml, OmpError> {
    serde_yaml::from_str(text).map_err(|e| bad(format!("{what} is not valid YAML: {e}")))
}

/// Sets the managed blocks of `original` to exactly `doc`.
pub fn apply(original: &str, doc: &OverridesDoc) -> Result<EditPlan, OmpError> {
    let (stripped, removed_blocks) = strip_managed(original)?;
    let before = parse_yaml(&stripped, "models.yml")?;
    let lines: Vec<&str> = stripped.split_inclusive('\n').collect();
    let sections = scan_providers(&lines)?;
    let eol = lines
        .first()
        .map(|l| eol_of(l))
        .filter(|e| !e.is_empty())
        .unwrap_or("\n");

    let mut inserts: Vec<(usize, Vec<String>)> = Vec::new();
    let mut inserted = Vec::new();
    let mut empty_providers = Vec::new();
    for p in &doc.providers {
        if p.models == 0 {
            empty_providers.push(p.name.clone());
            continue;
        }
        let Some(section) = sections.iter().find(|s| s.name == p.name) else {
            return Err(bad(format!(
                "provider `{}` is not defined under `providers:`",
                p.name
            )));
        };
        let pad = |n: usize| " ".repeat(n);
        let c = section.child_indent;
        let mut block = vec![format!("{}{MANAGED_START}", pad(c))];
        for l in &p.lines {
            block.push(if is_blank(l) {
                String::new()
            } else {
                // The file writes 4 spaces for `modelOverrides:`; this provider uses `c`.
                format!(
                    "{}{}",
                    pad((indent(l) + c).saturating_sub(FILE_SECTION_INDENT)),
                    l.trim_start()
                )
            });
        }
        block.push(format!("{}{MANAGED_END}", pad(c)));
        inserted.push(Inserted {
            provider: p.name.clone(),
            models: p.models,
            lines: block.len(),
        });
        inserts.push((section.insert_at, block));
    }

    let mut out: Vec<String> = lines.iter().map(|l| (*l).to_owned()).collect();
    inserts.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    for (at, block) in inserts {
        let at_eof = at == out.len();
        let mut new: Vec<String> = block.into_iter().map(|l| format!("{l}{eol}")).collect();
        if at_eof && out.last().is_some_and(|l| eol_of(l).is_empty()) {
            // The file has no final newline: end it the same way (see `strip_managed`).
            if let Some(last) = out.last_mut() {
                last.push_str(eol);
            }
            if let Some(end) = new.last_mut() {
                end.truncate(end.len() - eol.len());
            }
        }
        out.splice(at..at, new);
    }
    let text: String = out.concat();

    // The edit may only add the managed `modelOverrides` of the target providers.
    let mut after = parse_yaml(&text, "the edited models.yml")?;
    for p in doc.providers.iter().filter(|p| p.models > 0) {
        let slot = after
            .get_mut("providers")
            .and_then(|v| v.get_mut(p.name.as_str()))
            .and_then(Yaml::as_mapping_mut)
            .ok_or_else(|| bad(format!("internal: provider `{}` lost in the edit", p.name)))?;
        match slot.remove("modelOverrides") {
            Some(v) if v == p.value => {}
            _ => {
                return Err(bad(format!(
                    "internal: the inserted modelOverrides of `{}` do not parse back to the overrides file",
                    p.name
                )));
            }
        }
    }
    if after != before {
        return Err(bad(
            "internal: the edit changed more than the managed blocks",
        ));
    }
    Ok(EditPlan {
        changed: text != original,
        text,
        removed_blocks,
        inserted,
        empty_providers,
    })
}

/// Removes every managed block.
pub fn revert(original: &str) -> Result<EditPlan, OmpError> {
    let (text, removed_blocks) = strip_managed(original)?;
    Ok(EditPlan {
        changed: text != original,
        text,
        removed_blocks,
        inserted: Vec::new(),
        empty_providers: Vec::new(),
    })
}

// ---- writing ----

/// `YYYYMMDDTHHMMSSZ` of `ms` since the epoch.
fn compact_utc(ms: i64) -> String {
    let t: String = format_rfc3339(ms)
        .chars()
        .take(19)
        .filter(|c| !matches!(c, '-' | ':'))
        .collect();
    format!("{t}Z")
}

fn create_private(path: &Path) -> std::io::Result<fs::File> {
    let mut o = fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        o.mode(0o600);
    }
    o.open(path)
}

/// Where the backup of `path` goes: `<file>.tierbridge-<ts>.bak` next to it.
pub fn backup_path(path: &Path, now_ms: i64) -> PathBuf {
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    path.with_file_name(format!("{name}.tierbridge-{}.bak", compact_utc(now_ms)))
}

/// Writes `new_text` over `path`: a backup of `original` first (same permissions; never
/// overwritten), then a temp file in the same directory renamed into place.
pub fn commit(
    path: &Path,
    original: &[u8],
    new_text: &str,
    now_ms: i64,
) -> Result<PathBuf, OmpError> {
    let io = |p: &Path, e| OmpError::io(p, e);
    let real = fs::canonicalize(path).map_err(|e| io(path, e))?;
    let perms = fs::metadata(&real).map_err(|e| io(&real, e))?.permissions();

    let mut backup = backup_path(&real, now_ms);
    let stem = backup.with_extension("");
    let mut n = 0;
    let mut file = loop {
        match create_private(&backup) {
            Ok(f) => break f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 100 => {
                n += 1;
                backup = PathBuf::from(format!("{}-{n}.bak", stem.display()));
            }
            Err(e) => return Err(io(&backup, e)),
        }
    };
    file.write_all(original)
        .and_then(|()| file.sync_all())
        .map_err(|e| io(&backup, e))?;
    drop(file);
    fs::set_permissions(&backup, perms.clone()).map_err(|e| io(&backup, e))?;

    let name = real
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let tmp = real.with_file_name(format!(".{name}.tierbridge-tmp-{}", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        let mut f = create_private(&tmp)?;
        f.write_all(new_text.as_bytes())?;
        f.sync_all()?;
        drop(f);
        fs::set_permissions(&tmp, perms)?;
        fs::rename(&tmp, &real)
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(io(&tmp, e));
    }
    Ok(backup)
}

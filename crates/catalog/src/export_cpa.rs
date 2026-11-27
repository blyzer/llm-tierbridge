//! `openai-compatibility` block for CLIProxyAPI (v8 layout) from a merged `catalog.json`.
//!
//! One provider entry per gateway: every catalog row of that gateway becomes a `models[]` item whose
//! `name` is the id the gateway serves and whose `alias` is the id a client uses against CLIProxyAPI
//! (`alias_prefix` + id). The limits and capabilities the merge resolved (context window, modalities,
//! thinking levels) ride along so CLIProxyAPI advertises them instead of its defaults.
//!
//! The output is a YAML fragment, not a whole config: it goes under the `api-keys:` root of a
//! `config-version: 8` file. Scalars are emitted as JSON strings, which are valid YAML double-quoted
//! scalars, so no YAML serializer is needed and the output is byte-stable.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::Deserialize;

use crate::error::CatalogError;
use crate::model::{
    Effort, Lifecycle, LifecycleStatus, Modality, ModelKind, Thinking, ThinkingMode,
};

/// What the caller decides; everything else comes from the catalog.
#[derive(Debug, Clone)]
pub struct CpaOptions<'a> {
    /// Gateway source id as it prefixes catalog keys (`omp-live:easycliproxy`).
    pub gateway: &'a str,
    /// `name:` of the provider entry; CLIProxyAPI shows it as `owned_by` and in its logs.
    pub provider_name: &'a str,
    /// OpenAI-compatible base URL the generated entry sends to (`http://127.0.0.1:8317/v1`).
    pub base_url: &'a str,
    /// Upstream key written under `keys:`; `None` writes [`PLACEHOLDER_KEY`].
    pub api_key: Option<&'a str>,
    /// Prepended to every model id to form its client-facing alias.
    pub alias_prefix: &'a str,
    /// Optional CLIProxyAPI `prefix:` (clients then call `prefix/alias`).
    pub model_prefix: Option<&'a str>,
}

/// Written when no key was supplied; CLIProxyAPI will send it as is and the upstream will refuse it.
pub const PLACEHOLDER_KEY: &str = "REPLACE_WITH_UPSTREAM_API_KEY";

/// Why a catalog row of the chosen gateway has no `models[]` item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SkipReason {
    /// `kind` is known and is not `chat` (embeddings, images, rerankers, ...).
    NotChat,
    /// `lifecycle.status` is `retired`.
    Retired,
    /// Another row already produced the same alias; the first one in key order wins.
    DuplicateAlias,
}

impl SkipReason {
    pub fn label(self) -> &'static str {
        match self {
            Self::NotChat => "not-chat",
            Self::Retired => "retired",
            Self::DuplicateAlias => "duplicate-alias",
        }
    }
}

/// One generated `models[]` item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpaModel {
    /// Upstream id (`name:`).
    pub name: String,
    /// Client-facing id (`alias:`).
    pub alias: String,
}

#[derive(Debug, Clone)]
pub struct CpaExport {
    /// The YAML fragment, ending in a newline.
    pub yaml: String,
    /// Exported items in alias order.
    pub models: Vec<CpaModel>,
    /// `(catalog key, reason)` of every row of the gateway that was left out.
    pub skipped: Vec<(String, SkipReason)>,
}

/// The fields of a `catalog.json` row this export reads; the rest are ignored.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Row {
    key: String,
    provider: String,
    id: String,
    name: Option<String>,
    kind: Option<ModelKind>,
    context_window: Option<u64>,
    input: Option<Vec<Modality>>,
    output: Option<Vec<Modality>>,
    thinking: Option<Thinking>,
    lifecycle: Option<Lifecycle>,
}

impl Row {
    /// Source id the row came from: the key is `<sourceId>:<provider>/<id>` and the source id may
    /// itself contain `:` (`omp-live:easycliproxy`).
    fn gateway(&self) -> Option<&str> {
        let tail = format!("{}/{}", self.provider, self.id);
        self.key.strip_suffix(&tail)?.strip_suffix(':')
    }
}

/// Build the fragment for `opts.gateway` from the bytes of a merged `catalog.json`.
pub fn export_cpa(catalog: &[u8], opts: &CpaOptions<'_>) -> Result<CpaExport, CatalogError> {
    let mut rows: Vec<Row> = serde_json::from_slice(catalog)?;
    let available: BTreeSet<String> = rows
        .iter()
        .filter_map(|r| r.gateway().map(str::to_owned))
        .collect();
    rows.retain(|r| r.gateway() == Some(opts.gateway));
    if rows.is_empty() {
        return Err(CatalogError::Export(format!(
            "no rows from gateway `{}` in the catalog (gateways present: {})",
            opts.gateway,
            if available.is_empty() {
                "none".to_owned()
            } else {
                available.into_iter().collect::<Vec<_>>().join(", ")
            }
        )));
    }
    rows.sort_by(|a, b| a.key.cmp(&b.key));

    let mut skipped = Vec::new();
    let mut by_alias: BTreeMap<String, &Row> = BTreeMap::new();
    for row in &rows {
        if row.kind.is_some_and(|k| k != ModelKind::Chat) {
            skipped.push((row.key.clone(), SkipReason::NotChat));
        } else if row
            .lifecycle
            .as_ref()
            .is_some_and(|l| l.status == LifecycleStatus::Retired)
        {
            skipped.push((row.key.clone(), SkipReason::Retired));
        } else {
            let alias = format!("{}{}", opts.alias_prefix, row.id);
            match by_alias.entry(alias) {
                std::collections::btree_map::Entry::Vacant(slot) => {
                    slot.insert(row);
                }
                std::collections::btree_map::Entry::Occupied(_) => {
                    skipped.push((row.key.clone(), SkipReason::DuplicateAlias));
                }
            }
        }
    }

    let mut yaml = String::new();
    let _ = writeln!(
        yaml,
        "# Generated by `tierbridge export cpa` from gateway {} ({} models). Do not edit by hand.",
        scalar(opts.gateway),
        by_alias.len()
    );
    yaml.push_str("# Goes under the `api-keys:` root of a config-version 8 CLIProxyAPI config.\n");
    yaml.push_str("api-keys:\n  openai-compatibility:\n");
    let _ = writeln!(yaml, "    - name: {}", scalar(opts.provider_name));
    if let Some(prefix) = opts.model_prefix {
        let _ = writeln!(yaml, "      prefix: {}", scalar(prefix));
    }
    let _ = writeln!(yaml, "      base-url: {}", scalar(opts.base_url));
    yaml.push_str("      keys:\n");
    let _ = writeln!(
        yaml,
        "        - api-key: {}",
        scalar(opts.api_key.unwrap_or(PLACEHOLDER_KEY))
    );
    yaml.push_str("      models:\n");
    let mut models = Vec::with_capacity(by_alias.len());
    for (alias, row) in &by_alias {
        write_model(&mut yaml, alias, row);
        models.push(CpaModel {
            name: row.id.clone(),
            alias: alias.clone(),
        });
    }
    Ok(CpaExport {
        yaml,
        models,
        skipped,
    })
}

fn write_model(yaml: &mut String, alias: &str, row: &Row) {
    let _ = writeln!(yaml, "        - name: {}", scalar(&row.id));
    let _ = writeln!(yaml, "          alias: {}", scalar(alias));
    if let Some(name) = row
        .name
        .as_deref()
        .filter(|n| !n.is_empty() && *n != row.id)
    {
        let _ = writeln!(yaml, "          display-name: {}", scalar(name));
    }
    if let Some(window) = row.context_window.filter(|w| *w > 0) {
        let _ = writeln!(yaml, "          max-context-length: {window}");
    }
    for (key, list) in [
        ("input-modalities", &row.input),
        ("output-modalities", &row.output),
    ] {
        if let Some(list) = list.as_ref().filter(|l| !l.is_empty()) {
            let _ = writeln!(yaml, "          {key}: [{}]", modalities(list));
        }
    }
    if let Some(levels) = thinking_levels(row.thinking.as_ref()) {
        let _ = writeln!(yaml, "          thinking:\n            levels: [{levels}]");
    }
}

fn modalities(list: &[Modality]) -> String {
    let mut seen = BTreeSet::new();
    list.iter()
        .filter(|m| seen.insert(**m))
        .map(|m| format!("{m:?}").to_lowercase())
        .collect::<Vec<_>>()
        .join(", ")
}

/// CLIProxyAPI levels for a model with discrete reasoning efforts; `off` is not a level.
fn thinking_levels(thinking: Option<&Thinking>) -> Option<String> {
    let thinking = thinking.filter(|t| t.mode == ThinkingMode::Effort)?;
    let mut efforts: Vec<Effort> = thinking
        .efforts
        .iter()
        .copied()
        .filter(|e| *e != Effort::Off)
        .collect();
    efforts.sort();
    efforts.dedup();
    if efforts.is_empty() {
        return None;
    }
    Some(
        efforts
            .iter()
            .map(|e| scalar(&format!("{e:?}").to_lowercase()))
            .collect::<Vec<_>>()
            .join(", "),
    )
}

/// A YAML double-quoted scalar: JSON string syntax, with the three characters YAML treats as line
/// breaks escaped.
fn scalar(value: &str) -> String {
    let json = serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned());
    json.replace('\u{85}', "\\u0085")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_escapes_quotes_and_unicode_line_breaks() {
        assert_eq!(scalar(r#"a"b"#), r#""a\"b""#);
        assert_eq!(scalar("x\u{2028}y"), "\"x\\u2028y\"");
    }

    #[test]
    fn gateway_is_the_key_minus_provider_and_id() {
        let row = |key: &str| Row {
            key: key.into(),
            provider: "easycliproxy".into(),
            id: "a/b".into(),
            name: None,
            kind: None,
            context_window: None,
            input: None,
            output: None,
            thinking: None,
            lifecycle: None,
        };
        assert_eq!(
            row("omp-live:easycliproxy:easycliproxy/a/b").gateway(),
            Some("omp-live:easycliproxy")
        );
        assert_eq!(row("elsewhere").gateway(), None);
    }
}

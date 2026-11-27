//! The run report: the classified diff and a summary of what each source did. Rendered as
//! Markdown (`reports/<date>.md`, also the PR body) and JSON (`reports/<date>.json`).

use std::fmt::Write as _;

use serde::Serialize;

use super::diff::{Class, SourceDiff};

/// Changes listed per class and source in the Markdown report.
const MARKDOWN_LIST_LIMIT: usize = 20;
/// Changes kept per class and source in the JSON report (counts stay exact).
const JSON_LIST_LIMIT: usize = 200;
/// GitHub rejects PR bodies above 65 536 characters.
const PR_BODY_LIMIT: usize = 60_000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceLine {
    pub source: String,
    /// `refreshed`, `reused`, `input` or `skipped`.
    pub action: &'static str,
    pub reason: String,
    pub revision: Option<String>,
    /// Observations in the snapshot used.
    pub count: Option<usize>,
    /// Observations the previous state had.
    pub before: Option<usize>,
    pub rejections: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderLine {
    pub provider: String,
    pub rows: usize,
    pub with_overrides: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverridesLine {
    pub providers: Vec<ProviderLine>,
    /// The generated `overrides/omp.yml` differs from the committed one (comments aside).
    pub changed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunReport {
    pub date: String,
    pub generated_at: String,
    pub base: String,
    pub merged_models: usize,
    pub catalog_sha256: String,
    pub sources: Vec<SourceLine>,
    pub diffs: Vec<SourceDiff>,
    pub overrides: Option<OverridesLine>,
}

impl RunReport {
    fn total(&self, class: Class) -> usize {
        self.diffs.iter().map(|d| d.count(class)).sum()
    }

    fn out_of_scope(&self) -> usize {
        self.diffs.iter().map(|d| d.out_of_scope.total()).sum()
    }

    /// `+12 −3 ~7`, the tail of the PR title.
    pub fn summary(&self) -> String {
        let changed: usize = [
            Class::LimitChanged,
            Class::CostChanged,
            Class::LifecycleChanged,
            Class::EffortsChanged,
            Class::OtherChanged,
        ]
        .iter()
        .map(|c| self.total(*c))
        .sum();
        let mut s = format!(
            "+{} −{} ~{}",
            self.total(Class::Added),
            self.total(Class::Removed),
            changed
        );
        let oos = self.out_of_scope();
        if oos > 0 {
            let _ = write!(s, ", {oos} out of scope");
        }
        if self.overrides.as_ref().is_some_and(|o| o.changed) {
            s.push_str(", omp overrides updated");
        }
        s
    }

    pub fn title(&self) -> String {
        format!("sync {}: {}", self.date, self.summary())
    }

    pub fn to_json(&self) -> Vec<u8> {
        let mut capped = self.clone();
        capped.diffs = self
            .diffs
            .iter()
            .map(|d| d.capped(JSON_LIST_LIMIT))
            .collect();
        let mut bytes = serde_json::to_vec_pretty(&capped).expect("report serializes");
        bytes.push(b'\n');
        bytes
    }

    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "# {}\n", self.title());
        let _ = writeln!(
            out,
            "Generated {} against `{}`. Merged catalog: {} models (sha256 `{}`).\n",
            self.generated_at,
            self.base,
            self.merged_models,
            &self.catalog_sha256[..16.min(self.catalog_sha256.len())]
        );
        out.push_str("## Sources\n\n| source | action | rows | before | rejected | revision | why |\n|---|---|---:|---:|---:|---|---|\n");
        for s in &self.sources {
            let n = |v: Option<usize>| v.map_or("-".to_owned(), |v| v.to_string());
            let _ = writeln!(
                out,
                "| `{}` | {} | {} | {} | {} | `{}` | {} |",
                s.source,
                s.action,
                n(s.count),
                n(s.before),
                n(s.rejections),
                s.revision.as_deref().unwrap_or("-"),
                s.reason
            );
        }
        out.push_str("\n## Classified diff\n\n| source | added | removed | limit | cost | lifecycle | efforts | other | out of scope |\n|---|---:|---:|---:|---:|---:|---:|---:|---:|\n");
        for d in &self.diffs {
            let _ = writeln!(
                out,
                "| `{}` | {} | {} | {} | {} | {} | {} | {} | {} |",
                d.source,
                d.count(Class::Added),
                d.count(Class::Removed),
                d.count(Class::LimitChanged),
                d.count(Class::CostChanged),
                d.count(Class::LifecycleChanged),
                d.count(Class::EffortsChanged),
                d.count(Class::OtherChanged),
                d.out_of_scope.total()
            );
        }
        for d in self.diffs.iter().filter(|d| !d.is_empty()) {
            let _ = writeln!(out, "\n### `{}`", d.source);
            let oos = &d.out_of_scope;
            if oos.total() > 0 {
                let _ = writeln!(
                    out,
                    "\nOut of scope (non-chat), listed apart: {} added, {} removed, {} changed.",
                    oos.added, oos.removed, oos.changed
                );
            }
            for class in Class::ALL {
                let n = d.count(class);
                if n == 0 {
                    continue;
                }
                let _ = writeln!(out, "\n**{} ({n})**\n", class.label());
                let listed = d.changes.iter().filter(|c| c.class == class);
                for c in listed.take(MARKDOWN_LIST_LIMIT) {
                    let _ = writeln!(out, "- `{}`: {}", c.key, c.detail);
                }
                if n > MARKDOWN_LIST_LIMIT {
                    let _ = writeln!(
                        out,
                        "- … {} more (see the `index/` diff)",
                        n - MARKDOWN_LIST_LIMIT
                    );
                }
            }
        }
        if let Some(o) = &self.overrides {
            out.push_str("\n## omp overrides\n\n");
            let _ = writeln!(
                out,
                "`overrides/omp.yml` {}.\n",
                if o.changed { "changed" } else { "unchanged" }
            );
            for p in &o.providers {
                let _ = writeln!(
                    out,
                    "- `{}`: {} rows, {} models with overrides",
                    p.provider, p.rows, p.with_overrides
                );
            }
        }
        out.push_str(
            "\nNothing here applies itself: merge to accept, then `tierbridge sync apply`.\n",
        );
        out
    }

    /// The Markdown, cut to what GitHub accepts as a PR body.
    pub fn pr_body(&self) -> String {
        let md = self.to_markdown();
        if md.len() <= PR_BODY_LIMIT {
            return md;
        }
        let mut cut = PR_BODY_LIMIT;
        while !md.is_char_boundary(cut) {
            cut -= 1;
        }
        format!(
            "{}\n\n… report cut; the full text is in `reports/`.\n",
            &md[..cut]
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::diff::classify;
    use crate::sync::index::{IndexRow, SourceIndex};

    fn index(ids: &[&str]) -> SourceIndex {
        let mut i = SourceIndex::default();
        for id in ids {
            let row = IndexRow {
                provider: "p".into(),
                id: (*id).into(),
                auth_type: None,
                kind: Some("chat".into()),
                context_window: Some(1),
                max_input_tokens: None,
                max_tokens: None,
                cost: None,
                lifecycle: None,
                efforts: None,
                input_modalities: None,
            };
            i.rows
                .insert((row.provider.clone(), row.id.clone(), String::new()), row);
        }
        i
    }

    fn report(diff: SourceDiff) -> RunReport {
        RunReport {
            date: "2026-11-27".into(),
            generated_at: "2026-11-27T06:00:00Z".into(),
            base: "main".into(),
            merged_models: 3,
            catalog_sha256: "ab".repeat(32),
            sources: vec![],
            diffs: vec![diff],
            overrides: None,
        }
    }

    #[test]
    fn title_carries_the_counts() {
        let r = report(classify("s", &index(&["a", "b"]), &index(&["b", "c", "d"])));
        assert_eq!(r.title(), "sync 2026-11-27: +2 −1 ~0");
    }

    #[test]
    fn markdown_lists_changes_and_caps_long_lists() {
        let ids: Vec<String> = (0..30).map(|i| format!("m{i:02}")).collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let md = report(classify("s", &index(&[]), &index(&refs))).to_markdown();
        assert!(md.contains("**added (30)**"));
        assert!(md.contains("- `p/m00`:"));
        assert!(!md.contains("p/m25"));
        assert!(md.contains("… 10 more"));
    }

    #[test]
    fn pr_body_stays_under_the_github_limit() {
        let ids: Vec<String> = (0..4000).map(|i| format!("model-{i:05}")).collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let mut r = report(classify("s", &index(&[]), &index(&refs)));
        r.sources = (0..2000)
            .map(|i| SourceLine {
                source: format!("source-{i}-{}", "x".repeat(40)),
                action: "refreshed",
                reason: "cadence elapsed".into(),
                revision: None,
                count: Some(1),
                before: Some(0),
                rejections: Some(0),
            })
            .collect();
        assert!(r.pr_body().len() < 65_536);
        assert!(r.to_json().len() > 1000);
    }
}

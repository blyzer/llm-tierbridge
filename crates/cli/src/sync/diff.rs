//! Classifies the difference between the accepted `index/<source>.jsonl` and a new one:
//! added, removed, limit / cost / lifecycle / efforts changed, other, and out of scope (non-chat).

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Map, Value};

use super::index::{IndexRow, RowKey, SourceIndex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Class {
    Added,
    Removed,
    LimitChanged,
    CostChanged,
    LifecycleChanged,
    EffortsChanged,
    OtherChanged,
}

impl Class {
    pub const ALL: [Class; 7] = [
        Class::Added,
        Class::Removed,
        Class::LimitChanged,
        Class::CostChanged,
        Class::LifecycleChanged,
        Class::EffortsChanged,
        Class::OtherChanged,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::LimitChanged => "limit changed",
            Self::CostChanged => "cost changed",
            Self::LifecycleChanged => "lifecycle changed",
            Self::EffortsChanged => "efforts changed",
            Self::OtherChanged => "other changed",
        }
    }

    /// The class a changed field belongs to.
    fn of_field(field: &str) -> Self {
        match field {
            "contextWindow" | "maxInputTokens" | "maxTokens" => Self::LimitChanged,
            "cost" => Self::CostChanged,
            "lifecycle" => Self::LifecycleChanged,
            "efforts" => Self::EffortsChanged,
            _ => Self::OtherChanged,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Change {
    pub class: Class,
    pub key: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct OutOfScope {
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
}

impl OutOfScope {
    pub fn total(&self) -> usize {
        self.added + self.removed + self.changed
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SourceDiff {
    pub source: String,
    /// Rows in the accepted index / in the new one.
    pub before: usize,
    pub after: usize,
    /// Rows touched per class (a row that changed limit and cost counts in both).
    pub counts: BTreeMap<Class, usize>,
    pub out_of_scope: OutOfScope,
    pub changes: Vec<Change>,
}

impl SourceDiff {
    pub fn count(&self, class: Class) -> usize {
        self.counts.get(&class).copied().unwrap_or(0)
    }

    /// Nothing differs, in scope or out.
    pub fn is_empty(&self) -> bool {
        self.counts.values().all(|n| *n == 0) && self.out_of_scope.total() == 0
    }

    /// The same diff with at most `per_class` changes listed per class (counts stay exact).
    pub fn capped(&self, per_class: usize) -> Self {
        let mut seen: BTreeMap<Class, usize> = BTreeMap::new();
        let changes = self
            .changes
            .iter()
            .filter(|c| {
                let n = seen.entry(c.class).or_insert(0);
                *n += 1;
                *n <= per_class
            })
            .cloned()
            .collect();
        Self {
            changes,
            ..self.clone()
        }
    }
}

fn fields(row: &IndexRow) -> Map<String, Value> {
    match serde_json::to_value(row) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

fn shown(v: &Value) -> String {
    match v {
        Value::Null => "unknown".to_owned(),
        other => other.to_string(),
    }
}

/// One line describing a new or removed row.
fn describe(row: &IndexRow) -> String {
    let n = |v: Option<u64>| v.map_or("-".to_owned(), |v| v.to_string());
    let cost = match &row.cost {
        Some(c) => match (c.input, c.output) {
            (Some(i), Some(o)) => format!("{} {i}/{o}", c.status),
            _ => c.status.clone(),
        },
        None => "unknown".to_owned(),
    };
    format!(
        "ctx {} in {} out {}, cost {cost}",
        n(row.context_window),
        n(row.max_input_tokens),
        n(row.max_tokens)
    )
}

pub fn classify(source: &str, old: &SourceIndex, new: &SourceIndex) -> SourceDiff {
    let mut diff = SourceDiff {
        source: source.to_owned(),
        before: old.len(),
        after: new.len(),
        counts: BTreeMap::new(),
        out_of_scope: OutOfScope::default(),
        changes: Vec::new(),
    };
    let keys: std::collections::BTreeSet<&RowKey> =
        old.rows.keys().chain(new.rows.keys()).collect();
    for key in keys {
        match (old.rows.get(key), new.rows.get(key)) {
            (None, Some(row)) => {
                if row.out_of_scope() {
                    diff.out_of_scope.added += 1;
                } else {
                    diff.push(Class::Added, row, describe(row));
                }
            }
            (Some(row), None) => {
                if row.out_of_scope() {
                    diff.out_of_scope.removed += 1;
                } else {
                    diff.push(Class::Removed, row, describe(row));
                }
            }
            (Some(a), Some(b)) if a != b => {
                if a.out_of_scope() || b.out_of_scope() {
                    diff.out_of_scope.changed += 1;
                    continue;
                }
                let (fa, fb) = (fields(a), fields(b));
                let mut by_class: BTreeMap<Class, Vec<String>> = BTreeMap::new();
                for (name, va) in &fa {
                    let vb = fb.get(name).unwrap_or(&Value::Null);
                    if va != vb {
                        by_class
                            .entry(Class::of_field(name))
                            .or_default()
                            .push(format!("{name} {} → {}", shown(va), shown(vb)));
                    }
                }
                for (class, parts) in by_class {
                    diff.push(class, b, parts.join("; "));
                }
            }
            _ => {}
        }
    }
    diff
}

impl SourceDiff {
    fn push(&mut self, class: Class, row: &IndexRow, detail: String) {
        *self.counts.entry(class).or_insert(0) += 1;
        self.changes.push(Change {
            class,
            key: row.display_key(),
            detail,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::index::IndexCost;

    fn row(id: &str) -> IndexRow {
        IndexRow {
            provider: "p".into(),
            id: id.into(),
            auth_type: None,
            kind: Some("chat".into()),
            context_window: Some(100),
            max_input_tokens: None,
            max_tokens: Some(10),
            cost: Some(IndexCost {
                status: "known".into(),
                input: Some(1.0),
                output: Some(2.0),
            }),
            lifecycle: Some("live".into()),
            efforts: Some(vec!["low".into()]),
            input_modalities: Some(vec!["text".into()]),
        }
    }

    fn index(rows: Vec<IndexRow>) -> SourceIndex {
        let mut i = SourceIndex::default();
        for r in rows {
            i.rows
                .insert((r.provider.clone(), r.id.clone(), String::new()), r);
        }
        i
    }

    #[test]
    fn identical_indexes_have_an_empty_diff() {
        let i = index(vec![row("a"), row("b")]);
        let d = classify("s", &i, &i.clone());
        assert!(d.is_empty());
        assert_eq!((d.before, d.after), (2, 2));
    }

    #[test]
    fn every_class_is_told_apart() {
        let old = index(vec![
            row("keep"),
            row("gone"),
            row("lim"),
            row("cost"),
            row("life"),
            row("eff"),
            row("mod"),
        ]);
        let mut lim = row("lim");
        lim.context_window = Some(200);
        let mut cost = row("cost");
        cost.cost.as_mut().unwrap().input = Some(3.0);
        let mut life = row("life");
        life.lifecycle = Some("deprecated".into());
        let mut eff = row("eff");
        eff.efforts = Some(vec!["low".into(), "high".into()]);
        let mut modal = row("mod");
        modal.input_modalities = Some(vec!["text".into(), "image".into()]);
        let new = index(vec![row("keep"), row("fresh"), lim, cost, life, eff, modal]);
        let d = classify("s", &old, &new);
        for (class, n) in [
            (Class::Added, 1),
            (Class::Removed, 1),
            (Class::LimitChanged, 1),
            (Class::CostChanged, 1),
            (Class::LifecycleChanged, 1),
            (Class::EffortsChanged, 1),
            (Class::OtherChanged, 1),
        ] {
            assert_eq!(d.count(class), n, "{}", class.label());
        }
        let lim = d
            .changes
            .iter()
            .find(|c| c.class == Class::LimitChanged)
            .unwrap();
        assert_eq!(lim.key, "p/lim");
        assert_eq!(lim.detail, "contextWindow 100 → 200");
    }

    #[test]
    fn a_row_changing_two_classes_counts_in_both() {
        let old = index(vec![row("a")]);
        let mut a = row("a");
        a.max_tokens = Some(99);
        a.lifecycle = Some("retired".into());
        let d = classify("s", &old, &index(vec![a]));
        assert_eq!(d.count(Class::LimitChanged), 1);
        assert_eq!(d.count(Class::LifecycleChanged), 1);
    }

    #[test]
    fn non_chat_rows_are_counted_apart() {
        let mut img = row("img");
        img.kind = Some("image".into());
        let old = index(vec![]);
        let d = classify("s", &old, &index(vec![img.clone()]));
        assert_eq!(d.out_of_scope.added, 1);
        assert_eq!(d.count(Class::Added), 0);
        let mut img2 = img.clone();
        img2.context_window = Some(1);
        let d = classify("s", &index(vec![img]), &index(vec![img2]));
        assert_eq!(d.out_of_scope.changed, 1);
        assert!(d.changes.is_empty());
    }

    #[test]
    fn capping_keeps_counts_exact() {
        let new = index((0..30).map(|i| row(&format!("m{i:02}"))).collect());
        let d = classify("s", &index(vec![]), &new).capped(5);
        assert_eq!(d.count(Class::Added), 30);
        assert_eq!(d.changes.len(), 5);
    }
}

//! Placeholder detection: port of `detect_placeholders` / `is_placeholder` from
//! `prototype/fieldlib.py`.
//!
//! A gateway fills defaults where it does not know a limit or an effort ladder, so the same
//! `(contextWindow, maxTokens)` pair or the same ladder repeats across a large share of its rows.
//! No real catalog has that many models with identical limits, so a value that covers at least
//! [`PLACEHOLDER_SHARE`] of one source's rows is a *placeholder*: it is not evidence, and it may
//! only be kept as an alternate, never win a field.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::{Effort, FieldName, Fields};

/// A `(contextWindow, maxTokens)` pair or an effort ladder is a placeholder when it covers at
/// least this share of a source's rows (the prototype's `PLACEHOLDER_SHARE = 0.20`). Written as
/// `1 / SHARE_DENOMINATOR` so the comparison is exact integer arithmetic.
pub const PLACEHOLDER_SHARE: f64 = 0.20;
const SHARE_DENOMINATOR: usize = 5;

/// The placeholder values of one source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Placeholders {
    /// `(contextWindow, maxTokens)` pairs. A pair marks `contextWindow`, `maxTokens` and
    /// `maxInputTokens` of every row that carries it: the default is a *pair*, so one half of it
    /// that happens to look plausible is no more evidence than the other.
    pub pairs: BTreeSet<(Option<u64>, Option<u64>)>,
    /// Effort ladders (`thinking.efforts`, never empty).
    pub efforts: BTreeSet<Vec<Effort>>,
}

/// The effort ladder of a row; `None` when it has none (the prototype's `g_efforts`).
fn ladder(fields: &Fields) -> Option<&[Effort]> {
    fields
        .thinking
        .as_ref()
        .map(|t| t.efforts.as_slice())
        .filter(|e| !e.is_empty())
}

fn pair(fields: &Fields) -> (Option<u64>, Option<u64>) {
    (fields.context_window, fields.max_tokens)
}

impl Placeholders {
    /// Detects the placeholders among the rows of ONE source.
    pub fn detect<'a>(rows: impl IntoIterator<Item = &'a Fields>) -> Self {
        let mut pairs: BTreeMap<(Option<u64>, Option<u64>), usize> = BTreeMap::new();
        let mut efforts: BTreeMap<&[Effort], usize> = BTreeMap::new();
        let mut total = 0usize;
        for row in rows {
            total += 1;
            *pairs.entry(pair(row)).or_default() += 1;
            if let Some(l) = ladder(row) {
                *efforts.entry(l).or_default() += 1;
            }
        }
        let frequent = |n: usize| n * SHARE_DENOMINATOR >= total;
        Self {
            pairs: pairs
                .into_iter()
                .filter(|&(k, n)| frequent(n) && k != (None, None))
                .map(|(k, _)| k)
                .collect(),
            efforts: efforts
                .into_iter()
                .filter(|&(_, n)| frequent(n))
                .map(|(l, _)| l.to_vec())
                .collect(),
        }
    }

    /// Whether `fields`' value for `field` is one of this source's placeholders. Only limits
    /// and the effort ladder can be placeholders.
    pub fn is_placeholder(&self, fields: &Fields, field: FieldName) -> bool {
        match field {
            FieldName::ContextWindow | FieldName::MaxTokens | FieldName::MaxInputTokens => {
                self.pairs.contains(&pair(fields))
            }
            FieldName::Thinking => {
                ladder(fields).is_some_and(|l| self.efforts.iter().any(|p| p.as_slice() == l))
            }
            _ => false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty() && self.efforts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Thinking, ThinkingMode};

    fn row(ctx: Option<u64>, max: Option<u64>, efforts: &[Effort]) -> Fields {
        let mut f = Fields::empty("gw", "m");
        f.context_window = ctx;
        f.max_tokens = max;
        if !efforts.is_empty() {
            f.thinking = Some(Thinking {
                mode: ThinkingMode::Effort,
                efforts: efforts.to_vec(),
                options: None,
                default_level: None,
                effort_map: None,
            });
        }
        f
    }

    #[test]
    fn share_boundary_is_inclusive() {
        // 2 of 10 rows = exactly 20%: a placeholder. 1 of 10 is not.
        let mut rows: Vec<Fields> = (0..8)
            .map(|i| row(Some(1000 + i), Some(100 + i), &[]))
            .collect();
        rows.push(row(Some(128_000), Some(32_768), &[]));
        rows.push(row(Some(128_000), Some(32_768), &[]));
        let ph = Placeholders::detect(&rows);
        assert_eq!(
            ph.pairs,
            BTreeSet::from([(Some(128_000), Some(32_768))]),
            "20% exactly is a placeholder"
        );
        rows.pop();
        rows.push(row(Some(5), Some(6), &[]));
        assert!(Placeholders::detect(&rows).pairs.is_empty());
    }

    #[test]
    fn an_all_unknown_pair_is_never_a_placeholder() {
        let rows: Vec<Fields> = (0..10).map(|_| row(None, None, &[])).collect();
        assert!(Placeholders::detect(&rows).is_empty());
    }

    #[test]
    fn the_pair_marks_both_limits_and_max_input() {
        let rows: Vec<Fields> = (0..5)
            .map(|_| row(Some(272_000), Some(16_384), &[]))
            .collect();
        let ph = Placeholders::detect(&rows);
        for field in [
            FieldName::ContextWindow,
            FieldName::MaxTokens,
            FieldName::MaxInputTokens,
        ] {
            assert!(ph.is_placeholder(&rows[0], field));
        }
        // a row with the same context but another maxTokens is not that pair
        assert!(!ph.is_placeholder(
            &row(Some(272_000), Some(8_192), &[]),
            FieldName::ContextWindow
        ));
        assert!(!ph.is_placeholder(&rows[0], FieldName::Cost));
    }

    #[test]
    fn ladders_are_placeholders_by_share_and_rows_without_one_do_not_count_as_a_ladder() {
        let ladder = [Effort::Low, Effort::Medium, Effort::High];
        let mut rows: Vec<Fields> = (0..4).map(|_| row(None, None, &ladder)).collect();
        rows.extend((0..6).map(|i| row(Some(i + 1), Some(i + 1), &[])));
        let ph = Placeholders::detect(&rows);
        assert_eq!(ph.efforts, BTreeSet::from([ladder.to_vec()]));
        assert!(ph.is_placeholder(&rows[0], FieldName::Thinking));
        assert!(
            !ph.is_placeholder(&rows[9], FieldName::Thinking),
            "no ladder, no placeholder"
        );
        let other = row(None, None, &[Effort::Low, Effort::High]);
        assert!(!ph.is_placeholder(&other, FieldName::Thinking));
    }

    #[test]
    fn no_rows_no_placeholders() {
        assert!(Placeholders::detect(std::iter::empty()).is_empty());
    }
}

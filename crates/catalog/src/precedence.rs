//! Per-field precedence: the generic resolver behind the merge (`docs/catalog-proof.md` §9).
//!
//! Principles: (a) a gateway row defines the model; references only enrich it. (b) A value is
//! evidence only if it is not a placeholder; placeholders are kept as alternates, never win.
//! (c) First-party first, then a strict majority of resellers, never "first seen". (d) Any other
//! distinct value is kept in `alternates`; `conflict` is set when it is *evidence*, i.e. the
//! gateway's non-placeholder value or what a reference settled on (its first-party / strict
//! majority value, or every value of a pool that could not settle). Placeholders and entries
//! outvoted inside a reference are alternates only.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::Value;

use crate::model::{Alternate, Confidence, FieldProvenance, MatchTier};

/// `FieldProvenance::source` when no source supplied a value (the field is null / unknown).
pub const NO_SOURCE: &str = "none";

/// How far down the precedence chain a field was settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Configured,
    GatewayLive,
    RefFirstParty,
    RefMajority,
    /// modelparams.dev `range.max`, used for `maxTokens` only when models.dev has no value.
    RangeMax,
    /// The gateway lists the model, so it is live unless a reference degrades it.
    GatewayListed,
    Unresolved,
}

impl Level {
    pub fn name(self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::GatewayLive => "gateway-live",
            Self::RefFirstParty => "ref-first-party",
            Self::RefMajority => "ref-majority",
            Self::RangeMax => "range-max",
            Self::GatewayListed => "gateway-listed",
            Self::Unresolved => "none",
        }
    }

    pub fn confidence(self) -> Confidence {
        match self {
            Self::Configured | Self::GatewayLive | Self::RefFirstParty => Confidence::Declared,
            Self::RefMajority | Self::RangeMax | Self::GatewayListed => Confidence::Inferred,
            Self::Unresolved => Confidence::Unknown,
        }
    }
}

/// The gateway's own value for a field.
pub struct GwCand<V> {
    pub value: V,
    pub key: String,
    pub placeholder: bool,
}

/// One reference entry's value.
pub struct RefCand<V> {
    pub value: V,
    pub key: String,
    pub first_party: bool,
    pub tier: MatchTier,
}

/// What one reference source says about a field.
pub struct Stage<V> {
    pub source: String,
    pub cands: Vec<RefCand<V>>,
    /// Only first-party entries count (no strict-majority fallback among resellers).
    pub first_party_only: bool,
    /// Level to report instead of first-party / majority (`range-max`).
    pub level: Option<Level>,
    /// A settled value that is not accepted is dropped: it is not evidence (a reference's
    /// `live` lifecycle is the absence of a status, so it cannot confirm anything).
    pub accept: fn(&V) -> bool,
}

pub struct Win<V> {
    pub value: V,
    pub source: String,
    pub level: Level,
    pub tier: Option<MatchTier>,
    pub key: String,
}

/// A candidate value as it appears in provenance.
pub struct Seen {
    pub source: String,
    pub value: Value,
    pub key: String,
    pub confidence: Confidence,
    pub placeholder: bool,
}

pub struct Fallback<V> {
    pub value: V,
    pub source: String,
    pub level: Level,
    pub key: String,
}

pub struct Resolved<V> {
    pub winner: Option<Win<V>>,
    /// Values that count as evidence (they make `conflict` when they differ from the winner).
    pub evidence: Vec<Seen>,
    /// Placeholders and outvoted entries: alternates only.
    pub others: Vec<Seen>,
}

fn json<V: Serialize>(v: &V) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

struct StagePick<V> {
    win: Option<Win<V>>,
    evidence: Vec<Seen>,
    outvoted: Vec<Seen>,
}

fn seen<V: Serialize>(source: &str, c: &RefCand<V>, confidence: Confidence) -> Seen {
    Seen {
        source: source.to_owned(),
        value: json(&c.value),
        key: c.key.clone(),
        confidence,
        placeholder: false,
    }
}

/// First-party entries first, else every entry; a value wins only with a strict majority of
/// that pool (a single distinct value is trivially one).
fn pick<V: Clone + Serialize>(stage: &Stage<V>) -> StagePick<V> {
    let first_party: Vec<&RefCand<V>> = stage.cands.iter().filter(|c| c.first_party).collect();
    let basis_first_party = !first_party.is_empty();
    let pool: Vec<&RefCand<V>> = if basis_first_party {
        first_party
    } else if stage.first_party_only {
        Vec::new()
    } else {
        stage.cands.iter().collect()
    };
    let in_pool = |c: &RefCand<V>| pool.iter().any(|p| std::ptr::eq(*p, c));
    let outside = |out: &mut Vec<Seen>| {
        let mut keys = BTreeSet::new();
        for c in stage.cands.iter().filter(|c| !in_pool(c)) {
            if keys.insert(c.key.as_str()) {
                out.push(seen(&stage.source, c, Confidence::Inferred));
            }
        }
    };
    let mut pick = StagePick {
        win: None,
        evidence: Vec::new(),
        outvoted: Vec::new(),
    };
    if pool.is_empty() {
        outside(&mut pick.outvoted);
        return pick;
    }
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for c in &pool {
        *counts.entry(c.key.as_str()).or_default() += 1;
    }
    let confidence = if basis_first_party {
        Confidence::Declared
    } else {
        Confidence::Inferred
    };
    let top = counts
        .iter()
        .max_by_key(|&(k, n)| (*n, std::cmp::Reverse(*k)))
        .map(|(k, n)| (*k, *n));
    match top {
        Some((key, n)) if n * 2 > pool.len() => {
            let winner = pool
                .iter()
                .find(|c| c.key == key)
                .copied()
                .expect("the top key comes from the pool");
            if (stage.accept)(&winner.value) {
                pick.win = Some(Win {
                    value: winner.value.clone(),
                    source: stage.source.clone(),
                    level: stage.level.unwrap_or(if basis_first_party {
                        Level::RefFirstParty
                    } else {
                        Level::RefMajority
                    }),
                    tier: Some(winner.tier),
                    key: winner.key.clone(),
                });
                pick.evidence.push(seen(&stage.source, winner, confidence));
            }
            let mut keys = BTreeSet::from([key]);
            for c in pool.iter().filter(|c| keys.insert(c.key.as_str())) {
                pick.outvoted
                    .push(seen(&stage.source, c, Confidence::Inferred));
            }
        }
        _ => {
            let mut keys = BTreeSet::new();
            for c in pool.iter().filter(|c| keys.insert(c.key.as_str())) {
                pick.evidence.push(seen(&stage.source, c, confidence));
            }
        }
    }
    outside(&mut pick.outvoted);
    pick
}

/// Settles one field: the gateway's non-placeholder value, else the first reference stage that
/// settles on one.
pub fn resolve<V: Clone + Serialize>(
    gateway: Option<(&str, GwCand<V>)>,
    stages: &[Stage<V>],
) -> Resolved<V> {
    let mut r = Resolved {
        winner: None,
        evidence: Vec::new(),
        others: Vec::new(),
    };
    if let Some((source, g)) = gateway {
        let s = Seen {
            source: source.to_owned(),
            value: json(&g.value),
            key: g.key.clone(),
            confidence: if g.placeholder {
                Confidence::Unknown
            } else {
                Confidence::Declared
            },
            placeholder: g.placeholder,
        };
        if g.placeholder {
            r.others.push(s);
        } else {
            r.evidence.push(s);
            r.winner = Some(Win {
                value: g.value,
                source: source.to_owned(),
                level: Level::GatewayLive,
                tier: None,
                key: g.key,
            });
        }
    }
    for stage in stages {
        let p = pick(stage);
        if r.winner.is_none() {
            r.winner = p.win;
        }
        r.evidence.extend(p.evidence);
        r.others.extend(p.outvoted);
    }
    r
}

/// Applies a `configured` override: it wins over everything; what would have won becomes
/// evidence, so a configured value that disagrees with the sources is flagged as a conflict.
pub fn configured<V: Clone + Serialize>(
    mut r: Resolved<V>,
    value: Option<V>,
    source: &str,
    key: impl Fn(&V) -> String,
) -> Resolved<V> {
    let Some(value) = value else {
        return r;
    };
    if let Some(previous) = r.winner.take() {
        r.evidence.push(Seen {
            source: previous.source,
            value: json(&previous.value),
            key: previous.key,
            confidence: previous.level.confidence(),
            placeholder: false,
        });
    }
    r.winner = Some(Win {
        key: key(&value),
        value,
        source: source.to_owned(),
        level: Level::Configured,
        tier: None,
    });
    r
}

/// A settled field: its value, its provenance and what the report counts.
pub struct Outcome<V> {
    pub value: Option<V>,
    pub provenance: Option<FieldProvenance>,
    pub rule: String,
    pub conflict: bool,
    pub placeholder_alternates: usize,
    pub level: Level,
    pub winner_source: Option<String>,
}

/// Turns a [`Resolved`] into the winner, its provenance and the alternates. `fallback` is the
/// value when nothing was settled (`live` for lifecycle, `unknown` for cost); `confidence`
/// overrides the level's default for fields whose confidence is fixed by `§9`.
pub fn finish<V: Clone>(
    chain: &str,
    r: Resolved<V>,
    fallback: Option<Fallback<V>>,
    confidence: impl Fn(&Win<V>) -> Confidence,
) -> Outcome<V> {
    let winner = r.winner.or_else(|| {
        fallback.map(|f| Win {
            value: f.value,
            source: f.source,
            level: f.level,
            tier: None,
            key: f.key,
        })
    });
    let winner_key = winner.as_ref().map(|w| w.key.clone());
    let differs = |s: &Seen| winner_key.as_deref() != Some(s.key.as_str());
    let conflict = r.evidence.iter().any(differs);
    let mut dedup = BTreeSet::new();
    let alternates: Vec<Seen> = r
        .evidence
        .into_iter()
        .chain(r.others)
        .filter(|s| differs(s) && dedup.insert((s.source.clone(), s.key.clone())))
        .collect();
    let placeholder_alternates = alternates.iter().filter(|s| s.placeholder).count();
    let level = winner.as_ref().map_or(Level::Unresolved, |w| w.level);
    let rule = if level == Level::Configured {
        "configured".to_owned()
    } else {
        format!("{chain} [{}]", level.name())
    };
    let provenance = if winner.is_none() && alternates.is_empty() {
        None
    } else {
        let (source, confidence) = match &winner {
            Some(w) => (w.source.clone(), confidence(w)),
            None => (NO_SOURCE.to_owned(), Confidence::Unknown),
        };
        Some(FieldProvenance {
            source,
            rule: rule.clone(),
            confidence,
            conflict: Some(conflict),
            alternates: alternates
                .into_iter()
                .map(|s| Alternate {
                    source: s.source,
                    value: s.value,
                    confidence: Some(s.confidence),
                })
                .collect(),
        })
    };
    Outcome {
        winner_source: winner.as_ref().map(|w| w.source.clone()),
        value: winner.map(|w| w.value),
        provenance,
        rule,
        conflict,
        placeholder_alternates,
        level,
    }
}

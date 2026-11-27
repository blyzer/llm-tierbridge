//! Gateway-id -> reference-catalog matching. Port of `split_id`, `norm_leaf`, `RefIndex` and
//! `is_first_party` from `prototype/common.py`.
//!
//! A gateway spells model ids its own way (`gateway/claude-opus-4.6-thinking`); a reference catalog
//! (models.dev, modelparams.dev) spells them another. The first tier that hits wins:
//!
//!   * T1 exact-full: the whole lowercased id (or the reference's `provider/id`);
//!   * T2 exact-leaf: the part after the last `/`;
//!   * T3 normalized-leaf: leaf with `4.5` -> `4-5`, `_` -> `-`, `gemini2` -> `gemini-2`, `:latest`
//!     and trailing markers (`-thinking`, `-high`, `:free`, ...) removed from the *gateway* id;
//!   * T4 date-stripped: T3 plus a trailing date (`-20250929`, `-2025-09-29`, `-2507`).
//!
//! Markers are stripped from the gateway side only: a reference leaf such as `vision-medium` is
//! a product name, not an effort marker.
//!
//! Known deviation from the Python regexes: `\d` is ASCII-only here (Python's is any Unicode
//! decimal digit); no id in the committed snapshots contains another digit.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;

use crate::CatalogError;
use crate::model::{MatchTier, ModelRef, Observation};

/// Trailing markers (`_discovery.kdl`: `trailing-marker`, `billing-variant-suffix`). `-max`,
/// `-none` and `-extra-high` are deliberately absent: they name real product lanes.
const TRAILING_MARKERS: [&str; 20] = [
    "customtools",
    "cloud",
    "exacto",
    "nitro",
    "original",
    "optimized",
    "nvfp4",
    "fp8",
    "fp4",
    "bf16",
    "int8",
    "int4",
    "thinking",
    "minimal",
    "low",
    "medium",
    "high",
    "xhigh",
    "free",
    "contributor",
];
const WRAPPER_PREFIXES: [&str; 1] = ["duo-chat-"];
const SYNTHETIC_PREFIXES: [&str; 1] = ["hf:"];
/// `canonical-family-token`: attached version separator (`gemini2.5` -> `gemini-2.5`).
const FAMILY_TOKENS: [&str; 29] = [
    "claude",
    "gemini",
    "gpt",
    "grok",
    "glm",
    "qwen",
    "minimax",
    "kimi",
    "deepseek",
    "llama",
    "gemma",
    "nova",
    "mistral",
    "ministral",
    "pixtral",
    "codestral",
    "devstral",
    "magistral",
    "ernie",
    "doubao",
    "seed",
    "aion",
    "olmo",
    "molmo",
    "nemotron",
    "palmyra",
    "command",
    "codex",
    "coder",
];

pub const TIERS: [MatchTier; 4] = [
    MatchTier::ExactFull,
    MatchTier::ExactLeaf,
    MatchTier::NormalizedLeaf,
    MatchTier::DateStripped,
];

/// What matching needs to know about one catalog entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdKey {
    pub source_id: String,
    pub provider: String,
    pub id: String,
    pub canonical_id: Option<String>,
}

impl From<&Observation> for IdKey {
    fn from(o: &Observation) -> Self {
        Self {
            source_id: o.source.source_id.clone(),
            provider: o.fields.provider.clone(),
            id: o.fields.id.clone(),
            canonical_id: o.fields.canonical_id.clone(),
        }
    }
}

/// Reads the ids out of an `observations.<sourceId>.jsonl` file written by `catalog build`.
pub fn read_id_keys(jsonl: &[u8]) -> Result<Vec<IdKey>, CatalogError> {
    let text = std::str::from_utf8(jsonl)
        .map_err(|e| CatalogError::Malformed(format!("observations are not UTF-8: {e}")))?;
    let mut keys = Vec::new();
    for (n, line) in text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        let row: serde_json::Value = serde_json::from_str(line)?;
        let text = |v: Option<&serde_json::Value>| v.and_then(|v| v.as_str()).map(str::to_owned);
        let (Some(source_id), Some(provider), Some(id)) = (
            text(row.pointer("/source/sourceId")),
            text(row.get("provider")),
            text(row.get("id")),
        ) else {
            return Err(CatalogError::Malformed(format!(
                "line {}: not an observation (needs source.sourceId, provider, id)",
                n + 1
            )));
        };
        keys.push(IdKey {
            source_id,
            provider,
            id,
            canonical_id: text(row.get("canonicalId")),
        });
    }
    if keys.is_empty() {
        return Err(CatalogError::NoModels);
    }
    Ok(keys)
}

/// `-> (prefix segments, leaf)`: lowercased, synthetic and wrapper prefixes stripped.
pub fn split_id(raw: &str) -> (Vec<String>, String) {
    let mut s = raw.trim().to_lowercase();
    for p in SYNTHETIC_PREFIXES {
        if let Some(rest) = s.strip_prefix(p) {
            s = rest.to_owned();
        }
    }
    let mut parts: Vec<String> = s.split('/').map(str::to_owned).collect();
    let mut leaf = parts.pop().unwrap_or_default();
    for p in WRAPPER_PREFIXES {
        if let Some(rest) = leaf.strip_prefix(p) {
            leaf = rest.to_owned();
        }
    }
    (parts, leaf)
}

fn digits(b: &[u8]) -> bool {
    b.iter().all(u8::is_ascii_digit)
}

/// Length of a trailing `-20250929` / `-2025-09-29` / `-2507` date, longest (= leftmost) first.
fn date_suffix_len(x: &str) -> Option<usize> {
    let b = x.as_bytes();
    let tail = |n: usize| (b.len() >= n).then(|| &b[b.len() - n..]);
    if let Some(s) = tail(11)
        && s[0] == b'-'
        && &s[1..3] == b"20"
        && digits(&s[3..5])
        && s[5] == b'-'
        && digits(&s[6..8])
        && s[8] == b'-'
        && digits(&s[9..11])
    {
        return Some(11);
    }
    if let Some(s) = tail(9)
        && s[0] == b'-'
        && &s[1..3] == b"20"
        && digits(&s[3..9])
    {
        return Some(9);
    }
    if let Some(s) = tail(5)
        && s[0] == b'-'
        && s[1] == b'2'
        && (b'3'..=b'9').contains(&s[2])
        && digits(&s[3..5])
        && (1..=12).contains(&((s[3] - b'0') * 10 + (s[4] - b'0')))
    {
        return Some(5);
    }
    None
}

fn strip_marker(x: &str) -> &str {
    match x.rfind(['-', ':']) {
        Some(i) if TRAILING_MARKERS.contains(&&x[i + 1..]) => &x[..i],
        _ => x,
    }
}

/// Normalised leaf. `markers`: strip trailing markers; `strip_date`: strip a trailing date.
pub fn norm_leaf(leaf: &str, strip_date: bool, markers: bool) -> String {
    let attached = FAMILY_TOKENS.iter().find_map(|t| {
        let rest = leaf.strip_prefix(t)?;
        rest.starts_with(|c: char| c.is_ascii_digit())
            .then(|| format!("{t}-{rest}"))
    });
    let attached = attached.unwrap_or_else(|| leaf.to_owned());
    let chars: Vec<char> = attached.chars().collect();
    let mut x: String = chars
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            let between_digits = c == '.'
                && i > 0
                && chars[i - 1].is_ascii_digit()
                && chars.get(i + 1).is_some_and(char::is_ascii_digit);
            if between_digits || c == '_' { '-' } else { c }
        })
        .collect();
    if let Some(rest) = x.strip_suffix(":latest") {
        x = rest.to_owned();
    }
    loop {
        let mut y = if markers { strip_marker(&x) } else { &x };
        if strip_date && let Some(n) = date_suffix_len(y) {
            y = &y[..y.len() - n];
        }
        if y == x {
            return x;
        }
        x = y.to_owned();
    }
}

/// A reference entry is first-party when its provider is the vendor that owns the model:
/// `canonical_id`'s vendor segment is the provider, or (no canonical vendor) the vendor segment
/// of the gateway id is.
pub fn is_first_party(entry: &IdKey, gateway_id: &str) -> bool {
    if let Some(canon) = &entry.canonical_id
        && let Some((vendor, _)) = canon.split_once('/')
    {
        return vendor.to_lowercase() == entry.provider.to_lowercase();
    }
    split_id(gateway_id)
        .0
        .last()
        .is_some_and(|vendor| *vendor == entry.provider.to_lowercase())
}

/// Index of reference entries by the four tiers.
pub struct RefIndex {
    entries: Vec<IdKey>,
    by: [HashMap<String, Vec<usize>>; 4],
}

impl RefIndex {
    pub fn new(entries: &[IdKey]) -> Self {
        let mut by: [HashMap<String, Vec<usize>>; 4] = Default::default();
        for (i, e) in entries.iter().enumerate() {
            let names = std::iter::once(&e.id).chain(e.canonical_id.as_ref());
            for name in names {
                let low = name.to_lowercase();
                let (_, leaf) = split_id(&low);
                by[3]
                    .entry(norm_leaf(&leaf, true, false))
                    .or_default()
                    .push(i);
                by[2]
                    .entry(norm_leaf(&leaf, false, false))
                    .or_default()
                    .push(i);
                by[1].entry(leaf).or_default().push(i);
                by[0].entry(low).or_default().push(i);
            }
            by[0]
                .entry(format!("{}/{}", e.provider, e.id).to_lowercase())
                .or_default()
                .push(i);
        }
        Self {
            entries: entries.to_vec(),
            by,
        }
    }

    /// The first tier that hits, with the distinct entries it hit (index order).
    pub fn lookup(&self, gateway_id: &str) -> Option<(MatchTier, Vec<&IdKey>)> {
        let (tier, hits) = self.lookup_indices(gateway_id)?;
        Some((tier, hits.into_iter().map(|i| &self.entries[i]).collect()))
    }

    /// Like [`Self::lookup`], but returns positions in the slice given to [`Self::new`], so a
    /// caller can get back to whatever it built the keys from.
    pub fn lookup_indices(&self, gateway_id: &str) -> Option<(MatchTier, Vec<usize>)> {
        let mut low = gateway_id.trim().to_lowercase();
        for p in SYNTHETIC_PREFIXES {
            if let Some(rest) = low.strip_prefix(p) {
                low = rest.to_owned();
            }
        }
        let (_, leaf) = split_id(gateway_id);
        let keys = [
            low,
            leaf.clone(),
            norm_leaf(&leaf, false, true),
            norm_leaf(&leaf, true, true),
        ];
        for (t, key) in keys.iter().enumerate() {
            if let Some(hit) = self.by[t].get(key) {
                let mut seen = HashSet::new();
                let indices = hit.iter().copied().filter(|&i| seen.insert(i)).collect();
                return Some((TIERS[t], indices));
            }
        }
        None
    }
}

/// The result of matching one gateway id against one reference catalog.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IdMatch {
    pub id: String,
    pub tier: Option<MatchTier>,
    pub refs: Vec<ModelRef>,
}

/// `match(gateway_obs, reference_obs) -> per-id tier + refs`, in gateway order.
pub fn match_ids(gateway: &[IdKey], reference: &[IdKey]) -> Vec<IdMatch> {
    let index = RefIndex::new(reference);
    gateway.iter().map(|g| match_one(&index, g)).collect()
}

fn match_one(index: &RefIndex, g: &IdKey) -> IdMatch {
    let (tier, refs) = match index.lookup(&g.id) {
        Some((tier, entries)) => (
            Some(tier),
            entries
                .into_iter()
                .map(|e| ModelRef {
                    source_id: e.source_id.clone(),
                    provider: e.provider.clone(),
                    id: e.id.clone(),
                    tier,
                    first_party: Some(is_first_party(e, &g.id)),
                })
                .collect(),
        ),
        None => (None, Vec::new()),
    };
    IdMatch {
        id: g.id.clone(),
        tier,
        refs,
    }
}

/// How many gateway ids each tier caught.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct TierCounts {
    #[serde(rename = "T1-exact-full")]
    pub t1: usize,
    #[serde(rename = "T2-exact-leaf")]
    pub t2: usize,
    #[serde(rename = "T3-normalized-leaf")]
    pub t3: usize,
    #[serde(rename = "T4-date-stripped")]
    pub t4: usize,
    pub unmatched: usize,
}

impl TierCounts {
    pub fn of(matches: &[IdMatch]) -> Self {
        let mut c = Self::default();
        for m in matches {
            match m.tier {
                Some(MatchTier::ExactFull) => c.t1 += 1,
                Some(MatchTier::ExactLeaf) => c.t2 += 1,
                Some(MatchTier::NormalizedLeaf) => c.t3 += 1,
                Some(MatchTier::DateStripped) => c.t4 += 1,
                None => c.unmatched += 1,
            }
        }
        c
    }
}

/// A named reference catalog. A `probe` is shown per gateway but not counted in `either`,
/// `all`, `neither` or the union (the prototype's circularity probe, omp's own roster).
pub struct Reference<'a> {
    pub name: &'a str,
    pub keys: &'a [IdKey],
    pub probe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayMatch {
    pub source_id: String,
    pub ids: usize,
    pub per_reference: BTreeMap<String, TierCounts>,
    /// Matched by at least one counted reference.
    pub either: usize,
    /// Matched by every counted reference.
    pub all: usize,
    pub neither: usize,
    /// Matched at T1 or T2 by at least one counted reference.
    pub exact_only: usize,
}

/// Unique wire ids across all gateways.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnionMatch {
    pub unique_ids: usize,
    pub per_reference: BTreeMap<String, usize>,
    pub either: usize,
    pub neither: usize,
    pub in_more_than_one_source: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MatchReport {
    pub gateways: Vec<GatewayMatch>,
    pub union: UnionMatch,
}

/// Matches every gateway against every reference and tallies the prototype's headline figures.
pub fn match_report(gateways: &[(&str, &[IdKey])], references: &[Reference<'_>]) -> MatchReport {
    let indexes: Vec<RefIndex> = references.iter().map(|r| RefIndex::new(r.keys)).collect();
    let counted: Vec<usize> = (0..references.len())
        .filter(|&i| !references[i].probe)
        .collect();
    let tiers_of = |id: &str| -> Vec<Option<MatchTier>> {
        indexes
            .iter()
            .map(|ix| ix.lookup(id).map(|(t, _)| t))
            .collect()
    };

    let mut out = Vec::new();
    let mut union: BTreeMap<&str, usize> = BTreeMap::new();
    let mut union_hits: HashMap<&str, Vec<bool>> = HashMap::new();
    for &(source_id, keys) in gateways {
        let mut per_reference = BTreeMap::new();
        let (mut either, mut all, mut exact) = (0, 0, 0);
        for (r, ix) in references.iter().zip(&indexes) {
            let matches: Vec<IdMatch> = keys.iter().map(|g| match_one(ix, g)).collect();
            per_reference.insert(r.name.to_owned(), TierCounts::of(&matches));
        }
        for g in keys {
            let tiers = tiers_of(&g.id);
            let hit = |i: &usize| tiers[*i].is_some();
            either += usize::from(counted.iter().any(hit));
            all += usize::from(!counted.is_empty() && counted.iter().all(hit));
            exact +=
                usize::from(counted.iter().any(|&i| {
                    matches!(tiers[i], Some(MatchTier::ExactFull | MatchTier::ExactLeaf))
                }));
            *union.entry(g.id.as_str()).or_default() += 1;
            union_hits
                .entry(g.id.as_str())
                .or_insert_with(|| counted.iter().map(hit).collect());
        }
        out.push(GatewayMatch {
            source_id: source_id.to_owned(),
            ids: keys.len(),
            per_reference,
            either,
            all,
            neither: keys.len() - either,
            exact_only: exact,
        });
    }
    let mut per_ref = BTreeMap::new();
    for (slot, &i) in counted.iter().enumerate() {
        let n = union_hits.values().filter(|h| h[slot]).count();
        per_ref.insert(references[i].name.to_owned(), n);
    }
    let either = union_hits.values().filter(|h| h.iter().any(|&b| b)).count();
    MatchReport {
        gateways: out,
        union: UnionMatch {
            unique_ids: union.len(),
            per_reference: per_ref,
            either,
            neither: union.len() - either,
            in_more_than_one_source: union.values().filter(|&&n| n > 1).count(),
        },
    }
}

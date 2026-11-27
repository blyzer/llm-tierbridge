//! `sync-state.json`: what the data repo's `main` has accepted, per source, and the cadence rules
//! that decide when a source is refreshed again.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Failure, failure};

pub const STATE_FILE: &str = "sync-state.json";
const STATE_VERSION: u32 = 1;

const HOUR_MS: i64 = 3_600_000;

/// How often a source is looked at again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Cadence {
    /// Every 20 hours or more (a 06:00 job is due every day whatever the jitter).
    Daily,
    /// Every 6 days or more.
    Weekly,
    /// Only when the upstream git `HEAD` is not the revision already synced.
    OnChange,
}

impl Cadence {
    /// Minimum time between two refreshes; `None` for `OnChange`.
    fn period_ms(self) -> Option<i64> {
        match self {
            Self::Daily => Some(20 * HOUR_MS),
            Self::Weekly => Some(6 * 24 * HOUR_MS),
            Self::OnChange => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::OnChange => "on-change",
        }
    }
}

/// Why a source is refreshed this run; `None` from [`due`] means it is reused as is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DueReason {
    Never,
    Elapsed,
    Upstream,
}

impl DueReason {
    pub fn label(self) -> &'static str {
        match self {
            Self::Never => "never fetched",
            Self::Elapsed => "cadence elapsed",
            Self::Upstream => "upstream moved",
        }
    }
}

/// Decides whether a source is due. `last_fetched_ms` is when its newest snapshot was taken (`None`:
/// never); `upstream_moved` matters only for `OnChange` sources.
pub fn due(
    cadence: Cadence,
    last_fetched_ms: Option<i64>,
    now_ms: i64,
    upstream_moved: bool,
) -> Option<DueReason> {
    let Some(last) = last_fetched_ms else {
        return Some(DueReason::Never);
    };
    match cadence.period_ms() {
        Some(period) if now_ms - last >= period => Some(DueReason::Elapsed),
        Some(_) => None,
        None => upstream_moved.then_some(DueReason::Upstream),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceState {
    /// etag, git commit or `sha256:<16 hex>` of the snapshot.
    pub revision: String,
    pub sha256: String,
    pub fetched_at: String,
    /// Observations in the snapshot: what the shrink guard compares against.
    pub count: usize,
    pub cadence: Cadence,
    pub last_synced_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncState {
    pub version: u32,
    pub sources: BTreeMap<String, SourceState>,
}

impl Default for SyncState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            sources: BTreeMap::new(),
        }
    }
}

impl SyncState {
    pub fn parse(bytes: &[u8]) -> Result<Self, Failure> {
        let state: Self =
            serde_json::from_slice(bytes).map_err(|e| failure(format!("{STATE_FILE}: {e}")))?;
        if state.version != STATE_VERSION {
            return Err(failure(format!(
                "{STATE_FILE}: version {} is not supported (expected {STATE_VERSION})",
                state.version
            )));
        }
        Ok(state)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = serde_json::to_vec_pretty(self).expect("state serializes");
        bytes.push(b'\n');
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000;

    #[test]
    fn never_fetched_is_always_due() {
        for c in [Cadence::Daily, Cadence::Weekly, Cadence::OnChange] {
            assert_eq!(due(c, None, NOW, false), Some(DueReason::Never));
        }
    }

    #[test]
    fn daily_is_due_after_twenty_hours_not_before() {
        assert_eq!(
            due(Cadence::Daily, Some(NOW - 19 * HOUR_MS), NOW, false),
            None
        );
        assert_eq!(
            due(Cadence::Daily, Some(NOW - 20 * HOUR_MS), NOW, false),
            Some(DueReason::Elapsed)
        );
    }

    #[test]
    fn weekly_waits_six_days() {
        assert_eq!(
            due(Cadence::Weekly, Some(NOW - 5 * 24 * HOUR_MS), NOW, false),
            None
        );
        assert_eq!(
            due(Cadence::Weekly, Some(NOW - 6 * 24 * HOUR_MS), NOW, false),
            Some(DueReason::Elapsed)
        );
    }

    #[test]
    fn on_change_ignores_the_clock() {
        let long_ago = Some(NOW - 400 * 24 * HOUR_MS);
        assert_eq!(due(Cadence::OnChange, long_ago, NOW, false), None);
        assert_eq!(
            due(Cadence::OnChange, long_ago, NOW, true),
            Some(DueReason::Upstream)
        );
    }

    #[test]
    fn state_round_trips_and_rejects_other_versions() {
        let mut state = SyncState::default();
        state.sources.insert(
            "models.dev".into(),
            SourceState {
                revision: "etag:abc".into(),
                sha256: "0".repeat(64),
                fetched_at: "2026-11-27T06:00:00Z".into(),
                count: 3,
                cadence: Cadence::Daily,
                last_synced_at: "2026-11-27T06:01:00Z".into(),
            },
        );
        let bytes = state.to_bytes();
        assert_eq!(SyncState::parse(&bytes).unwrap(), state);
        let other = String::from_utf8(bytes)
            .unwrap()
            .replace("\"version\": 1", "\"version\": 9");
        assert!(SyncState::parse(other.as_bytes()).is_err());
    }
}

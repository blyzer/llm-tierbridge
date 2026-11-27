//! Value-level findings. Nothing outside the contract is dropped silently: it is either a
//! rejection (value removed, run continues) or a documented sentinel (value mapped).

use serde::Serialize;
use serde_json::Value;

/// A value the source supplied that the contract cannot hold; it was removed from the record.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub source_id: String,
    pub provider: String,
    pub id: String,
    pub field: String,
    pub value: Value,
    pub reason: String,
}

/// A documented sentinel that was mapped to a contract value (not a rejection).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SentinelMapping {
    pub source_id: String,
    pub provider: String,
    pub id: String,
    pub field: String,
    pub value: Value,
    pub mapped_to: String,
    pub reason: String,
}

/// Collects findings while one source is adapted.
#[derive(Debug, Default)]
pub struct Findings {
    pub rejections: Vec<Diagnostic>,
    pub sentinels: Vec<SentinelMapping>,
    /// Values the source supplied that the contract has no field for (e.g. models.dev
    /// `cost.reasoning`): they are not in the record, and not silently: one entry per value.
    pub unmodelled: Vec<Diagnostic>,
    /// Limits that were exactly `0` (also recorded one by one in `sentinels`).
    pub zero_limits: usize,
}

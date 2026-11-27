//! Snapshot envelope and per-row reporting shared by the sources that are read from a git
//! checkout (router-for-me/models, dsh, freellmapi, OmniRoute).
//!
//! The CLI reads one file per `--input`, and these projects publish several files (or TypeScript).
//! A committed script under `tools/` therefore reads the checkout at a pinned commit and writes one
//! JSON envelope:
//!
//! ```json
//! { "format": "tierbridge-snapshot/1", "sourceId": "dsh", "repository": "https://github.com/...",
//!   "commit": "<40 hex>", "commitDate": "2026-11-26T07:34:11Z", "...": "source specific payload" }
//! ```
//!
//! The adapter validates the header here, then reads its own payload keys.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use crate::CatalogError;
use crate::diagnostics::{Diagnostic, Findings};
use crate::model::{Confidence, Effort, Modality, ProvenanceKind, ProvenanceSource};
use crate::time::parse_rfc3339_ms;

pub const FORMAT: &str = "tierbridge-snapshot/1";

pub fn malformed(what: impl Into<String>) -> CatalogError {
    CatalogError::Malformed(what.into())
}

/// A short rendering of a value for error messages.
fn describe(value: Option<&Value>) -> String {
    match value {
        None => "nothing".to_owned(),
        Some(v) => {
            let text = v.to_string();
            if text.chars().count() > 60 {
                format!("{}…", text.chars().take(60).collect::<String>())
            } else {
                text
            }
        }
    }
}

/// `value` as an object, or a structural error naming `what`.
pub fn expect_object<'a>(
    value: Option<&'a Value>,
    what: &str,
) -> Result<&'a Map<String, Value>, CatalogError> {
    match value {
        Some(Value::Object(o)) => Ok(o),
        other => Err(malformed(format!(
            "{what} is missing or not an object (found {})",
            describe(other)
        ))),
    }
}

/// `value` as an array, or a structural error naming `what`.
pub fn expect_array<'a>(
    value: Option<&'a Value>,
    what: &str,
) -> Result<&'a Vec<Value>, CatalogError> {
    match value {
        Some(Value::Array(a)) => Ok(a),
        other => Err(malformed(format!(
            "{what} is missing or not an array (found {})",
            describe(other)
        ))),
    }
}

/// A non-empty string member, or a structural error naming `what`.
pub fn required_str<'a>(
    obj: &'a Map<String, Value>,
    key: &str,
    what: &str,
) -> Result<&'a str, CatalogError> {
    match obj.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Ok(s),
        other => Err(malformed(format!(
            "{what}: `{key}` is missing or not a non-empty string (found {})",
            describe(other)
        ))),
    }
}

/// The validated header of a snapshot plus its whole top-level object.
#[derive(Debug)]
pub struct Envelope {
    pub repository: String,
    /// Full lowercase hex commit sha.
    pub commit: String,
    /// RFC 3339, UTC.
    pub commit_date: String,
    pub root: Map<String, Value>,
}

impl Envelope {
    /// Parses `bytes` and checks the header; `source_id` must match the envelope's `sourceId`.
    pub fn parse(bytes: &[u8], source_id: &str) -> Result<Self, CatalogError> {
        let Value::Object(root) = serde_json::from_slice::<Value>(bytes)? else {
            return Err(malformed("snapshot is not a JSON object"));
        };
        match root.get("format") {
            Some(Value::String(f)) if f == FORMAT => {}
            other => {
                return Err(malformed(format!(
                    "`format` must be \"{FORMAT}\" (found {})",
                    describe(other)
                )));
            }
        }
        match root.get("sourceId") {
            Some(Value::String(s)) if s == source_id => {}
            other => {
                return Err(malformed(format!(
                    "`sourceId` must be \"{source_id}\" (found {})",
                    describe(other)
                )));
            }
        }
        let repository = required_str(&root, "repository", "snapshot header")?.to_owned();
        let commit = required_str(&root, "commit", "snapshot header")?.to_owned();
        if commit.len() != 40
            || !commit
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(malformed(format!(
                "`commit` must be 40 lowercase hex digits (found {commit})"
            )));
        }
        let commit_date = required_str(&root, "commitDate", "snapshot header")?.to_owned();
        parse_rfc3339_ms(&commit_date)?;
        Ok(Self {
            repository,
            commit,
            commit_date,
            root,
        })
    }

    /// `commit sha + date`, the revision the descriptor carries.
    pub fn revision(&self) -> String {
        format!("{}@{}", self.commit, self.commit_date)
    }

    /// The source descriptor. `note` (may be empty) is appended to `origin`, e.g. a licence caveat.
    pub fn descriptor(
        &self,
        source_id: &str,
        confidence: Confidence,
        note: &str,
        fetched_at_ms: i64,
    ) -> ProvenanceSource {
        let mut origin = format!("{}#commit={}", self.repository, &self.commit[..12]);
        if !note.is_empty() {
            origin.push_str(&format!(" ({note})"));
        }
        ProvenanceSource {
            source_id: source_id.to_owned(),
            kind: ProvenanceKind::Bundled,
            origin,
            revision: Some(self.revision()),
            confidence,
            observed_at_ms: Some(fetched_at_ms),
        }
    }
}

/// Per-row reporting context: every out-of-contract value becomes a [`Diagnostic`].
pub struct Cx<'a> {
    pub source_id: &'a str,
    pub provider: &'a str,
    pub id: &'a str,
    pub findings: &'a mut Findings,
}

impl Cx<'_> {
    pub fn reject(&mut self, field: &str, value: &Value, reason: &str) {
        self.findings.rejections.push(Diagnostic {
            source_id: self.source_id.to_owned(),
            provider: self.provider.to_owned(),
            id: self.id.to_owned(),
            field: field.to_owned(),
            value: value.clone(),
            reason: reason.to_owned(),
        });
    }

    /// `null`/absent -> `None`; a string -> `Some`; anything else is rejected.
    pub fn string(&mut self, obj: &Map<String, Value>, key: &str, field: &str) -> Option<String> {
        match obj.get(key)? {
            Value::Null => None,
            Value::String(s) => Some(s.clone()),
            other => {
                self.reject(field, other, "expected a string");
                None
            }
        }
    }

    pub fn boolean(&mut self, obj: &Map<String, Value>, key: &str, field: &str) -> Option<bool> {
        match obj.get(key)? {
            Value::Null => None,
            Value::Bool(b) => Some(*b),
            other => {
                self.reject(field, other, "expected a boolean");
                None
            }
        }
    }

    /// Token limit: a positive integer. `0`, negatives, fractions and non-numbers are rejected
    /// (none of these sources documents a meaning for `0`).
    pub fn token_limit(&mut self, obj: &Map<String, Value>, key: &str, field: &str) -> Option<u64> {
        let value = obj.get(key)?;
        match value {
            Value::Null => None,
            Value::Number(n) => match n.as_u64() {
                Some(v) if v > 0 => Some(v),
                Some(_) => {
                    self.reject(
                        field,
                        value,
                        "limit is zero and the source documents no meaning for it",
                    );
                    None
                }
                None => {
                    let reason = if n.as_i64().is_some_and(|v| v < 0) {
                        "negative limit"
                    } else {
                        "limit is not a positive integer"
                    };
                    self.reject(field, value, reason);
                    None
                }
            },
            other => {
                self.reject(field, other, "limit is not a number");
                None
            }
        }
    }

    /// Budget bound: a non-negative integer.
    pub fn non_negative(&mut self, value: Option<&Value>, field: &str) -> Option<u64> {
        let value = value?;
        match value {
            Value::Null => None,
            Value::Number(n) => {
                let parsed = n.as_u64();
                if parsed.is_none() {
                    self.reject(field, value, "not a non-negative integer");
                }
                parsed
            }
            other => {
                self.reject(field, other, "not a number");
                None
            }
        }
    }

    /// An array of modality names; names outside the contract enum are rejected one by one.
    pub fn modalities(
        &mut self,
        obj: &Map<String, Value>,
        key: &str,
        field: &str,
    ) -> Option<Vec<Modality>> {
        let items = match obj.get(key)? {
            Value::Array(a) => a,
            Value::Null => return None,
            other => {
                self.reject(field, other, "expected an array");
                return None;
            }
        };
        let mut set = BTreeSet::new();
        for item in items {
            match item.as_str().and_then(Modality::parse) {
                Some(m) => {
                    set.insert(m);
                }
                None => self.reject(field, item, "modality outside the contract enum"),
            }
        }
        (!set.is_empty()).then(|| set.into_iter().collect())
    }

    /// An array of effort names, de-duplicated in contract order; names outside the contract enum
    /// (`ultra`, `default`, ...) are rejected one by one.
    pub fn efforts(&mut self, value: &Value, field: &str) -> Vec<Effort> {
        let Value::Array(items) = value else {
            self.reject(field, value, "expected an array");
            return Vec::new();
        };
        let mut set = BTreeSet::new();
        for item in items {
            match item.as_str().and_then(Effort::parse) {
                Some(e) => {
                    set.insert(e);
                }
                None => self.reject(field, item, "effort outside the contract enum"),
            }
        }
        set.into_iter().collect()
    }
}

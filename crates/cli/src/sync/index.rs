//! `index/<sourceId>.jsonl`: the compact, sorted, reviewable view of what one source says, one line
//! per observation key. It is the surface `git diff` shows and the base the classified diff reads.
//! No timestamps, so an unchanged source gives a byte-identical file.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tierbridge_catalog::model::Observation;

use crate::{Failure, failure};

pub const INDEX_DIR: &str = "index";

/// `index/<sourceId>.jsonl` (the `:` of `omp-live:<p>` becomes `_`).
pub fn index_path(source_id: &str) -> String {
    format!("{INDEX_DIR}/{}.jsonl", source_id.replace(':', "_"))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexCost {
    pub status: String,
    pub input: Option<f64>,
    pub output: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexRow {
    pub provider: String,
    pub id: String,
    pub auth_type: Option<String>,
    /// Needed to tell chat models from the out-of-scope ones (image, embedding, ...).
    pub kind: Option<String>,
    pub context_window: Option<u64>,
    pub max_input_tokens: Option<u64>,
    pub max_tokens: Option<u64>,
    pub cost: Option<IndexCost>,
    pub lifecycle: Option<String>,
    pub efforts: Option<Vec<String>>,
    pub input_modalities: Option<Vec<String>>,
}

/// The spelling an enum has on the wire (`"api_key"`, `"live"`, ...).
fn wire<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

impl IndexRow {
    pub fn from_observation(o: &Observation) -> Self {
        let f = &o.fields;
        Self {
            provider: f.provider.clone(),
            id: f.id.clone(),
            auth_type: f.auth_type.as_ref().map(wire),
            kind: f.kind.as_ref().map(wire),
            context_window: f.context_window,
            max_input_tokens: f.max_input_tokens,
            max_tokens: f.max_tokens,
            cost: f.cost.as_ref().map(|c| IndexCost {
                status: wire(&c.status),
                input: c.input,
                output: c.output,
            }),
            lifecycle: f.lifecycle.as_ref().map(|l| wire(&l.status)),
            efforts: f
                .thinking
                .as_ref()
                .map(|t| t.efforts.iter().map(wire).collect()),
            input_modalities: f.input.as_ref().map(|m| m.iter().map(wire).collect()),
        }
    }

    /// `provider/id`, plus ` [authType]` when the source says one.
    pub fn display_key(&self) -> String {
        match &self.auth_type {
            Some(a) => format!("{}/{} [{a}]", self.provider, self.id),
            None => format!("{}/{}", self.provider, self.id),
        }
    }

    /// A model that is not a chat model is out of scope for omp (listed apart in the diff).
    pub fn out_of_scope(&self) -> bool {
        self.kind.as_deref().is_some_and(|k| k != "chat")
    }
}

/// Sort key of a row: provider, id, authType.
pub type RowKey = (String, String, String);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourceIndex {
    pub rows: BTreeMap<RowKey, IndexRow>,
    /// Observations whose key repeated one already indexed (the first wins).
    pub duplicates: usize,
}

impl SourceIndex {
    pub fn from_observations(observations: &[Observation]) -> Self {
        let mut index = Self::default();
        for o in observations {
            let row = IndexRow::from_observation(o);
            let key = (
                row.provider.clone(),
                row.id.clone(),
                row.auth_type.clone().unwrap_or_default(),
            );
            if index.rows.contains_key(&key) {
                index.duplicates += 1;
            } else {
                index.rows.insert(key, row);
            }
        }
        index
    }

    pub fn parse(bytes: &[u8], path: &str) -> Result<Self, Failure> {
        let text = std::str::from_utf8(bytes).map_err(|_| failure(format!("{path}: not UTF-8")))?;
        let mut index = Self::default();
        for (n, line) in text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
        {
            let row: IndexRow = serde_json::from_str(line)
                .map_err(|e| failure(format!("{path}:{}: {e}", n + 1)))?;
            let key = (
                row.provider.clone(),
                row.id.clone(),
                row.auth_type.clone().unwrap_or_default(),
            );
            index.rows.insert(key, row);
        }
        Ok(index)
    }

    pub fn to_jsonl(&self) -> String {
        let mut out = String::new();
        for row in self.rows.values() {
            out.push_str(&serde_json::to_string(row).expect("index row serializes"));
            out.push('\n');
        }
        out
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(provider: &str, id: &str, auth: Option<&str>, ctx: Option<u64>) -> IndexRow {
        IndexRow {
            provider: provider.into(),
            id: id.into(),
            auth_type: auth.map(str::to_owned),
            kind: Some("chat".into()),
            context_window: ctx,
            max_input_tokens: None,
            max_tokens: Some(4096),
            cost: Some(IndexCost {
                status: "known".into(),
                input: Some(1.5),
                output: Some(6.0),
            }),
            lifecycle: Some("live".into()),
            efforts: Some(vec!["low".into(), "high".into()]),
            input_modalities: Some(vec!["text".into()]),
        }
    }

    fn index(rows: Vec<IndexRow>) -> SourceIndex {
        let mut i = SourceIndex::default();
        for r in rows {
            let key = (
                r.provider.clone(),
                r.id.clone(),
                r.auth_type.clone().unwrap_or_default(),
            );
            i.rows.insert(key, r);
        }
        i
    }

    #[test]
    fn jsonl_is_sorted_and_round_trips() {
        let i = index(vec![
            row("z", "b", None, Some(10)),
            row("a", "m", Some("subscription"), None),
            row("a", "m", Some("api_key"), Some(5)),
        ]);
        let text = i.to_jsonl();
        let order: Vec<&str> = text.lines().map(|l| l.split('"').nth(3).unwrap()).collect();
        assert_eq!(order, ["a", "a", "z"]);
        assert!(
            text.lines()
                .nth(0)
                .unwrap()
                .contains("\"authType\":\"api_key\"")
        );
        assert_eq!(SourceIndex::parse(text.as_bytes(), "t").unwrap(), i);
    }

    #[test]
    fn path_replaces_the_colon_of_gateway_ids() {
        assert_eq!(index_path("omp-live:dsh"), "index/omp-live_dsh.jsonl");
        assert_eq!(index_path("models.dev"), "index/models.dev.jsonl");
    }

    #[test]
    fn non_chat_rows_are_out_of_scope() {
        let mut r = row("a", "img", None, None);
        assert!(!r.out_of_scope());
        r.kind = Some("image".into());
        assert!(r.out_of_scope());
        r.kind = None;
        assert!(!r.out_of_scope());
    }

    #[test]
    fn a_corrupt_line_is_an_error_naming_the_line() {
        let e = SourceIndex::parse(b"{\"provider\":1}\n", "index/x.jsonl").unwrap_err();
        assert!(e.message.starts_with("index/x.jsonl:1:"), "{}", e.message);
    }
}

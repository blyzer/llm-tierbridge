//! `--overrides overrides.json`: the `configured` precedence level. Minimal on purpose; the
//! full overrides workflow is slice 4.
//!
//! Shape: `{ "<model key>": { <any of the shared fields> } }`, with the key spelled exactly as
//! in `catalog.json` (`<gatewaySourceId>:<provider>/<id>`). A field present in an entry wins over
//! every source with the rule `configured`; what would have won is kept as an alternate.
//! `provider`, `id` and `authType` are identity and cannot be overridden (unknown field =
//! structural error, so a typo never passes silently).

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::CatalogError;
use crate::model::{
    AcceptedParam, Confidence, Cost, Lifecycle, Modality, ModelKind, ProvenanceKind,
    ProvenanceSource, Thinking,
};
use crate::models_dev::sha256_hex;

/// `sourceId` of the overrides in `provenance.sources` and as the winner of a `configured` field.
pub const OVERRIDES_SOURCE_ID: &str = "overrides";

/// The fields an override may set (`null` is the same as absent).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OverrideFields {
    pub name: Option<String>,
    pub canonical_id: Option<String>,
    pub kind: Option<ModelKind>,
    pub context_window: Option<u64>,
    pub max_input_tokens: Option<u64>,
    pub max_tokens: Option<u64>,
    pub cost: Option<Cost>,
    pub input: Option<Vec<Modality>>,
    pub output: Option<Vec<Modality>>,
    pub reasoning: Option<bool>,
    pub thinking: Option<Thinking>,
    pub supports_tools: Option<bool>,
    pub lifecycle: Option<Lifecycle>,
    pub accepted_params: Option<Vec<AcceptedParam>>,
}

impl OverrideFields {
    /// Whether the entry sets anything.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone)]
pub struct Overrides {
    origin: String,
    sha256: String,
    entries: BTreeMap<String, OverrideFields>,
}

fn bad(key: &str, what: impl std::fmt::Display) -> CatalogError {
    CatalogError::Malformed(format!("overrides `{key}`: {what}"))
}

fn price(key: &str, field: &str, v: Option<f64>) -> Result<(), CatalogError> {
    match v {
        Some(p) if !p.is_finite() || p < 0.0 => {
            Err(bad(key, format!("{field} must be a number >= 0")))
        }
        _ => Ok(()),
    }
}

fn validate(key: &str, o: &mut OverrideFields) -> Result<(), CatalogError> {
    for (field, v) in [
        ("contextWindow", o.context_window),
        ("maxInputTokens", o.max_input_tokens),
        ("maxTokens", o.max_tokens),
    ] {
        if v == Some(0) {
            return Err(bad(key, format!("{field} must be >= 1")));
        }
    }
    if let Some(c) = &o.cost {
        for (field, v) in [
            ("cost.input", c.input),
            ("cost.output", c.output),
            ("cost.cacheRead", c.cache_read),
            ("cost.cacheWrite", c.cache_write),
        ] {
            price(key, field, v)?;
        }
        for t in &c.tiers {
            for v in [t.input, t.output, t.cache_read, t.cache_write] {
                price(key, "cost.tiers", v)?;
            }
        }
    }
    if let Some(t) = &mut o.thinking {
        t.efforts.sort();
        if t.efforts.windows(2).any(|w| w[0] == w[1]) {
            return Err(bad(key, "thinking.efforts must not repeat a level"));
        }
    }
    Ok(())
}

impl Overrides {
    /// Parses an overrides file. `origin` names it in provenance (its path).
    pub fn parse(bytes: &[u8], origin: &str) -> Result<Self, CatalogError> {
        let mut entries: BTreeMap<String, OverrideFields> = serde_json::from_slice(bytes)?;
        for (key, fields) in &mut entries {
            validate(key, fields)?;
        }
        Ok(Self {
            origin: origin.to_owned(),
            sha256: sha256_hex(bytes),
            entries,
        })
    }

    pub fn get(&self, key: &str) -> Option<&OverrideFields> {
        self.entries.get(key).filter(|f| !f.is_empty())
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// The overrides as a `provenance.sources[]` entry.
    pub fn descriptor(&self) -> ProvenanceSource {
        ProvenanceSource {
            source_id: OVERRIDES_SOURCE_ID.to_owned(),
            kind: ProvenanceKind::Configured,
            origin: format!("{}#sha256={}", self.origin, &self.sha256[..16]),
            revision: None,
            confidence: Confidence::Declared,
            observed_at_ms: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Result<Overrides, CatalogError> {
        Overrides::parse(json.as_bytes(), "overrides.json")
    }

    #[test]
    fn reads_partial_fields_and_ignores_empty_entries() {
        let o = parse(
            r#"{"gw:gw/a": {"contextWindow": 1000, "cost": {"status": "known", "input": 1.5, "output": 3},
                             "thinking": {"mode": "effort", "efforts": ["high", "low"], "canDisable": true}},
               "gw:gw/b": {}}"#,
        )
        .unwrap();
        assert_eq!(o.len(), 2);
        let a = o.get("gw:gw/a").unwrap();
        assert_eq!(a.context_window, Some(1000));
        let t = a.thinking.as_ref().unwrap();
        assert_eq!(t.efforts.len(), 2);
        assert!(
            t.efforts[0] < t.efforts[1],
            "efforts are put in the stable order"
        );
        assert_eq!(t.options.as_ref().unwrap().can_disable, Some(true));
        assert!(
            o.get("gw:gw/b").is_none(),
            "an empty entry overrides nothing"
        );
    }

    #[test]
    fn a_typo_or_an_identity_field_is_a_structural_error() {
        assert!(parse(r#"{"k": {"contextWindo": 1}}"#).is_err());
        assert!(parse(r#"{"k": {"provider": "x"}}"#).is_err());
        assert!(parse(r#"{"k": {"authType": "api_key"}}"#).is_err());
    }

    #[test]
    fn out_of_contract_values_are_rejected_with_the_key() {
        let e = parse(r#"{"k": {"contextWindow": 0}}"#)
            .unwrap_err()
            .to_string();
        assert!(e.contains("`k`") && e.contains("contextWindow"), "{e}");
        assert!(parse(r#"{"k": {"cost": {"status": "known", "input": -1}}}"#).is_err());
        assert!(
            parse(r#"{"k": {"thinking": {"mode": "effort", "efforts": ["low", "low"]}}}"#).is_err()
        );
        assert!(
            parse(r#"{"k": {"thinking": {"mode": "effort", "efforts": ["default"]}}}"#).is_err()
        );
        assert!(parse("[1]").is_err());
    }
}

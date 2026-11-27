//! Observations -> served `model[]`. With one source there is nothing to arbitrate, so every
//! non-null field is attributed to that source under the rule `single-source`.

use std::collections::BTreeMap;

use crate::model::{FieldName, FieldProvenance, Model, ModelProvenance, Observation, RecordType};

/// Rule id recorded in [`FieldProvenance::rule`] when only one source reported the model.
pub const SINGLE_SOURCE_RULE: &str = "single-source";

/// One [`Model`] per observation, `key = provider/id`.
pub fn single_source(observation: &Observation) -> Model {
    let f = &observation.fields;
    let present = [
        (FieldName::Name, f.name.is_some()),
        (FieldName::CanonicalId, f.canonical_id.is_some()),
        (FieldName::ContextWindow, f.context_window.is_some()),
        (FieldName::MaxInputTokens, f.max_input_tokens.is_some()),
        (FieldName::MaxTokens, f.max_tokens.is_some()),
        (FieldName::Cost, f.cost.is_some()),
        (FieldName::Input, f.input.is_some()),
        (FieldName::Output, f.output.is_some()),
        (FieldName::Reasoning, f.reasoning.is_some()),
        (FieldName::Thinking, f.thinking.is_some()),
        (FieldName::SupportsTools, f.supports_tools.is_some()),
        (FieldName::Lifecycle, f.lifecycle.is_some()),
        (FieldName::AcceptedParams, f.accepted_params.is_some()),
    ];
    let fields: BTreeMap<FieldName, FieldProvenance> = present
        .into_iter()
        .filter(|&(_, set)| set)
        .map(|(name, _)| {
            (
                name,
                FieldProvenance {
                    source: observation.source.source_id.clone(),
                    rule: SINGLE_SOURCE_RULE.to_owned(),
                    confidence: observation.source.confidence,
                    conflict: None,
                    alternates: Vec::new(),
                },
            )
        })
        .collect();
    Model {
        record_type: RecordType::Model,
        key: format!("{}/{}", f.provider, f.id),
        fields: f.clone(),
        references: Vec::new(),
        provenance: ModelProvenance {
            sources: vec![observation.source.clone()],
            fields,
        },
    }
}

/// [`single_source`] over a slice, preserving order.
pub fn single_source_all(observations: &[Observation]) -> Vec<Model> {
    observations.iter().map(single_source).collect()
}

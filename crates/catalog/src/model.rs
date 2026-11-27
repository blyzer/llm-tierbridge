//! Rust mirror of `schema/gateway-model.schema.json`: one vocabulary, two record shapes
//! ([`Observation`]: what one source says; [`Model`]: the served record with provenance).
//!
//! Serialization is part of the contract. `Option` fields that the schema calls "unknown = null"
//! are always written (as `null`); optional-by-absence fields are skipped when empty.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordType {
    Observation,
    Model,
}

/// crate `ProvenanceKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProvenanceKind {
    Bundled,
    Discovered,
    Configured,
}

/// crate `EvidenceConfidence`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Verified,
    Declared,
    Inferred,
    Unknown,
}

/// crate `SourceModality`. Variants are alphabetical so `Ord` equals sorting by wire name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modality {
    Audio,
    Image,
    Pdf,
    Text,
    Video,
}

impl Modality {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "audio" => Self::Audio,
            "image" => Self::Image,
            "pdf" => Self::Pdf,
            "text" => Self::Text,
            "video" => Self::Video,
            _ => return None,
        })
    }
}

/// crate `ReasoningEffort`. Declaration order is the stable output order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    /// Maps a source spelling to the enum; `None` when it is outside the contract
    /// (e.g. `default`, `ultra`). `none` is the source spelling of `off`.
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value.to_lowercase().as_str() {
            "off" | "none" => Self::Off,
            "minimal" => Self::Minimal,
            "low" => Self::Low,
            "medium" => Self::Medium,
            "high" => Self::High,
            "xhigh" | "x_high" | "extra-high" | "extra_high" => Self::Xhigh,
            "max" => Self::Max,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelKind {
    Chat,
    Embedding,
    Image,
    Video,
    Rerank,
    Stt,
    Tts,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthType {
    ApiKey,
    Subscription,
    Oauth,
    None,
}

impl AuthType {
    /// Maps a source spelling to the enum; `None` when it is outside the contract.
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "api_key" => Self::ApiKey,
            "subscription" => Self::Subscription,
            "oauth" => Self::Oauth,
            "none" => Self::None,
            _ => return None,
        })
    }
}

/// Who says what, and how far to trust it (schema `ProvenanceSource`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceSource {
    pub source_id: String,
    pub kind: ProvenanceKind,
    pub origin: String,
    pub revision: Option<String>,
    pub confidence: Confidence,
    pub observed_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CostStatus {
    Known,
    Free,
    Unknown,
}

/// Replacement prices above a prompt-token threshold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceTier {
    pub prompt_tokens_above: u64,
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
}

/// USD per million tokens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cost {
    pub status: CostStatus,
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<PriceTier>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingMode {
    None,
    Toggle,
    Effort,
    Budget,
    Unknown,
}

/// Budget/disable facts, present only when the source actually describes reasoning options.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingOptions {
    pub can_disable: Option<bool>,
    pub min_budget_tokens: Option<u64>,
    pub max_budget_tokens: Option<u64>,
    /// models.dev `budget_tokens.min == -1`: the provider picks the budget.
    pub dynamic_budget: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Thinking {
    pub mode: ThinkingMode,
    pub efforts: Vec<Effort>,
    #[serde(flatten)]
    pub options: Option<ThinkingOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_level: Option<Effort>,
    /// effort -> native spelling; `None` = level not accepted by the route.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort_map: Option<BTreeMap<Effort, Option<String>>>,
}

/// Reads the wire shape back: the four budget/disable keys are flattened, so they make
/// `options` `Some` as soon as any of them is present.
impl<'de> Deserialize<'de> for Thinking {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Wire {
            mode: ThinkingMode,
            #[serde(default)]
            efforts: Vec<Effort>,
            can_disable: Option<bool>,
            min_budget_tokens: Option<u64>,
            max_budget_tokens: Option<u64>,
            dynamic_budget: Option<bool>,
            default_level: Option<Effort>,
            effort_map: Option<BTreeMap<Effort, Option<String>>>,
        }
        let w = Wire::deserialize(deserializer)?;
        let has_options = w.can_disable.is_some()
            || w.min_budget_tokens.is_some()
            || w.max_budget_tokens.is_some()
            || w.dynamic_budget.is_some();
        Ok(Self {
            mode: w.mode,
            efforts: w.efforts,
            options: has_options.then_some(ThinkingOptions {
                can_disable: w.can_disable,
                min_budget_tokens: w.min_budget_tokens,
                max_budget_tokens: w.max_budget_tokens,
                dynamic_budget: w.dynamic_budget,
            }),
            default_level: w.default_level,
            effort_map: w.effort_map,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LifecycleStatus {
    Live,
    Preview,
    Deprecated,
    Retired,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lifecycle {
    pub status: LifecycleStatus,
    pub replacement: Option<String>,
    pub shutdown_on: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamType {
    Integer,
    Number,
    Boolean,
    String,
    Enum,
    Array,
    Object,
}

impl ParamType {
    /// Maps a source spelling to the enum; `None` when it is outside the contract.
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "integer" => Self::Integer,
            "number" => Self::Number,
            "boolean" => Self::Boolean,
            "string" => Self::String,
            "enum" => Self::Enum,
            "array" => Self::Array,
            "object" => Self::Object,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamRange {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
}

/// A request-body knob the model accepts (modelparams.dev `params[]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcceptedParam {
    pub path: String,
    #[serde(rename = "type")]
    pub param_type: ParamType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<ParamRange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conditional: Option<bool>,
}

/// Fields shared by observations and models (schema `SharedFields`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Fields {
    pub provider: String,
    pub id: String,
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
    pub auth_type: Option<AuthType>,
}

impl Fields {
    /// All-unknown fields for `provider`/`id`.
    pub fn empty(provider: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            id: id.into(),
            name: None,
            canonical_id: None,
            kind: None,
            context_window: None,
            max_input_tokens: None,
            max_tokens: None,
            cost: None,
            input: None,
            output: None,
            reasoning: None,
            thinking: None,
            supports_tools: None,
            lifecycle: None,
            accepted_params: None,
            auth_type: None,
        }
    }
}

/// What ONE source claims about ONE model.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Observation {
    pub record_type: RecordType,
    pub source: ProvenanceSource,
    #[serde(flatten)]
    pub fields: Fields,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    /// Source fields we did not normalise, passed through verbatim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<Map<String, Value>>,
}

impl Observation {
    pub fn new(source: ProvenanceSource, fields: Fields) -> Self {
        Self {
            record_type: RecordType::Observation,
            source,
            fields,
            selector: None,
            raw: None,
        }
    }
}

/// Rule tier by which a reference-catalog entry matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum MatchTier {
    #[serde(rename = "T1-exact-full")]
    ExactFull,
    #[serde(rename = "T2-exact-leaf")]
    ExactLeaf,
    #[serde(rename = "T3-normalized-leaf")]
    NormalizedLeaf,
    #[serde(rename = "T4-date-stripped")]
    DateStripped,
}

/// Pointer to a reference-catalog entry that matched.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRef {
    pub source_id: String,
    pub provider: String,
    pub id: String,
    pub tier: MatchTier,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_party: Option<bool>,
}

/// Names that may appear as keys of `provenance.fields`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FieldName {
    ContextWindow,
    MaxInputTokens,
    MaxTokens,
    Cost,
    Input,
    Output,
    Reasoning,
    Thinking,
    SupportsTools,
    Lifecycle,
    AcceptedParams,
    Name,
    CanonicalId,
}

/// A disagreeing candidate kept for audit.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Alternate {
    pub source: String,
    pub value: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Confidence>,
}

/// Why a value won: source, rule and any disagreeing candidates.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldProvenance {
    /// `sourceId` of the winner.
    pub source: String,
    pub rule: String,
    pub confidence: Confidence,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alternates: Vec<Alternate>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelProvenance {
    pub sources: Vec<ProvenanceSource>,
    pub fields: BTreeMap<FieldName, FieldProvenance>,
}

/// The merged record the gateway serves.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub record_type: RecordType,
    /// `provider/id`; the id itself may contain `/`.
    pub key: String,
    #[serde(flatten)]
    pub fields: Fields,
    pub references: Vec<ModelRef>,
    pub provenance: ModelProvenance,
}

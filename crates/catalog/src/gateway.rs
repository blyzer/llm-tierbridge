//! Helpers shared by the gateway adapters (`cliproxyapi-cache`, `omp-live:*`, `omp-bundled`).
//!
//! These sources describe what a *gateway* serves. Two rules set them apart from the reference
//! catalogs (models.dev, modelparams.dev):
//!
//!   * a gateway fills `0` where it has no price or limit, so an all-zero price list is
//!     "unknown", never "free" (`zero_means_unknown`, ported from the prototype's `cost_obj`);
//!   * a price that is out of contract (a negative number) makes the whole `cost` unknown: with
//!     the gateway's stand-in zeros left in place the remaining prices would read as "free".

use std::collections::{BTreeSet, HashSet};

use serde_json::{Map, Value};

use crate::CatalogError;
use crate::diagnostics::{Diagnostic, Findings, SentinelMapping};
use crate::model::{Cost, CostStatus, Effort, Modality, ModelKind, PriceTier};

/// Why a published limit of `0` becomes unknown.
pub(crate) const ZERO_LIMIT_REASON: &str =
    "the gateway publishes 0 when it does not know the limit";

/// Per-model reporting context.
pub(crate) struct Cx<'a> {
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

    pub fn sentinel(&mut self, field: &str, value: &Value, mapped_to: &str, reason: &str) {
        self.findings.sentinels.push(SentinelMapping {
            source_id: self.source_id.to_owned(),
            provider: self.provider.to_owned(),
            id: self.id.to_owned(),
            field: field.to_owned(),
            value: value.clone(),
            mapped_to: mapped_to.to_owned(),
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

    /// Token limit. `0` is the gateway's "unknown": recorded as a sentinel and mapped to
    /// unknown. Negative or non-integer values are rejected.
    pub fn limit(&mut self, obj: &Map<String, Value>, key: &str, field: &str) -> Option<u64> {
        let value = obj.get(key)?;
        match value {
            Value::Null => None,
            Value::Number(n) => match n.as_u64() {
                Some(0) => {
                    self.findings.zero_limits += 1;
                    self.sentinel(field, value, "unknown", ZERO_LIMIT_REASON);
                    None
                }
                Some(v) => Some(v),
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

    /// USD per million tokens. `Err(())` = rejected (already reported).
    pub fn price(
        &mut self,
        obj: &Map<String, Value>,
        key: &str,
        field: &str,
    ) -> Result<Option<f64>, ()> {
        let Some(value) = obj.get(key) else {
            return Ok(None);
        };
        match value {
            Value::Null => Ok(None),
            Value::Number(n) => match n.as_f64() {
                Some(v) if v >= 0.0 => Ok(Some(v)),
                _ => {
                    self.reject(field, value, "negative price");
                    Err(())
                }
            },
            other => {
                self.reject(field, other, "price is not a number");
                Err(())
            }
        }
    }

    /// Modalities: sorted and de-duplicated; values outside the contract are rejected.
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

    /// `kind`: null/absent -> unknown; a string outside the contract enum is rejected.
    pub fn kind(&mut self, obj: &Map<String, Value>) -> Option<ModelKind> {
        let value = obj.get("kind")?;
        let kind = match value {
            Value::Null => return None,
            Value::String(s) => match s.as_str() {
                "chat" => Some(ModelKind::Chat),
                "embedding" => Some(ModelKind::Embedding),
                "image" => Some(ModelKind::Image),
                "video" => Some(ModelKind::Video),
                "rerank" => Some(ModelKind::Rerank),
                "stt" => Some(ModelKind::Stt),
                "tts" => Some(ModelKind::Tts),
                "other" => Some(ModelKind::Other),
                _ => None,
            },
            _ => None,
        };
        if kind.is_none() {
            self.reject("kind", value, "kind outside the contract enum");
        }
        kind
    }

    /// A list of effort spellings -> distinct efforts in declaration order (`off` .. `max`).
    /// Spellings outside the contract (`default`, `ultra`) are rejected one by one.
    pub fn efforts(&mut self, items: &[Value], field: &str) -> Vec<Effort> {
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

    /// An optional array of effort spellings; `null`/absent means none were given.
    pub fn effort_list(&mut self, value: Option<&Value>, field: &str) -> Option<Vec<Effort>> {
        match value? {
            Value::Null => None,
            Value::Array(items) => Some(self.efforts(items, field)),
            other => {
                self.reject(field, other, "expected an array");
                None
            }
        }
    }

    /// Replacement prices above a prompt-size threshold. A tier with a rejected price is
    /// dropped whole: a tier's prices *replace* the base prices, so a partial tier would
    /// misstate them.
    pub fn price_tier(
        &mut self,
        tier: &Value,
        path: &str,
        threshold_key: &str,
    ) -> Option<PriceTier> {
        let Some(t) = tier.as_object() else {
            self.reject(path, tier, "expected an object");
            return None;
        };
        let threshold = match t.get(threshold_key).and_then(Value::as_u64) {
            Some(s) if s > 0 => s,
            _ => {
                self.reject(
                    &format!("{path}.{threshold_key}"),
                    t.get(threshold_key).unwrap_or(&Value::Null),
                    "tier without a positive integer threshold",
                );
                return None;
            }
        };
        let mut price = |key: &str| self.price(t, key, &format!("{path}.{key}"));
        let (input, output) = (price("input"), price("output"));
        let (cache_read, cache_write) = (price("cacheRead"), price("cacheWrite"));
        Some(PriceTier {
            prompt_tokens_above: threshold,
            input: input.ok()?,
            output: output.ok()?,
            cache_read: cache_read.ok()?,
            cache_write: cache_write.ok()?,
        })
    }

    /// The `cost` object of a gateway row (`input`/`output`/`cacheRead`/`cacheWrite`, optional
    /// `tiers`). `tier_threshold_key` is `Some("inputTokensAbove")` for sources that carry tiers.
    pub fn gateway_cost(
        &mut self,
        cost: Option<&Value>,
        tier_threshold_key: Option<&str>,
        zero_means_unknown: bool,
    ) -> Option<Cost> {
        let c = match cost? {
            Value::Null => return None,
            Value::Object(o) => o,
            other => {
                self.reject("cost", other, "expected an object");
                return None;
            }
        };
        if c.is_empty() {
            return None;
        }
        let mut tiers = Vec::new();
        if let Some(key) = tier_threshold_key {
            match c.get("tiers") {
                None | Some(Value::Null) => {}
                Some(Value::Array(list)) => {
                    for (i, t) in list.iter().enumerate() {
                        tiers.extend(self.price_tier(t, &format!("cost.tiers[{i}]"), key));
                    }
                }
                Some(other) => self.reject("cost.tiers", other, "expected an array"),
            }
        }
        let mut rejected = false;
        let mut top = |key: &str| match self.price(c, key, &format!("cost.{key}")) {
            Ok(p) => p,
            Err(()) => {
                rejected = true;
                None
            }
        };
        let (input, output) = (top("input"), top("output"));
        let (cache_read, cache_write) = (top("cacheRead"), top("cacheWrite"));
        if rejected {
            return Some(cost_obj([None; 4], Vec::new(), zero_means_unknown));
        }
        Some(cost_obj(
            [input, output, cache_read, cache_write],
            tiers,
            zero_means_unknown,
        ))
    }
}

/// Port of the prototype's `cost_obj`: no price at all is `unknown`; an all-zero price list
/// without tiers is `unknown` when the gateway fills 0 for "no price" and `free` otherwise.
pub(crate) fn cost_obj(
    prices: [Option<f64>; 4],
    tiers: Vec<PriceTier>,
    zero_means_unknown: bool,
) -> Cost {
    let [input, output, cache_read, cache_write] = prices;
    let unknown = || Cost {
        status: CostStatus::Unknown,
        input: None,
        output: None,
        cache_read: None,
        cache_write: None,
        tiers: Vec::new(),
    };
    if prices.iter().all(Option::is_none) {
        return unknown();
    }
    if tiers.is_empty() && prices.iter().all(|p| p.unwrap_or(0.0) == 0.0) {
        if zero_means_unknown {
            return unknown();
        }
        return Cost {
            status: CostStatus::Free,
            input: Some(0.0),
            output: Some(0.0),
            cache_read,
            cache_write,
            tiers,
        };
    }
    Cost {
        status: CostStatus::Known,
        input,
        output,
        cache_read,
        cache_write,
        tiers,
    }
}

/// Fails a snapshot in which `provider/id` appears twice.
pub(crate) fn ensure_unique(
    seen: &mut HashSet<(String, String)>,
    provider: &str,
    id: &str,
    what: &str,
) -> Result<(), CatalogError> {
    if seen.insert((provider.to_owned(), id.to_owned())) {
        Ok(())
    } else {
        Err(CatalogError::Malformed(format!(
            "{what} lists {provider}/{id} more than once"
        )))
    }
}

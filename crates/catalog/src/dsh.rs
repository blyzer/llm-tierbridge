//! dsh (`Ebony-Vinyl/dsh-our-free-model`) adapter: the aggregate (bring-your-own-key) provider
//! registry -> [`Observation`]s.
//!
//! Only `src/providers/registry.json` is read. The repository's `catalog/` and `feed/` hold release
//! integrity data and announcements, no models; `vendor/channel-pack` and every account, pool or
//! channel module are never read. Endpoint URLs, credential variable names, notes and websites of
//! the registry entries are not carried over: the registry is used for model facts only.
//!
//! The registry is derived from models.dev, 9router and official documentation (each provider
//! entry says which in `source`), so it is a secondary source: confidence `inferred`, and the
//! provider's own `source` tag rides along in `raw.registrySource` so the merge can drop or rank
//! rows by upstream.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use crate::CatalogError;
use crate::diagnostics::Findings;
use crate::model::{
    AuthType, Confidence, Fields, Modality, Observation, ProvenanceSource, Thinking, ThinkingMode,
};
use crate::models_dev::{Adapted, sha256_hex};
use crate::snapshot::{Cx, Envelope, expect_array, expect_object, malformed, required_str};

pub const SOURCE_ID: &str = "dsh";
pub const REPOSITORY_URL: &str = "https://github.com/Ebony-Vinyl/dsh-our-free-model";
const REGISTRY_FILE: &str = "src/providers/registry.json";
const NOTE: &str =
    "aggregate provider registry only, derived from models.dev/9router/official docs";

/// How a provider authenticates, resolved once per provider and applied to each of its models.
enum Auth {
    Known(AuthType),
    Absent,
    Invalid(Value, &'static str),
}

fn provider_auth(provider: &Map<String, Value>) -> Auth {
    match provider.get("auth") {
        None | Some(Value::Null) => Auth::Absent,
        Some(Value::Object(auth)) => match auth.get("kind") {
            None | Some(Value::Null) => Auth::Absent,
            Some(Value::String(kind)) => match kind.as_str() {
                "apiKey" => Auth::Known(AuthType::ApiKey),
                "oauth" => Auth::Known(AuthType::Oauth),
                "none" => Auth::Known(AuthType::None),
                _ => Auth::Invalid(
                    Value::String(kind.clone()),
                    "auth kind outside the contract enum (api_key, subscription, oauth, none)",
                ),
            },
            Some(other) => Auth::Invalid(other.clone(), "auth kind is not a string"),
        },
        Some(other) => Auth::Invalid(other.clone(), "auth is not an object"),
    }
}

/// Adapts a snapshot envelope. `fetched_at_ms` is when the bytes were obtained.
pub fn adapt(bytes: &[u8], fetched_at_ms: i64) -> Result<Adapted, CatalogError> {
    let env = Envelope::parse(bytes, SOURCE_ID)?;
    let sha256 = sha256_hex(bytes);
    let source = env.descriptor(SOURCE_ID, Confidence::Inferred, NOTE, fetched_at_ms);
    let files = expect_object(env.root.get("files"), "`files`")?;
    let providers = expect_array(files.get(REGISTRY_FILE), REGISTRY_FILE)?;

    let mut findings = Findings::default();
    let mut observations = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, provider) in providers.iter().enumerate() {
        let what = format!("{REGISTRY_FILE}[{index}]");
        let provider = expect_object(Some(provider), &what)?;
        let provider_id = required_str(provider, "id", &what)?;
        if !seen.insert(provider_id) {
            return Err(malformed(format!(
                "provider `{provider_id}` is listed twice"
            )));
        }
        let models = expect_array(
            provider.get("models"),
            &format!("provider `{provider_id}` `models`"),
        )?;
        let auth = provider_auth(provider);
        let mut model_ids = BTreeSet::new();
        for (m_index, model) in models.iter().enumerate() {
            let what = format!("provider `{provider_id}` models[{m_index}]");
            let model = expect_object(Some(model), &what)?;
            let id = required_str(model, "id", &what)?;
            if !model_ids.insert(id) {
                return Err(malformed(format!(
                    "provider `{provider_id}` lists `{id}` more than once"
                )));
            }
            let mut cx = Cx {
                source_id: SOURCE_ID,
                provider: provider_id,
                id,
                findings: &mut findings,
            };
            observations.push(adapt_model(&mut cx, &source, provider, model, &auth));
        }
    }
    if observations.is_empty() {
        return Err(CatalogError::NoModels);
    }
    observations.sort_by(|a, b| {
        (&a.fields.provider, &a.fields.id).cmp(&(&b.fields.provider, &b.fields.id))
    });
    Ok(Adapted {
        source,
        sha256,
        providers: providers.len(),
        observations,
        findings,
    })
}

fn adapt_model(
    cx: &mut Cx<'_>,
    source: &ProvenanceSource,
    provider: &Map<String, Value>,
    m: &Map<String, Value>,
    auth: &Auth,
) -> Observation {
    let mut fields = Fields::empty(cx.provider, cx.id);
    fields.name = cx.string(m, "name", "name");
    fields.context_window = cx.token_limit(m, "contextWindow", "contextWindow");
    fields.reasoning = cx.boolean(m, "reasoning", "reasoning");
    fields.thinking = fields.reasoning.map(|r| Thinking {
        // The registry says whether a model reasons, never how.
        mode: if r {
            ThinkingMode::Unknown
        } else {
            ThinkingMode::None
        },
        efforts: Vec::new(),
        options: None,
        default_level: None,
        effort_map: None,
    });
    // `vision: true` -> images in; `vision: false` -> text only; `null` -> unknown.
    fields.input = cx.boolean(m, "vision", "input").map(|vision| {
        if vision {
            vec![Modality::Image, Modality::Text]
        } else {
            vec![Modality::Text]
        }
    });
    match auth {
        Auth::Known(a) => fields.auth_type = Some(*a),
        Auth::Absent => {}
        Auth::Invalid(value, reason) => cx.reject("authType", value, reason),
    }

    let mut raw = Map::new();
    for (key, name) in [
        ("source", "registrySource"),
        ("sourceUrl", "registrySourceUrl"),
        ("category", "category"),
        ("format", "format"),
        ("local", "local"),
    ] {
        if let Some(v) = provider.get(key).filter(|v| !v.is_null()) {
            raw.insert(name.to_owned(), v.clone());
        }
    }
    if let Some(Value::String(kind)) = provider
        .get("auth")
        .and_then(Value::as_object)
        .and_then(|a| a.get("kind"))
    {
        raw.insert("authKind".to_owned(), json!(kind));
    }

    let mut obs = Observation::new(source.clone(), fields);
    obs.raw = (!raw.is_empty()).then_some(raw);
    obs
}

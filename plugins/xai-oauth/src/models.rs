use std::collections::HashMap;

use covalt_provider::{Control, Model, Pricing, ProviderError, Tag, TagTone};
use reqwest::Client;
use serde_json::Value;

pub const XAI_API_BASE: &str = "https://api.x.ai/v1";
pub const CLI_PROXY_BASE: &str = "https://cli-chat-proxy.grok.com/v1";

const XAI_MODELS_URL: &str = "https://api.x.ai/v1/models";
const CLI_MODELS_URL: &str = "https://cli-chat-proxy.grok.com/v1/models";

const CLI_PROXY_MODELS: &[&str] = &["grok-build", "grok-composer-2.5-fast"];
const REASONING_MODEL_PREFIXES: &[&str] = &["grok-3-mini", "grok-4.20-multi-agent", "grok-4.3"];

pub async fn list_models(token: &str) -> Result<Vec<Model>, ProviderError> {
    let mut merged: HashMap<String, Model> = HashMap::new();

    if let Ok(entries) = fetch_models(XAI_MODELS_URL, token).await {
        for entry in entries {
            if let Some(model) = model_from_api_entry(&entry) {
                merged.insert(model.id.clone(), model);
            }
        }
    }

    if let Ok(entries) = fetch_models(CLI_MODELS_URL, token).await {
        for entry in entries {
            if let Some(model) = model_from_cli_entry(&entry) {
                merged.insert(model.id.clone(), model);
            }
        }
    }

    let mut models: Vec<Model> = merged.into_values().collect();
    models.sort_by_cached_key(|model| model.name.to_ascii_lowercase());
    Ok(models)
}

pub fn is_cli_proxy_model(model: &str) -> bool {
    let lowered = model.trim().to_ascii_lowercase();
    CLI_PROXY_MODELS.iter().any(|id| *id == lowered)
}

pub fn cli_proxy_headers(model: &str) -> HashMap<String, String> {
    HashMap::from([
        (
            "x-grok-client-identifier".into(),
            "covalt-xai-oauth".into(),
        ),
        ("x-grok-client-version".into(), "0.2.16".into()),
        ("x-xai-token-auth".into(), "xai-grok-cli".into()),
        ("x-grok-model-override".into(), model.to_string()),
    ])
}

pub fn supports_reasoning(model_id: &str) -> bool {
    let normalized = model_id.to_ascii_lowercase();
    let normalized = normalized.split('/').next_back().unwrap_or(&normalized);
    REASONING_MODEL_PREFIXES
        .iter()
        .any(|prefix| normalized.starts_with(prefix))
}

pub fn reasoning_controls(model_id: &str) -> Vec<Control> {
    if !supports_reasoning(model_id) {
        return Vec::new();
    }
    vec![Control::segmented(
        "reasoning",
        vec![
            "none".into(),
            "low".into(),
            "medium".into(),
            "high".into(),
        ],
        Some("medium".into()),
        false,
        Some("Reasoning".into()),
    )]
}

pub fn reasoning_effort(req: &Value) -> Option<String> {
    let options = req.get("options")?.as_object()?;
    for key in ["reasoning", "reasoningEffort", "reasoning_effort"] {
        if let Some(value) = options.get(key).and_then(Value::as_str) {
            let normalized = value.trim().to_ascii_lowercase();
            if normalized == "none" || normalized == "off" {
                return Some("none".into());
            }
            if ["low", "medium", "high"].contains(&normalized.as_str()) {
                return Some(normalized);
            }
        }
    }
    let params = options.get("requestParams")?.as_object()?;
    for key in ["reasoning", "reasoningEffort", "reasoning_effort"] {
        if let Some(value) = params.get(key).and_then(Value::as_str) {
            let normalized = value.trim().to_ascii_lowercase();
            if normalized == "none" || normalized == "off" {
                return Some("none".into());
            }
            if ["low", "medium", "high"].contains(&normalized.as_str()) {
                return Some(normalized);
            }
        }
    }
    None
}

async fn fetch_models(url: &str, token: &str) -> Result<Vec<Value>, ProviderError> {
    let client = Client::new();
    let resp = client
        .get(url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|err| ProviderError::Message(err.to_string()))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|err| ProviderError::Message(err.to_string()))?;
    if !status.is_success() {
        return Err(ProviderError::Message(format!(
            "models fetch failed ({url}): {status} {}",
            text.chars().take(400).collect::<String>()
        )));
    }
    let payload: Value = serde_json::from_str(&text)
        .map_err(|err| ProviderError::Message(format!("models response invalid JSON: {err}")))?;
    Ok(payload
        .get("data")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter(|item| item.is_object()).cloned().collect())
        .unwrap_or_default())
}

fn model_from_api_entry(entry: &Value) -> Option<Model> {
    let model_id = entry.get("id")?.as_str()?.trim();
    if model_id.is_empty() {
        return None;
    }
    let name = entry
        .get("aliases")
        .and_then(Value::as_array)
        .and_then(|aliases| aliases.first())
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(model_id)
        .to_string();

    let context_window = entry
        .get("context_length")
        .or_else(|| entry.get("context_window"))
        .and_then(Value::as_i64);

    let pricing = {
        let input = price_per_million(entry.get("prompt_text_token_price"));
        let output = price_per_million(entry.get("completion_text_token_price"));
        let cached = price_per_million(entry.get("cached_prompt_text_token_price"));
        if input.is_some() || output.is_some() || cached.is_some() {
            Some(Pricing {
                input,
                output,
                cached_input: cached,
                per: "1M".into(),
            })
        } else {
            None
        }
    };

    let mut tags = Vec::new();
    if let Some(tag) = model_kind_tag(model_id) {
        tags.push(tag);
    }

    Some(Model {
        id: model_id.to_string(),
        name,
        description: Some(format!("xAI model ({model_id})")),
        context_window,
        max_output: None,
        pricing,
        tags,
        details: HashMap::from([("Provider".into(), "xAI".into())]),
        controls: reasoning_controls(model_id),
    })
}

fn model_from_cli_entry(entry: &Value) -> Option<Model> {
    let model_id = entry
        .get("id")
        .or_else(|| entry.get("model"))
        .and_then(Value::as_str)?
        .trim();
    if model_id.is_empty() {
        return None;
    }

    let name = entry
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(model_id)
        .to_string();
    let description = entry
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("xAI coding model ({model_id})"));

    let context_window = entry.get("context_window").and_then(Value::as_i64);

    let mut tags = vec![Tag {
        label: "Coding".into(),
        tone: TagTone::Positive,
    }];
    if entry
        .get("supports_backend_search")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        tags.push(Tag {
            label: "Search".into(),
            tone: TagTone::Positive,
        });
    }

    let mut details = HashMap::from([("Provider".into(), "xAI".into())]);
    if let Some(agent_type) = entry.get("agent_type").and_then(Value::as_str) {
        let trimmed = agent_type.trim();
        if !trimmed.is_empty() {
            details.insert("Agent type".into(), trimmed.to_string());
        }
    }

    Some(Model {
        id: model_id.to_string(),
        name,
        description: Some(description),
        context_window,
        max_output: None,
        pricing: None,
        tags,
        details,
        controls: reasoning_controls(model_id),
    })
}

fn price_per_million(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(number)) => number.as_f64().map(|value| value / 10_000.0),
        _ => None,
    }
}

fn model_kind_tag(model_id: &str) -> Option<Tag> {
    let lowered = model_id.to_ascii_lowercase();
    if lowered.contains("imagine-image") {
        return Some(Tag {
            label: "Image".into(),
            tone: TagTone::Neutral,
        });
    }
    if lowered.contains("imagine-video") {
        return Some(Tag {
            label: "Video".into(),
            tone: TagTone::Neutral,
        });
    }
    if CLI_PROXY_MODELS.contains(&lowered.as_str()) {
        return Some(Tag {
            label: "Coding".into(),
            tone: TagTone::Positive,
        });
    }
    None
}
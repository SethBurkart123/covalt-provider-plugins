use std::collections::HashMap;

use covalt_provider::{Control, Model, ProviderError};
use reqwest::Client;
use serde::Deserialize;

const MODELS_URL: &str = "https://api.anthropic.com/v1/models";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const ANTHROPIC_BETA: &str =
    "claude-code-20250219,oauth-2025-04-20,fine-grained-tool-streaming-2025-05-14,interleaved-thinking-2025-05-14";

const LOW_TO_MAX: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const LOW_TO_HIGH: &[&str] = &["low", "medium", "high"];
const LOW_TO_LEGACY_MAX: &[&str] = &["low", "medium", "high", "max"];
const BUDGET_LEVELS: &[&str] = &["none", "low", "medium", "high", "max"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThinkingMode {
    Adaptive,
    BudgetWithEffort,
    Budget,
}

#[derive(Clone, Copy, Debug)]
pub struct ReasoningSpec {
    pub mode: ThinkingMode,
    pub efforts: &'static [&'static str],
    pub can_disable: bool,
}

#[derive(Deserialize)]
struct ModelsPage {
    #[serde(default)]
    data: Vec<ApiModel>,
    #[serde(default)]
    has_more: bool,
    last_id: Option<String>,
}

#[derive(Deserialize)]
struct ApiModel {
    id: String,
    display_name: String,
    created_at: Option<String>,
}

pub async fn list_models(token: &str) -> Result<Vec<Model>, ProviderError> {
    let client = Client::new();
    let mut after_id = None;
    let mut models = Vec::new();

    loop {
        let mut request = client
            .get(MODELS_URL)
            .header("Authorization", format!("Bearer {token}"))
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("anthropic-beta", ANTHROPIC_BETA)
            .header("Accept", "application/json");
        if let Some(cursor) = after_id.as_deref() {
            request = request.query(&[("after_id", cursor)]);
        }

        let response = request
            .send()
            .await
            .map_err(|error| ProviderError::Message(error.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| ProviderError::Message(error.to_string()))?;
        if !status.is_success() {
            return Err(ProviderError::Message(format!(
                "models fetch failed: {status} {}",
                text.chars().take(400).collect::<String>()
            )));
        }

        let page: ModelsPage = serde_json::from_str(&text).map_err(|error| {
            ProviderError::Message(format!("models response invalid JSON: {error}"))
        })?;
        models.extend(page.data.into_iter().map(model_from_api));
        if !page.has_more {
            break;
        }
        let next = page.last_id.filter(|cursor| !cursor.is_empty());
        if next.is_none() || next == after_id {
            break;
        }
        after_id = next;
    }

    models.sort_by_cached_key(|model| model.name.to_ascii_lowercase());
    Ok(models)
}

fn model_from_api(entry: ApiModel) -> Model {
    let mut details = HashMap::from([("Provider".into(), "Anthropic".into())]);
    if let Some(created_at) = entry.created_at.filter(|value| !value.is_empty()) {
        details.insert("Created".into(), created_at);
    }
    Model {
        controls: reasoning_controls(&entry.id),
        id: entry.id,
        name: entry.display_name,
        description: Some("Claude subscription model".into()),
        context_window: None,
        max_output: None,
        pricing: None,
        tags: Vec::new(),
        details,
    }
}

pub fn reasoning_spec(model_id: &str) -> Option<ReasoningSpec> {
    let model = normalize_model_id(model_id);
    let spec =
        if matches_family(&model, "claude-fable-5") || matches_family(&model, "claude-mythos-5") {
            ReasoningSpec {
                mode: ThinkingMode::Adaptive,
                efforts: LOW_TO_MAX,
                can_disable: false,
            }
        } else if matches_family(&model, "claude-opus-5")
            || matches_family(&model, "claude-sonnet-5")
            || matches_family(&model, "claude-opus-4-8")
            || matches_family(&model, "claude-opus-4-7")
        {
            ReasoningSpec {
                mode: ThinkingMode::Adaptive,
                efforts: LOW_TO_MAX,
                can_disable: true,
            }
        } else if matches_family(&model, "claude-opus-4-6")
            || matches_family(&model, "claude-sonnet-4-6")
            || matches_family(&model, "claude-mythos-preview")
        {
            ReasoningSpec {
                mode: ThinkingMode::Adaptive,
                efforts: LOW_TO_LEGACY_MAX,
                can_disable: true,
            }
        } else if matches_family(&model, "claude-opus-4-5") {
            ReasoningSpec {
                mode: ThinkingMode::BudgetWithEffort,
                efforts: LOW_TO_HIGH,
                can_disable: true,
            }
        } else if [
            "claude-haiku-4-5",
            "claude-sonnet-4-5",
            "claude-opus-4-1",
            "claude-opus-4",
            "claude-sonnet-4",
            "claude-3-7-sonnet",
        ]
        .iter()
        .any(|family| matches_family(&model, family))
        {
            ReasoningSpec {
                mode: ThinkingMode::Budget,
                efforts: BUDGET_LEVELS,
                can_disable: true,
            }
        } else {
            return None;
        };
    Some(spec)
}

pub fn reasoning_controls(model_id: &str) -> Vec<Control> {
    let Some(spec) = reasoning_spec(model_id) else {
        return Vec::new();
    };
    let mut values: Vec<String> = spec.efforts.iter().map(|value| (*value).into()).collect();
    if spec.can_disable && spec.mode != ThinkingMode::Budget {
        values.insert(0, "none".into());
    }
    vec![Control::segmented(
        if spec.mode == ThinkingMode::Budget {
            "thinking"
        } else {
            "reasoning_effort"
        },
        values,
        Some(
            if spec.mode == ThinkingMode::Budget {
                "none"
            } else {
                "high"
            }
            .into(),
        ),
        false,
        Some(
            if spec.mode == ThinkingMode::Budget {
                "Thinking"
            } else {
                "Reasoning"
            }
            .into(),
        ),
    )]
}

fn normalize_model_id(model_id: &str) -> String {
    model_id
        .trim()
        .to_ascii_lowercase()
        .replace(['.', '_'], "-")
}

fn matches_family(model_id: &str, family: &str) -> bool {
    model_id == family
        || model_id
            .strip_prefix(family)
            .is_some_and(|suffix| suffix.starts_with('-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_current_reasoning_families() {
        assert_eq!(
            reasoning_spec("claude-fable-5").unwrap().mode,
            ThinkingMode::Adaptive
        );
        assert!(!reasoning_spec("claude-fable-5").unwrap().can_disable);
        assert_eq!(
            reasoning_spec("claude-opus-4.5-20251101").unwrap().mode,
            ThinkingMode::BudgetWithEffort
        );
        assert_eq!(
            reasoning_spec("claude-sonnet-4-20250514").unwrap().mode,
            ThinkingMode::Budget
        );
        assert!(reasoning_spec("claude-3-5-sonnet").is_none());
    }
}

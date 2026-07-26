use crate::models::{reasoning_spec, ThinkingMode};
use serde_json::{json, Value};

const SYSTEM_PREPEND: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

const REASONING_BUDGETS: [(&str, u32); 4] = [
    ("low", 2048),
    ("medium", 8192),
    ("high", 16384),
    ("max", 32000),
];

pub fn prepare(mut req: Value) -> Value {
    req = apply_reasoning(req);
    prepend_system(req)
}

fn prepend_system(mut req: Value) -> Value {
    let Some(obj) = req.as_object_mut() else {
        return req;
    };
    let blocks = obj.entry("systemBlocks").or_insert_with(|| json!([]));
    let Some(list) = blocks.as_array_mut() else {
        return req;
    };
    list.insert(0, json!(SYSTEM_PREPEND));
    req
}

fn apply_reasoning(mut req: Value) -> Value {
    let model_id = req
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let selection = reasoning_selection_from_request(&req);
    if let (Some(spec), Some(selection)) = (reasoning_spec(&model_id), selection.as_deref()) {
        if selection != "none" && spec.efforts.contains(&selection) {
            let budget = (spec.mode != ThinkingMode::Adaptive)
                .then(|| thinking_budget_from_request(&req, selection));
            let body = req
                .as_object_mut()
                .and_then(|obj| obj.get_mut("body"))
                .and_then(Value::as_object_mut);
            let body = if let Some(body) = body {
                body
            } else if let Some(obj) = req.as_object_mut() {
                obj.entry("body").or_insert_with(|| json!({}));
                obj.get_mut("body")
                    .and_then(Value::as_object_mut)
                    .expect("body object")
            } else {
                return req;
            };

            match spec.mode {
                ThinkingMode::Adaptive => {
                    body.insert("thinking".into(), json!({ "type": "adaptive" }));
                    body.insert("output_config".into(), json!({ "effort": selection }));
                }
                ThinkingMode::BudgetWithEffort => {
                    body.insert(
                        "thinking".into(),
                        json!({
                            "type": "enabled",
                            "budget_tokens": budget.expect("fixed thinking uses a budget")
                        }),
                    );
                    body.insert("output_config".into(), json!({ "effort": selection }));
                }
                ThinkingMode::Budget => {
                    body.insert(
                        "thinking".into(),
                        json!({
                            "type": "enabled",
                            "budget_tokens": budget.expect("fixed thinking uses a budget")
                        }),
                    );
                }
            }
        }
    }

    strip_reasoning_options(&mut req);
    req
}

fn strip_reasoning_options(req: &mut Value) {
    let Some(options) = req.get_mut("options").and_then(Value::as_object_mut) else {
        return;
    };
    options.remove("reasoningEffort");
    options.remove("reasoning_effort");
    options.remove("thinking");
    if let Some(params) = options
        .get_mut("requestParams")
        .and_then(Value::as_object_mut)
    {
        params.remove("reasoningEffort");
        params.remove("reasoning_effort");
        params.remove("thinking");
    }
}

fn reasoning_selection_from_request(req: &Value) -> Option<String> {
    let options = req.get("options")?.as_object()?;
    for key in ["reasoningEffort", "reasoning_effort", "thinking"] {
        if let Some(value) = options.get(key).and_then(Value::as_str) {
            if let Some(normalized) = normalize_reasoning_selection(value) {
                return Some(normalized);
            }
        }
    }
    let params = options.get("requestParams")?.as_object()?;
    for key in ["reasoningEffort", "reasoning_effort", "thinking"] {
        if let Some(value) = params.get(key).and_then(Value::as_str) {
            if let Some(normalized) = normalize_reasoning_selection(value) {
                return Some(normalized);
            }
        }
    }
    None
}

fn normalize_reasoning_selection(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "none" | "off" => Some("none".into()),
        "low" | "medium" | "high" | "xhigh" | "max" => Some(normalized),
        _ => None,
    }
}

fn thinking_budget_from_request(req: &Value, effort: &str) -> u32 {
    if let Some(thinking) = req
        .get("options")
        .and_then(Value::as_object)
        .and_then(|options| options.get("thinking"))
        .and_then(Value::as_object)
    {
        if let Some(budget) = thinking.get("budgetTokens").and_then(Value::as_u64) {
            if budget > 0 {
                return budget as u32;
            }
        }
    }
    REASONING_BUDGETS
        .iter()
        .find_map(|(level, budget)| (*level == effort).then_some(*budget))
        .unwrap_or(REASONING_BUDGETS[1].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepends_system_block() {
        let req = json!({ "model": "claude-sonnet-4-20250514" });
        let prepared = prepare(req);
        assert_eq!(prepared["systemBlocks"][0], json!(SYSTEM_PREPEND));
    }

    #[test]
    fn applies_thinking_budget_from_effort() {
        let req = json!({
            "model": "claude-sonnet-4-20250514",
            "options": { "reasoningEffort": "high" }
        });
        let prepared = prepare(req);
        assert_eq!(
            prepared["body"]["thinking"],
            json!({ "type": "enabled", "budget_tokens": 16384 })
        );
        assert!(prepared["options"].get("reasoningEffort").is_none());
    }

    #[test]
    fn adaptive_reasoning_uses_output_config() {
        let req = json!({
            "model": "claude-opus-4-6",
            "options": { "reasoning_effort": "medium" }
        });
        let prepared = prepare(req);
        assert_eq!(prepared["body"]["thinking"], json!({ "type": "adaptive" }));
        assert_eq!(
            prepared["body"]["output_config"],
            json!({ "effort": "medium" })
        );
    }

    #[test]
    fn preserves_distinct_xhigh_effort_for_opus_5() {
        let req = json!({
            "model": "claude-opus-5",
            "options": { "reasoning_effort": "xhigh" }
        });
        let prepared = prepare(req);
        assert_eq!(prepared["body"]["thinking"], json!({ "type": "adaptive" }));
        assert_eq!(
            prepared["body"]["output_config"],
            json!({ "effort": "xhigh" })
        );
    }

    #[test]
    fn fable_5_uses_adaptive_reasoning() {
        let req = json!({
            "model": "claude-fable-5",
            "options": { "reasoning_effort": "max" }
        });
        let prepared = prepare(req);
        assert_eq!(prepared["body"]["thinking"], json!({ "type": "adaptive" }));
        assert_eq!(
            prepared["body"]["output_config"],
            json!({ "effort": "max" })
        );
    }

    #[test]
    fn budget_models_use_thinking_control() {
        let req = json!({
            "model": "claude-sonnet-4-5",
            "options": { "thinking": "high" }
        });
        let prepared = prepare(req);
        assert_eq!(
            prepared["body"]["thinking"],
            json!({ "type": "enabled", "budget_tokens": 16384 })
        );
        assert!(prepared["options"].get("thinking").is_none());
    }
}

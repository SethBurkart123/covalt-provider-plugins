use serde_json::{json, Value};

const SYSTEM_PREPEND: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

const REASONING_BUDGETS: [(&str, u32); 6] = [
    ("minimal", 1024),
    ("low", 2048),
    ("medium", 8192),
    ("high", 16384),
    ("max", 32000),
    ("xhigh", 32000),
];

pub fn prepare(mut req: Value) -> Value {
    req = apply_reasoning(req);
    prepend_system(req)
}

fn prepend_system(mut req: Value) -> Value {
    let Some(obj) = req.as_object_mut() else {
        return req;
    };
    let blocks = obj
        .entry("systemBlocks")
        .or_insert_with(|| json!([]));
    let Some(list) = blocks.as_array_mut() else {
        return req;
    };
    list.insert(
        0,
        json!({ "type": "text", "text": SYSTEM_PREPEND }),
    );
    req
}

fn apply_reasoning(mut req: Value) -> Value {
    let model_id = req
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let effort = reasoning_effort_from_request(&req);
    if let Some(effort) = effort.as_deref() {
        if !matches!(effort, "none" | "auto") && supports_reasoning(&model_id) {
            let budget = if supports_adaptive_reasoning(&model_id) {
                None
            } else {
                Some(thinking_budget_from_request(&req, effort))
            };

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

            if supports_adaptive_reasoning(&model_id) {
                body.insert("thinking".into(), json!({ "type": "adaptive" }));
                body.insert(
                    "output_config".into(),
                    json!({ "effort": map_effort_to_anthropic(effort) }),
                );
            } else if let Some(budget) = budget {
                body.insert(
                    "thinking".into(),
                    json!({ "type": "enabled", "budget_tokens": budget }),
                );
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
    if let Some(params) = options.get_mut("requestParams").and_then(Value::as_object_mut) {
        params.remove("reasoningEffort");
        params.remove("reasoning_effort");
    }
}

fn reasoning_effort_from_request(req: &Value) -> Option<String> {
    let options = req.get("options")?.as_object()?;
    for key in ["reasoningEffort", "reasoning_effort"] {
        if let Some(value) = options.get(key).and_then(Value::as_str) {
            if let Some(normalized) = normalize_reasoning_effort(value) {
                return Some(normalized);
            }
        }
    }
    let params = options.get("requestParams")?.as_object()?;
    for key in ["reasoningEffort", "reasoning_effort"] {
        if let Some(value) = params.get(key).and_then(Value::as_str) {
            if let Some(normalized) = normalize_reasoning_effort(value) {
                return Some(normalized);
            }
        }
    }
    None
}

fn normalize_reasoning_effort(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "none" | "off" => Some("none".into()),
        "xhigh" => Some("max".into()),
        "auto" | "minimal" | "low" | "medium" | "high" | "max" => Some(normalized),
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
        .unwrap_or(REASONING_BUDGETS[2].1)
}

fn map_effort_to_anthropic(effort: &str) -> &'static str {
    match effort {
        "max" => "max",
        "minimal" | "low" => "low",
        "medium" => "medium",
        _ => "high",
    }
}

pub fn supports_reasoning(model_id: &str) -> bool {
    let model = model_id.to_ascii_lowercase();
    if model.contains("claude-3-7") {
        return true;
    }
    for family in ["haiku", "sonnet", "opus"] {
        let prefix = format!("claude-{family}-4");
        if model.starts_with(&prefix) {
            let rest = model.strip_prefix(&prefix).unwrap_or_default();
            if rest.is_empty() || rest.starts_with('.') || rest.starts_with('-') {
                return true;
            }
        }
    }
    model.starts_with("claude-opus-4.")
}

pub fn supports_adaptive_reasoning(model_id: &str) -> bool {
    let model = model_id.to_ascii_lowercase();
    model.contains("opus-4-6") || model.contains("opus-4.6")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepends_system_block() {
        let req = json!({ "model": "claude-sonnet-4-20250514" });
        let prepared = prepare(req);
        assert_eq!(
            prepared["systemBlocks"][0],
            json!({ "type": "text", "text": SYSTEM_PREPEND })
        );
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
}

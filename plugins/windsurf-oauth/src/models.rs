use covalt_provider::{Control, Model};

pub struct ResolvedModel {
    pub model_uid: String,
}

pub fn list_models() -> Vec<Model> {
    vec![
        model_entry("swe-1.6", "SWE 1.6", 1_000_000, 128_000, &["fast"], "fast"),
        model_entry(
            "claude-opus-4.7",
            "Claude Opus 4.7",
            1_000_000,
            128_000,
            &["low", "medium", "high", "xhigh", "max"],
            "medium",
        ),
        model_entry(
            "gpt-5.5",
            "GPT 5.5",
            1_050_000,
            128_000,
            &["none", "low", "medium", "high", "xhigh"],
            "medium",
        ),
        model_entry("kimi-k2.6", "Kimi K2.6", 262_144, 262_144, &[], ""),
        model_entry(
            "gemini-3.5-flash",
            "Gemini 3.5 Flash",
            1_048_576,
            65_536,
            &["minimal", "low", "medium", "high"],
            "medium",
        ),
        model_entry(
            "claude-opus-4.6",
            "Claude Opus 4.6",
            1_000_000,
            128_000,
            &["thinking", "1m", "thinking-1m", "fast", "thinking-fast"],
            "thinking",
        ),
        model_entry("deepseek-v4", "DeepSeek V4", 1_000_000, 384_000, &[], ""),
    ]
}

fn model_entry(
    id: &str,
    name: &str,
    context_window: i64,
    max_output: i64,
    variants: &[&str],
    default_variant: &str,
) -> Model {
    let mut model = Model {
        id: id.to_string(),
        name: name.to_string(),
        description: Some(format!("Windsurf subscription model ({name})")),
        context_window: Some(context_window),
        max_output: Some(max_output),
        pricing: None,
        tags: vec![],
        details: std::collections::HashMap::from([(
            "Provider".to_string(),
            "Cognition (Windsurf)".to_string(),
        )]),
        controls: Vec::new(),
    };
    if !variants.is_empty() {
        model.controls.push(Control::segmented(
            "variant",
            variants.iter().map(|value| (*value).to_string()).collect(),
            Some(default_variant.to_string()),
            false,
            Some("Variant".to_string()),
        ));
    }
    model
}

pub fn resolve_model(model: &str, variant_override: Option<&str>) -> Result<ResolvedModel, String> {
    let requested = model.trim();
    let (base, inline_variant) = split_model_and_variant(model);
    let variant = variant_override
        .or(inline_variant.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let uid = match base {
        "swe-1.6" | "swe-1-6" => match variant {
            Some("fast") => "swe-1-6-fast",
            _ => "swe-1-6",
        },
        "claude-opus-4.7" | "claude-opus-4-7" => match variant {
            Some("low") => "claude-opus-4-7-low",
            Some("high") => "claude-opus-4-7-high",
            Some("xhigh") => "claude-opus-4-7-xhigh",
            Some("max") => "claude-opus-4-7-max",
            Some("low-fast") => "claude-opus-4-7-low-fast",
            Some("medium-fast") => "claude-opus-4-7-medium-fast",
            Some("high-fast") => "claude-opus-4-7-high-fast",
            Some("xhigh-fast") => "claude-opus-4-7-xhigh-fast",
            Some("max-fast") => "claude-opus-4-7-max-fast",
            Some("medium") | None => "claude-opus-4-7-medium",
            Some(other) => return Err(format!("Unknown variant for {base}: {other}")),
        },
        "gpt-5.5" | "gpt-5-5" => match variant {
            Some("none") => "gpt-5-5-none",
            Some("low") => "gpt-5-5-low",
            Some("high") => "gpt-5-5-high",
            Some("xhigh") => "gpt-5-5-xhigh",
            Some("medium") | None => "gpt-5-5-medium",
            Some(other) => return Err(format!("Unknown variant for {base}: {other}")),
        },
        "kimi-k2.6" | "kimi-k2-6" => "kimi-k2-6",
        "gemini-3.5-flash" | "gemini-3-5-flash" => match variant {
            Some("minimal") => "gemini-3-5-flash-minimal",
            Some("low") => "gemini-3-5-flash-low",
            Some("high") => "gemini-3-5-flash-high",
            Some("medium") | None => "gemini-3-5-flash-medium",
            Some(other) => return Err(format!("Unknown variant for {base}: {other}")),
        },
        "claude-opus-4.6" | "claude-opus-4-6" => match variant {
            Some("1m") => "claude-opus-4-6-1m",
            Some("thinking-1m") => "claude-opus-4-6-thinking-1m",
            Some("fast") => "claude-opus-4-6-fast",
            Some("thinking-fast") => "claude-opus-4-6-thinking-fast",
            Some("thinking") | None => "claude-opus-4-6-thinking",
            Some(other) => return Err(format!("Unknown variant for {base}: {other}")),
        },
        "deepseek-v4" | "deepseek-v-4" => "deepseek-v4",
        other => {
            return Ok(ResolvedModel {
                model_uid: if variant.is_some() {
                    requested.to_string()
                } else {
                    other.to_string()
                },
            });
        }
    };
    Ok(ResolvedModel {
        model_uid: uid.to_string(),
    })
}

fn split_model_and_variant(model: &str) -> (&str, Option<String>) {
    if let Some((base, variant)) = model.rsplit_once(':') {
        return (base, Some(variant.to_string()));
    }
    (model, None)
}

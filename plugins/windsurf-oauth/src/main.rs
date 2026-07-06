use std::collections::HashMap;
use std::pin::Pin;

use async_stream::stream;
use async_trait::async_trait;
use covalt_provider::{
    events, method::Method, Auth, Model, Provider, ProviderContext, ProviderError,
};
use futures::{Stream, StreamExt};
use serde_json::Value;
use windsurf_provider::auth;
use windsurf_provider::cloud;
use windsurf_provider::cloud::chat::{
    messages_from_json, stream_chat_events, tools_from_json, ChatHistoryItem, CloudChatEvent,
    CloudChatRequest, ContentPart, FinishReason,
};
use windsurf_provider::models::{list_models, resolve_model};

struct WindsurfProvider;

#[async_trait]
impl Provider for WindsurfProvider {
    fn id(&self) -> &str {
        "windsurf"
    }

    fn name(&self) -> &str {
        "Windsurf"
    }

    fn implements(&self) -> &'static [Method] {
        &[
            Method::ModelsList,
            Method::Stream,
            Method::Login,
            Method::Logout,
        ]
    }

    async fn models(&self, ctx: &ProviderContext) -> Result<Vec<Model>, ProviderError> {
        let api_key = match credential_api_key(ctx) {
            Ok(value) => value,
            Err(_) => return Ok(list_models()),
        };
        let api_server_url = credential_api_server(ctx);
        match cloud::list_catalog_models(&api_key, &api_server_url).await {
            Ok(models) if !models.is_empty() => Ok(models),
            Ok(_) => Ok(list_models()),
            Err(err) => {
                eprintln!("Windsurf model catalog unavailable: {err}");
                Ok(list_models())
            }
        }
    }

    async fn login(&self, ctx: &ProviderContext) -> Result<Auth, ProviderError> {
        auth::login(ctx).await
    }

    async fn logout(&self, _ctx: &ProviderContext, _auth: &Auth) -> Result<(), ProviderError> {
        cloud::clear_cached_user_jwt();
        Ok(())
    }

    fn stream(
        &self,
        ctx: &ProviderContext,
        req: Value,
    ) -> Pin<Box<dyn Stream<Item = Result<Value, ProviderError>> + Send + '_>> {
        let api_key = match credential_api_key(ctx) {
            Ok(value) => value,
            Err(err) => {
                return Box::pin(stream! {
                    yield Err(err);
                });
            }
        };
        let api_server_url = credential_api_server(ctx);
        let inference_server_url = cloud::auth::inference_host_for(&api_server_url);
        let model = req
            .get("model")
            .and_then(Value::as_str)
            .or(ctx.model.as_deref())
            .unwrap_or("swe-1.6")
            .to_string();
        let variant = req
            .get("options")
            .and_then(Value::as_object)
            .and_then(|options| options.get("variant"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut messages = messages_from_json(req.get("messages").unwrap_or(&Value::Null));
        if let Some(system) = req
            .get("system")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            messages.insert(
                0,
                ChatHistoryItem {
                    role: "system".to_string(),
                    content: vec![ContentPart::Text { text: system.to_string() }],
                    tool_call_id: None,
                    tool_calls: Vec::new(),
                },
            );
        }
        let tools = tools_from_json(req.get("tools").unwrap_or(&Value::Null));
        let max_output_tokens = req
            .get("options")
            .and_then(Value::as_object)
            .and_then(|options| options.get("maxTokens"))
            .and_then(Value::as_u64)
            .unwrap_or(128_000);

        Box::pin(stream! {
            let resolved = match resolve_model(&model, variant.as_deref()) {
                Ok(value) => value,
                Err(err) => {
                    yield Err(ProviderError::Message(err));
                    return;
                }
            };
            let mut cloud_events = match stream_chat_events(CloudChatRequest {
                api_key,
                api_server_url,
                inference_server_url,
                model_uid: resolved.model_uid,
                messages,
                tools,
                max_output_tokens,
            }).await {
                Ok(value) => value,
                Err(err) => {
                    yield Err(ProviderError::Message(err));
                    return;
                }
            };

            let mut tool_index = 0u32;
            let mut tool_id_to_index: HashMap<String, u32> = HashMap::new();
            let mut last_tool_id: Option<String> = None;
            let mut saw_tool_call = false;
            let mut finish_reason = FinishReason::Stop;

            while let Some(event) = cloud_events.next().await {
                let event = match event {
                    Ok(value) => value,
                    Err(err) => {
                        yield Err(ProviderError::Message(err));
                        return;
                    }
                };
                match event {
                    CloudChatEvent::Text { text } => yield Ok(events::text(text)),
                    CloudChatEvent::Reasoning { text } => yield Ok(events::thinking(text)),
                    CloudChatEvent::ToolCallStart { id, name } => {
                        saw_tool_call = true;
                        tool_id_to_index.insert(id.clone(), tool_index);
                        last_tool_id = Some(id.clone());
                        yield Ok(events::tool_call_start(id, name, tool_index));
                        tool_index += 1;
                    }
                    CloudChatEvent::ToolCallArgs { args_delta, id } => {
                        // Windsurf sends arg frames without an id; they belong
                        // to the most recently started tool call.
                        let Some(id) = id.or_else(|| last_tool_id.clone()) else {
                            continue;
                        };
                        let index = tool_id_to_index
                            .get(&id)
                            .copied()
                            .unwrap_or(tool_index.saturating_sub(1));
                        yield Ok(events::tool_call_arg_delta(id, args_delta, index));
                    }
                    CloudChatEvent::Finish { reason } => finish_reason = reason,
                    CloudChatEvent::Usage {
                        prompt_tokens,
                        completion_tokens,
                        ..
                    } => yield Ok(events::usage(prompt_tokens, completion_tokens, None, None)),
                }
            }

            for (id, index) in &tool_id_to_index {
                yield Ok(events::tool_call_end(id.clone(), *index));
            }
            let stop_reason = match finish_reason {
                FinishReason::ToolCalls => "tool_calls",
                FinishReason::Length => "length",
                FinishReason::ContentFilter => "content_filter",
                FinishReason::Stop if saw_tool_call => "tool_calls",
                FinishReason::Stop => "stop",
            };
            yield Ok(events::stop(stop_reason));
        })
    }
}

fn credential_api_key(ctx: &ProviderContext) -> Result<String, ProviderError> {
    if let Some(key) = ctx
        .auth
        .api_key
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        return Ok(key.to_string());
    }
    if let Some(token) = ctx
        .auth
        .access_token
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        return Ok(token.to_string());
    }
    Err(ProviderError::Message(
        "Windsurf is not signed in — connect your account first".into(),
    ))
}

fn credential_api_server(ctx: &ProviderContext) -> String {
    ctx.auth
        .extra
        .get("apiServerUrl")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| cloud::default_host().to_string())
}

covalt_provider::main!(WindsurfProvider);

use std::collections::HashMap;
use std::pin::Pin;

use async_stream::stream;
use async_trait::async_trait;
use covalt_provider::{
    events, method::Method, Auth, Model, Provider, ProviderContext, ProviderError,
};
use futures::Stream;
use serde_json::Value;
use windsurf_provider::auth;
use windsurf_provider::cloud;
use windsurf_provider::cloud::chat::{
    messages_from_json, stream_chat_events, tools_from_json, CloudChatEvent, CloudChatRequest,
    FinishReason,
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
        let messages = messages_from_json(req.get("messages").unwrap_or(&Value::Null));
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
            let cloud_events = match stream_chat_events(CloudChatRequest {
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
            for event in map_cloud_events(cloud_events) {
                yield Ok(event);
            }
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
        .base_url_override
        .clone()
        .unwrap_or_else(|| cloud::default_host().to_string())
}

fn map_cloud_events(events: Vec<CloudChatEvent>) -> Vec<Value> {
    let mut out = Vec::new();
    let mut tool_index = 0u32;
    let mut tool_id_to_index: HashMap<String, u32> = HashMap::new();
    let mut saw_tool_call = false;
    let mut finish_reason = FinishReason::Stop;

    for event in events {
        match event {
            CloudChatEvent::Text { text } => out.push(events::text(text)),
            CloudChatEvent::Reasoning { text } => out.push(events::thinking(text)),
            CloudChatEvent::ToolCallStart { id, name } => {
                saw_tool_call = true;
                tool_id_to_index.insert(id.clone(), tool_index);
                out.push(events::tool_call_start(id, name, tool_index));
                tool_index += 1;
            }
            CloudChatEvent::ToolCallArgs { args_delta, id } => {
                let index = id
                    .as_ref()
                    .and_then(|value| tool_id_to_index.get(value).copied())
                    .unwrap_or(tool_index.saturating_sub(1));
                out.push(events::tool_call_arg_delta(
                    id.unwrap_or_else(|| "tool".to_string()),
                    args_delta,
                    index,
                ));
            }
            CloudChatEvent::Finish { reason } => finish_reason = reason,
            CloudChatEvent::Usage {
                prompt_tokens,
                completion_tokens,
                ..
            } => out.push(events::usage(prompt_tokens, completion_tokens, None, None)),
        }
    }

    let stop_reason = match finish_reason {
        FinishReason::ToolCalls => "tool_calls",
        FinishReason::Length => "length",
        FinishReason::ContentFilter => "content_filter",
        FinishReason::Stop if saw_tool_call => "tool_calls",
        FinishReason::Stop => "stop",
    };
    out.push(events::stop(stop_reason));
    out
}

covalt_provider::main!(WindsurfProvider);

use std::collections::HashMap;

use async_trait::async_trait;
use covalt_provider::{
    method::Method, Auth, Model, Provider, ProviderContext, ProviderError, Transport,
};
use serde_json::{json, Value};
use xai_provider::auth;
use xai_provider::models::{
    self, cli_proxy_headers, is_cli_proxy_model, reasoning_effort, supports_reasoning, CLI_PROXY_BASE,
    XAI_API_BASE,
};

struct XaiOAuthProvider;

#[async_trait]
impl Provider for XaiOAuthProvider {
    fn id(&self) -> &str {
        "xai-oauth"
    }

    fn name(&self) -> &str {
        "xAI (Grok)"
    }

    fn implements(&self) -> &'static [Method] {
        &[
            Method::ModelsList,
            Method::Transport,
            Method::Prepare,
            Method::Login,
            Method::Refresh,
        ]
    }

    async fn models(&self, ctx: &ProviderContext) -> Result<Vec<Model>, ProviderError> {
        let Some(token) = access_token(ctx) else {
            return Ok(Vec::new());
        };
        models::list_models(&token).await
    }

    async fn transport(
        &self,
        _ctx: &ProviderContext,
        model: &str,
    ) -> Result<Transport, ProviderError> {
        if is_cli_proxy_model(model) {
            return Ok(Transport {
                dialect: "openai-responses".into(),
                base_url: CLI_PROXY_BASE.into(),
                headers: cli_proxy_headers(model),
            });
        }
        Ok(Transport {
            dialect: "openai-responses".into(),
            base_url: XAI_API_BASE.into(),
            headers: HashMap::from([("Accept".into(), "application/json".into())]),
        })
    }

    async fn prepare(&self, ctx: &ProviderContext, mut req: Value) -> Result<Value, ProviderError> {
        let model = req
            .get("model")
            .and_then(Value::as_str)
            .or(ctx.model.as_deref())
            .unwrap_or("")
            .to_string();
        let effort = reasoning_effort(&req);
        if effort.as_deref() == Some("none") || effort.is_none() || !supports_reasoning(&model) {
            return Ok(req);
        }
        let effort = effort.expect("checked above");
        let obj = req
            .as_object_mut()
            .ok_or_else(|| ProviderError::Message("prepare request must be an object".into()))?;
        let body = obj
            .entry("body")
            .or_insert_with(|| json!({}));
        if let Some(body_obj) = body.as_object_mut() {
            body_obj.insert("reasoning_effort".into(), json!(effort));
        }
        Ok(req)
    }

    async fn login(&self, ctx: &ProviderContext) -> Result<Auth, ProviderError> {
        auth::login(ctx).await
    }

    async fn refresh(&self, ctx: &ProviderContext, auth: &Auth) -> Result<Auth, ProviderError> {
        auth::refresh(ctx, auth).await
    }
}

fn access_token(ctx: &ProviderContext) -> Option<String> {
    ctx.auth
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            ctx.auth
                .api_key
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
}

covalt_provider::main!(XaiOAuthProvider);
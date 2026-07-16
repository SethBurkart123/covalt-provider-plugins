use std::collections::HashMap;

use async_trait::async_trait;
use claude_oauth_provider::{auth, prepare};
use covalt_provider::{
    method::Method, Auth, Provider, ProviderContext, ProviderError, Transport,
};
use serde_json::Value;

struct ClaudeOAuthProvider;

#[async_trait]
impl Provider for ClaudeOAuthProvider {
    fn id(&self) -> &str {
        "anthropic-oauth"
    }

    fn name(&self) -> &str {
        "Claude OAuth"
    }

    fn implements(&self) -> &'static [Method] {
        &[
            Method::Transport,
            Method::Prepare,
            Method::Login,
            Method::Refresh,
        ]
    }

    async fn transport(
        &self,
        _ctx: &ProviderContext,
        _model: &str,
    ) -> Result<Transport, ProviderError> {
        Ok(Transport {
            dialect: "anthropic-messages".into(),
            base_url: "https://api.anthropic.com".into(),
            headers: HashMap::from([
                ("anthropic-version".into(), "2023-06-01".into()),
                (
                    "anthropic-beta".into(),
                    "claude-code-20250219,oauth-2025-04-20,fine-grained-tool-streaming-2025-05-14,interleaved-thinking-2025-05-14".into(),
                ),
                ("content-type".into(), "application/json".into()),
                ("accept".into(), "application/json".into()),
                (
                    "user-agent".into(),
                    "claude-cli/2.1.2 (external, cli)".into(),
                ),
                ("x-app".into(), "cli".into()),
            ]),
        })
    }

    async fn prepare(&self, _ctx: &ProviderContext, req: Value) -> Result<Value, ProviderError> {
        Ok(prepare::prepare(req))
    }

    async fn login(&self, ctx: &ProviderContext) -> Result<Auth, ProviderError> {
        auth::login(ctx).await
    }

    async fn refresh(&self, ctx: &ProviderContext, auth: &Auth) -> Result<Auth, ProviderError> {
        auth::refresh(ctx, auth).await
    }
}

covalt_provider::main!(ClaudeOAuthProvider);

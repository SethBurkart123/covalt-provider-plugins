use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use covalt_provider::{Auth, ProviderContext, ProviderError};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const OAUTH_DISCOVERY_URL: &str = "https://auth.x.ai/.well-known/openid-configuration";
const OAUTH_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const OAUTH_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const OAUTH_REFRESH_SKEW_SEC: i64 = 120;

#[derive(Debug, Deserialize)]
struct OAuthDiscovery {
    authorization_endpoint: String,
    token_endpoint: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
}

pub async fn login(ctx: &ProviderContext) -> Result<Auth, ProviderError> {
    let discovery = oauth_discovery().await?;
    let (verifier, challenge) = pkce();
    let state = Uuid::new_v4().simple().to_string();
    let nonce = Uuid::new_v4().simple().to_string();
    let redirect_uri = callback_uri(ctx)?;

    let auth_url = format!(
        "{}?{}",
        discovery.authorization_endpoint,
        encode_query(&[
            ("response_type", "code"),
            ("client_id", OAUTH_CLIENT_ID),
            ("redirect_uri", redirect_uri.as_str()),
            ("scope", OAUTH_SCOPE),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", state.as_str()),
            ("nonce", nonce.as_str()),
        ])
    );

    let mut show = Map::new();
    show.insert("note".into(), json!("Sign in with your xAI / Grok account"));
    show.insert("url".into(), json!(auth_url));
    ctx.ui().show(show).await?;

    let params = ctx.ui().browser(&auth_url).await?;
    let code = params
        .get("code")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ProviderError::Message("xAI authorization failed: no authorization code returned".into()))?;

    if let Some(error) = params.get("error").and_then(Value::as_str) {
        let description = params
            .get("error_description")
            .and_then(Value::as_str)
            .unwrap_or(error);
        return Err(ProviderError::Message(format!(
            "xAI authorization failed: {description}"
        )));
    }

    if let Some(callback_state) = params.get("state").and_then(Value::as_str) {
        if !callback_state.is_empty() && callback_state != state {
            return Err(ProviderError::Message(
                "xAI authorization failed: state mismatch".into(),
            ));
        }
    }

    let payload = exchange_token(
        &discovery.token_endpoint,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri.as_str()),
            ("client_id", OAUTH_CLIENT_ID),
            ("code_verifier", verifier.as_str()),
        ],
    )
    .await?;

    Ok(auth_from_token(payload, &discovery.token_endpoint))
}

pub async fn refresh(_ctx: &ProviderContext, auth: &Auth) -> Result<Auth, ProviderError> {
    let refresh_token = auth
        .refresh
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ProviderError::Message("Missing refresh token".into()))?;

    let token_endpoint = auth
        .extra
        .as_ref()
        .and_then(|extra| {
            extra
                .get("tokenEndpoint")
                .or_else(|| extra.get("token_endpoint"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            // Discovery is async; block is avoided by requiring stored endpoint after login.
            String::new()
        });

    let token_endpoint = if token_endpoint.is_empty() {
        oauth_discovery().await?.token_endpoint
    } else {
        validate_xai_url(&token_endpoint)?
    };

    let payload = exchange_token(
        &token_endpoint,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", OAUTH_CLIENT_ID),
        ],
    )
    .await?;

    Ok(auth_from_token(payload, &token_endpoint))
}

async fn oauth_discovery() -> Result<OAuthDiscovery, ProviderError> {
    let client = Client::new();
    let resp = client
        .get(OAUTH_DISCOVERY_URL)
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
            "xAI OAuth discovery failed: {status} {text}"
        )));
    }
    let discovery: OAuthDiscovery = serde_json::from_str(&text)
        .map_err(|err| ProviderError::Message(format!("xAI OAuth discovery invalid JSON: {err}")))?;
    Ok(OAuthDiscovery {
        authorization_endpoint: validate_xai_url(&discovery.authorization_endpoint)?,
        token_endpoint: validate_xai_url(&discovery.token_endpoint)?,
    })
}

async fn exchange_token(endpoint: &str, body: &[(&str, &str)]) -> Result<TokenResponse, ProviderError> {
    let client = Client::new();
    let resp = client
        .post(endpoint)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(body)
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
            "xAI token request failed: {status} {text}"
        )));
    }
    serde_json::from_str(&text)
        .map_err(|err| ProviderError::Message(format!("xAI token response invalid JSON: {err}")))
}

fn auth_from_token(payload: TokenResponse, token_endpoint: &str) -> Auth {
    let mut expires_in = payload.expires_in - OAUTH_REFRESH_SKEW_SEC;
    if expires_in < 1 {
        expires_in = 1;
    }
    Auth {
        access: payload.access_token,
        refresh: Some(payload.refresh_token),
        expires_in: Some(expires_in),
        keep_fresh: None,
        extra: Some(json!({ "tokenEndpoint": token_endpoint })),
    }
}

fn callback_uri(ctx: &ProviderContext) -> Result<String, ProviderError> {
    // xAI's Grok CLI OAuth client only registers loopback URIs ending in /callback.
    let port = callback_port(ctx);
    Ok(format!("http://127.0.0.1:{port}/callback"))
}

fn callback_port(ctx: &ProviderContext) -> u16 {
    if let Some(uri) = ctx.callback_uri.as_deref().map(str::trim).filter(|value| !value.is_empty())
    {
        for prefix in ["http://127.0.0.1:", "http://localhost:"] {
            if let Some(rest) = uri.strip_prefix(prefix) {
                if let Ok(port) = rest.split('/').next().unwrap_or_default().parse::<u16>() {
                    return port;
                }
            }
        }
    }
    ctx.callback_port.unwrap_or(56_121) as u16
}

fn pkce() -> (String, String) {
    let verifier = URL_SAFE_NO_PAD.encode(rand_bytes(32));
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

fn rand_bytes(len: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; len];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
    bytes
}

fn validate_xai_url(url: &str) -> Result<String, ProviderError> {
    if !url.starts_with("https://") {
        return Err(ProviderError::Message(format!(
            "unexpected xAI OAuth endpoint: {url}"
        )));
    }
    let host = url
        .split('/')
        .nth(2)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if host != "x.ai" && !host.ends_with(".x.ai") {
        return Err(ProviderError::Message(format!(
            "unexpected xAI OAuth endpoint: {url}"
        )));
    }
    Ok(url.to_string())
}

fn encode_query(params: &[(&str, &str)]) -> String {
    params
        .iter()
        .map(|(key, value)| format!("{key}={}", percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}
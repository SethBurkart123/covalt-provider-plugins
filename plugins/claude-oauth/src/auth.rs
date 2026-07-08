use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use covalt_provider::{Auth, ProviderContext, ProviderError};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Map};
use sha2::{Digest, Sha256};

const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
const SCOPE: &str = "org:create_api_key user:profile user:inference";
const OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: i64,
}

pub async fn login(ctx: &ProviderContext) -> Result<Auth, ProviderError> {
    let (verifier, challenge) = pkce();
    let auth_url = format!(
        "{}?{}",
        AUTHORIZE_URL,
        encode_query(&[
            ("code", "true"),
            ("client_id", OAUTH_CLIENT_ID),
            ("response_type", "code"),
            ("redirect_uri", REDIRECT_URI),
            ("scope", SCOPE),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", verifier.as_str()),
        ])
    );

    let mut show = Map::new();
    show.insert("note".into(), json!("Paste the authorization code"));
    show.insert("url".into(), json!(auth_url));
    ctx.ui().show(show).await?;

    let code = ctx
        .ui()
        .prompt("Paste the authorization code", Some(&auth_url))
        .await?;
    let payload = exchange_code(&code, &verifier).await?;
    auth_from_login(payload)
}

pub async fn refresh(_ctx: &ProviderContext, auth: &Auth) -> Result<Auth, ProviderError> {
    let refresh_token = auth
        .refresh
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ProviderError::Message("Missing refresh token".into()))?;

    let payload = refresh_tokens(refresh_token).await?;
    Ok(Auth {
        access: payload.access_token,
        refresh: Some(
            payload
                .refresh_token
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| refresh_token.to_string()),
        ),
        expires_in: Some(payload.expires_in),
        keep_fresh: None,
        extra: None,
    })
}

async fn exchange_code(code: &str, verifier: &str) -> Result<TokenResponse, ProviderError> {
    let client = Client::new();
    let resp = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/json")
        .json(&json!({
            "grant_type": "authorization_code",
            "client_id": OAUTH_CLIENT_ID,
            "code": code.trim(),
            "state": verifier,
            "redirect_uri": REDIRECT_URI,
            "code_verifier": verifier,
        }))
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|err| ProviderError::Message(err.to_string()))?;
    parse_token_response(resp).await
}

async fn refresh_tokens(refresh_token: &str) -> Result<TokenResponse, ProviderError> {
    let client = Client::new();
    let resp = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/json")
        .json(&json!({
            "grant_type": "refresh_token",
            "client_id": OAUTH_CLIENT_ID,
            "refresh_token": refresh_token,
        }))
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|err| ProviderError::Message(err.to_string()))?;
    parse_token_response(resp).await
}

async fn parse_token_response(resp: reqwest::Response) -> Result<TokenResponse, ProviderError> {
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|err| ProviderError::Message(err.to_string()))?;
    if !status.is_success() {
        return Err(ProviderError::Message(text));
    }
    let payload: TokenResponse = serde_json::from_str(&text)
        .map_err(|err| ProviderError::Message(format!("Invalid token response: {err}")))?;
    if payload.access_token.is_empty() || payload.expires_in <= 0 {
        return Err(ProviderError::Message("Invalid token response".into()));
    }
    Ok(payload)
}

fn auth_from_login(payload: TokenResponse) -> Result<Auth, ProviderError> {
    let refresh = payload
        .refresh_token
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ProviderError::Message("Invalid token response".into()))?;
    Ok(Auth {
        access: payload.access_token,
        refresh: Some(refresh),
        expires_in: Some(payload.expires_in),
        keep_fresh: None,
        extra: None,
    })
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

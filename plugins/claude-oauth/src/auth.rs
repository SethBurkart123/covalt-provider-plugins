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
    show.insert(
        "note".into(),
        json!("Paste the authorization code (CODE#STATE from the callback page)"),
    );
    show.insert("url".into(), json!(auth_url));
    ctx.ui().show(show).await?;

    let pasted = ctx
        .ui()
        .prompt("Paste the authorization code", Some(&auth_url))
        .await?;
    let (code, state) = parse_authorization_input(&pasted, &verifier)?;
    let payload = exchange_code(&code, &state, &verifier).await?;
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

async fn exchange_code(
    code: &str,
    state: &str,
    verifier: &str,
) -> Result<TokenResponse, ProviderError> {
    let client = Client::new();
    let resp = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/json")
        .json(&json!({
            "grant_type": "authorization_code",
            "client_id": OAUTH_CLIENT_ID,
            "code": code,
            "state": state,
            "redirect_uri": REDIRECT_URI,
            "code_verifier": verifier,
        }))
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|err| ProviderError::Message(err.to_string()))?;
    parse_token_response(resp).await
}

fn parse_authorization_input(
    pasted: &str,
    verifier: &str,
) -> Result<(String, String), ProviderError> {
    let trimmed = pasted.trim();
    if trimmed.is_empty() {
        return Err(ProviderError::Message("Missing authorization code".into()));
    }

    let (code, state) = if let Some(query) = trimmed.split('?').nth(1) {
        let (code, state) = parse_query_params(query);
        if code.is_some() {
            (code, state)
        } else {
            split_code_state(trimmed)
        }
    } else if trimmed.contains('=') {
        let (code, state) = parse_query_params(trimmed);
        if code.is_some() {
            (code, state)
        } else {
            split_code_state(trimmed)
        }
    } else {
        split_code_state(trimmed)
    };

    let code = code.filter(|value| !value.is_empty()).ok_or_else(|| {
        ProviderError::Message(
            "Missing authorization code — paste the full CODE#STATE from the callback page".into(),
        )
    })?;
    let state = state
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| verifier.to_string());
    if state != verifier {
        return Err(ProviderError::Message(
            "OAuth state mismatch — start sign-in again and paste a fresh code".into(),
        ));
    }
    Ok((code, state))
}

fn split_code_state(input: &str) -> (Option<String>, Option<String>) {
    if let Some((code_part, state_part)) = input.split_once('#') {
        let code = (!code_part.is_empty()).then(|| code_part.to_string());
        let state = (!state_part.is_empty()).then(|| state_part.to_string());
        return (code, state);
    }
    (Some(input.to_string()), None)
}

fn parse_query_params(input: &str) -> (Option<String>, Option<String>) {
    let mut code = None;
    let mut state = None;
    for pair in input.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        match key {
            "code" if !value.is_empty() => code = Some(value.to_string()),
            "state" if !value.is_empty() => state = Some(value.to_string()),
            _ => {}
        }
    }
    (code, state)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_code_and_state_from_hash_format() {
        let (code, state) =
            parse_authorization_input("abc123#verifier-state", "verifier-state").expect("parse");
        assert_eq!(code, "abc123");
        assert_eq!(state, "verifier-state");
    }

    #[test]
    fn accepts_code_only_and_falls_back_to_verifier_state() {
        let (code, state) = parse_authorization_input("abc123", "verifier-state").expect("parse");
        assert_eq!(code, "abc123");
        assert_eq!(state, "verifier-state");
    }

    #[test]
    fn rejects_state_mismatch() {
        let err = parse_authorization_input("abc123#wrong-state", "verifier-state")
            .expect_err("mismatch");
        assert!(err.to_string().contains("state mismatch"));
    }

    #[test]
    fn parses_code_from_query_string() {
        let (code, state) = parse_authorization_input(
            "https://console.anthropic.com/oauth/code/callback?code=abc123&state=verifier-state",
            "verifier-state",
        )
        .expect("parse");
        assert_eq!(code, "abc123");
        assert_eq!(state, "verifier-state");
    }
}

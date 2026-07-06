use covalt_provider::Auth;
use covalt_provider::ProviderContext;
use covalt_provider::ProviderError;
use reqwest::Client;
use serde::Deserialize;

const OAUTH_CLIENT_ID: &str = "3GUryQ7ldAeKEuD2obYnppsnmj58eP5u";
const REGISTER_URL: &str =
    "https://register.windsurf.com/exa.seat_management_pb.SeatManagementService/RegisterUser";

#[derive(Debug, Deserialize)]
struct RegisterUserResponse {
    api_key: Option<String>,
    name: Option<String>,
    api_server_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConnectError {
    code: Option<String>,
    message: Option<String>,
}

pub async fn login(ctx: &ProviderContext) -> Result<Auth, ProviderError> {
    let callback_uri = ctx
        .callback_uri
        .as_deref()
        .ok_or_else(|| ProviderError::Message("login requires host callback URI".into()))?;
    let expected_state = ctx
        .options
        .get("oauthState")
        .or_else(|| ctx.options.get("oauth_state"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let url = build_login_url(callback_uri, expected_state.as_deref().unwrap_or("pending"));
    let params = ctx.ui().browser(&url).await?;
    let token = extract_firebase_token(&params)?;
    if let Some(expected_state) = expected_state.as_deref() {
        if params
            .get("state")
            .and_then(|value| value.as_str())
            .is_some_and(|value| value != expected_state)
        {
            return Err(ProviderError::Message("OAuth state mismatch".into()));
        }
    }
    let registered = register_user(&token).await?;
    Ok(Auth {
        access: registered.api_key,
        refresh: None,
        expires_in: None,
        keep_fresh: None,
        extra: Some(serde_json::json!({ "apiServerUrl": registered.api_server_url })),
    })
}

fn build_login_url(redirect_uri: &str, state: &str) -> String {
    let query = [
        ("response_type", "token"),
        ("client_id", OAUTH_CLIENT_ID),
        ("redirect_uri", redirect_uri),
        ("state", state),
        ("prompt", "login"),
        ("redirect_parameters_type", "query"),
    ]
    .into_iter()
    .map(|(key, value)| format!("{key}={}", urlencoding_encode(value)))
    .collect::<Vec<_>>()
    .join("&");
    format!("https://windsurf.com/windsurf/signin?{query}")
}

fn urlencoding_encode(value: &str) -> String {
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

fn extract_firebase_token(
    params: &serde_json::Map<String, serde_json::Value>,
) -> Result<String, ProviderError> {
    for key in ["firebase_id_token", "access_token", "token"] {
        if let Some(value) = params.get(key).and_then(|value| value.as_str()) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_string());
            }
        }
    }
    Err(ProviderError::Message(
        "OAuth callback missing firebase_id_token".into(),
    ))
}

struct RegisteredUser {
    api_key: String,
    api_server_url: String,
}

async fn register_user(firebase_id_token: &str) -> Result<RegisteredUser, ProviderError> {
    let client = Client::new();
    let resp = client
        .post(REGISTER_URL)
        .header("Content-Type", "application/json")
        .header("Connect-Protocol-Version", "1")
        .json(&serde_json::json!({ "firebase_id_token": firebase_id_token }))
        .send()
        .await
        .map_err(|err| ProviderError::Message(err.to_string()))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|err| ProviderError::Message(err.to_string()))?;
    if !status.is_success() {
        let message = serde_json::from_str::<ConnectError>(&text)
            .ok()
            .and_then(|err| err.message)
            .unwrap_or(text);
        return Err(ProviderError::Message(format!(
            "RegisterUser failed ({status}): {message}"
        )));
    }
    let parsed: RegisterUserResponse = serde_json::from_str(&text)
        .map_err(|err| ProviderError::Message(format!("RegisterUser invalid JSON: {err}")))?;
    let api_key = parsed
        .api_key
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ProviderError::Message("RegisterUser returned empty api_key".into()))?;
    let api_server_url = parsed
        .api_server_url
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "https://server.codeium.com".to_string());
    Ok(RegisteredUser {
        api_key,
        api_server_url,
    })
}

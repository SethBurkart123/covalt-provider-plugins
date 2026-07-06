use std::sync::{LazyLock, Mutex};

use reqwest::Client;

use crate::cloud::metadata::{build_metadata, MetadataInput};
use crate::cloud::wire::{encode_message, iter_fields, FieldValue};

const DEFAULT_HOST: &str = "https://server.codeium.com";
const DEFAULT_INFERENCE_HOST: &str = "https://inference.codeium.com";

#[derive(Debug, Clone)]
pub struct MintedUserJwt {
    pub jwt: String,
    pub expires_at: i64,
}

struct CacheEntry {
    jwt: String,
    expires_at: i64,
    api_key: String,
    host: String,
}

static JWT_CACHE: LazyLock<Mutex<Option<CacheEntry>>> = LazyLock::new(|| Mutex::new(None));

pub fn clear_cached_user_jwt() {
    *JWT_CACHE.lock().expect("jwt cache") = None;
}

pub async fn get_cached_user_jwt(api_key: &str, host: &str) -> Result<String, String> {
    let now = unix_now();
    if let Some(entry) = JWT_CACHE.lock().expect("jwt cache").as_ref() {
        if entry.api_key == api_key && entry.host == host && entry.expires_at > now + 60 {
            return Ok(entry.jwt.clone());
        }
    }

    let minted = mint_user_jwt(api_key, host).await?;
    *JWT_CACHE.lock().expect("jwt cache") = Some(CacheEntry {
        jwt: minted.jwt.clone(),
        expires_at: minted.expires_at,
        api_key: api_key.to_string(),
        host: host.to_string(),
    });
    Ok(minted.jwt)
}

pub async fn mint_user_jwt(api_key: &str, host: &str) -> Result<MintedUserJwt, String> {
    let host = normalize_host(host);
    let session_id = uuid::Uuid::new_v4().to_string();
    let trigger_id = uuid::Uuid::new_v4().to_string();
    let metadata = build_metadata(&MetadataInput {
        api_key,
        user_jwt: None,
        session_id: &session_id,
        request_id: unix_millis(),
        trigger_id: &trigger_id,
    });
    let body = encode_message(1, &metadata);
    let client = Client::new();
    let resp = client
        .post(format!("{host}/exa.auth_pb.AuthService/GetUserJwt"))
        .header("Content-Type", "application/proto")
        .header("Connect-Protocol-Version", "1")
        .body(body)
        .send()
        .await
        .map_err(|err| err.to_string())?;
    let status = resp.status();
    let buf = resp.bytes().await.map_err(|err| err.to_string())?;
    if !status.is_success() {
        return Err(format!(
            "GetUserJwt HTTP {}: {}",
            status,
            String::from_utf8_lossy(&buf[..buf.len().min(400)])
        ));
    }

    let mut jwt = None;
    for field in iter_fields(&buf) {
        if field.num == 1 {
            if let FieldValue::Bytes(bytes) = field.value {
                let candidate = String::from_utf8_lossy(&bytes).to_string();
                if candidate.starts_with("eyJ") {
                    jwt = Some(candidate);
                    break;
                }
            }
        }
    }
    let jwt = jwt.ok_or_else(|| "GetUserJwt returned no JWT".to_string())?;
    let expires_at = decode_jwt_exp(&jwt).unwrap_or(unix_now() + 600);
    Ok(MintedUserJwt { jwt, expires_at })
}

fn decode_jwt_exp(jwt: &str) -> Option<i64> {
    let payload = jwt.split('.').nth(1)?;
    let mut padded = payload.to_string();
    while padded.len() % 4 != 0 {
        padded.push('=');
    }
    let decoded = base64_decode(&padded)?;
    let value: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    value.get("exp").and_then(|exp| exp.as_i64())
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    const TABLE: [i8; 256] = {
        let mut table = [-1i8; 256];
        let mut i = 0u8;
        while i < 26 {
            table[b'A' as usize + i as usize] = i as i8;
            table[b'a' as usize + i as usize] = (i + 26) as i8;
            i += 1;
        }
        let mut i = 0u8;
        while i < 10 {
            table[b'0' as usize + i as usize] = (i + 52) as i8;
            i += 1;
        }
        table[b'+' as usize] = 62;
        table[b'/' as usize] = 63;
        table
    };
    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0u32;
    for &byte in bytes {
        if byte == b'=' {
            break;
        }
        let val = TABLE[byte as usize];
        if val < 0 {
            continue;
        }
        buf = (buf << 6) | val as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn normalize_host(host: &str) -> String {
    host.trim_end_matches('/').to_string()
}

pub fn default_host() -> &'static str {
    DEFAULT_HOST
}

pub fn inference_host_for(api_server_url: &str) -> String {
    let api_host = normalize_host(if api_server_url.is_empty() {
        DEFAULT_HOST
    } else {
        api_server_url
    });
    if api_host == DEFAULT_HOST {
        DEFAULT_INFERENCE_HOST.to_string()
    } else {
        api_host
    }
}

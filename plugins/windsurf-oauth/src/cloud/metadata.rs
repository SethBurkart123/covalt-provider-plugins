use crate::cloud::wire::{
    encode_message, encode_string, encode_timestamp_body, encode_varint_field,
};

const WINDSURF_VERSION: &str = "2.0.0";

pub struct MetadataInput<'a> {
    pub api_key: &'a str,
    pub user_jwt: Option<&'a str>,
    pub session_id: &'a str,
    pub request_id: u64,
    pub trigger_id: &'a str,
}

fn os_string() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    }
}

pub fn build_metadata(input: &MetadataInput<'_>) -> Vec<u8> {
    let mut parts = vec![
        encode_string(1, "windsurf"),
        encode_string(2, WINDSURF_VERSION),
        encode_string(3, input.api_key),
        encode_string(4, "en"),
        encode_string(5, os_string()),
        encode_string(7, WINDSURF_VERSION),
        encode_varint_field(9, input.request_id),
        encode_string(10, input.session_id),
        encode_string(12, "windsurf"),
        encode_message(16, &encode_timestamp_body()),
        encode_string(25, input.trigger_id),
        encode_string(26, "Unset"),
        encode_string(28, "windsurf"),
    ];
    if let Some(jwt) = input.user_jwt {
        parts.push(encode_string(21, jwt));
    }
    parts.into_iter().flatten().collect()
}

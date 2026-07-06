use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use futures::StreamExt;
use reqwest::Client;

use crate::cloud::auth::{get_cached_user_jwt, inference_host_for, normalize_host};
use crate::cloud::metadata::{build_metadata, MetadataInput};
use crate::cloud::wire::{
    encode_fixed64_field, encode_message, encode_string, encode_varint_field, frame_connect_stream,
    iter_fields, parse_connect_frames, FieldValue,
};

const MAX_TOOL_DESC_LEN: usize = 6998;

#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone)]
pub enum ContentPart {
    Text {
        text: String,
    },
    Image {
        mime_type: String,
        base64_data: String,
    },
}

#[derive(Debug, Clone)]
pub struct ChatHistoryItem {
    pub role: String,
    pub content: Vec<ContentPart>,
    pub tool_call_id: Option<String>,
    pub tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone)]
pub enum CloudChatEvent {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallArgs {
        args_delta: String,
        id: Option<String>,
    },
    Finish {
        reason: FinishReason,
    },
    Usage {
        prompt_tokens: Option<u64>,
        completion_tokens: Option<u64>,
        total_tokens: Option<u64>,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    ContentFilter,
}

pub struct CloudChatRequest {
    pub api_key: String,
    pub api_server_url: String,
    pub inference_server_url: String,
    pub model_uid: String,
    pub messages: Vec<ChatHistoryItem>,
    pub tools: Vec<ToolDef>,
    pub max_output_tokens: u64,
}

#[derive(Clone)]
struct SessionIds {
    session_id: String,
    cascade_id: String,
}

static SESSION_CACHE: LazyLock<Mutex<HashMap<String, SessionIds>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub async fn stream_chat_events(req: CloudChatRequest) -> Result<Vec<CloudChatEvent>, String> {
    let api_host = normalize_host(if req.api_server_url.is_empty() {
        crate::cloud::auth::default_host()
    } else {
        &req.api_server_url
    });
    let inference_server_url = if req.inference_server_url.is_empty() {
        inference_host_for(&api_host)
    } else {
        req.inference_server_url.clone()
    };
    let inference_host = normalize_host(&inference_server_url);
    let user_jwt = get_cached_user_jwt(&req.api_key, &api_host).await?;
    let cache_key = format!("{inference_host}\x1f{}", req.api_key);
    let session_ids = {
        let mut cache = SESSION_CACHE.lock().expect("session cache");
        cache
            .entry(cache_key)
            .or_insert_with(|| SessionIds {
                session_id: uuid::Uuid::new_v4().to_string(),
                cascade_id: uuid::Uuid::new_v4().to_string(),
            })
            .clone()
    };

    let proto = build_get_chat_message_request(BuildArgs {
        api_key: &req.api_key,
        user_jwt: &user_jwt,
        model_uid: &req.model_uid,
        messages: &req.messages,
        tools: &req.tools,
        cascade_id: &session_ids.cascade_id,
        prompt_id: &uuid::Uuid::new_v4().to_string(),
        session_id: &session_ids.session_id,
        request_id: unix_millis(),
        trigger_id: &uuid::Uuid::new_v4().to_string(),
        max_output_tokens: req.max_output_tokens,
    });
    let body = frame_connect_stream(&proto, true);
    let client = Client::new();
    let resp = client
        .post(format!(
            "{inference_host}/exa.api_server_pb.ApiServerService/GetChatMessage"
        ))
        .header("Content-Type", "application/connect+proto")
        .header("Connect-Protocol-Version", "1")
        .header("Connect-Content-Encoding", "gzip")
        .header("Connect-Accept-Encoding", "gzip")
        .body(body)
        .send()
        .await
        .map_err(|err| err.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("GetChatMessage HTTP {status}: {text}"));
    }

    let mut events = Vec::new();
    let mut buffer = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|err| err.to_string())?;
        buffer.extend_from_slice(&chunk);
        while buffer.len() >= 5 {
            let len = u32::from_be_bytes([buffer[1], buffer[2], buffer[3], buffer[4]]) as usize;
            if buffer.len() < 5 + len {
                break;
            }
            let frame_bytes = buffer.drain(..5 + len).collect::<Vec<_>>();
            for frame in parse_connect_frames(&frame_bytes) {
                if frame.eos {
                    if !frame.payload.is_empty() {
                        if let Ok(value) =
                            serde_json::from_slice::<serde_json::Value>(&frame.payload)
                        {
                            if let Some(message) = value
                                .get("error")
                                .and_then(|err| err.get("message"))
                                .and_then(|msg| msg.as_str())
                            {
                                return Err(message.to_string());
                            }
                        }
                    }
                    continue;
                }
                events.extend(decode_chat_frame(&frame.payload));
            }
        }
    }
    Ok(events)
}

struct BuildArgs<'a> {
    api_key: &'a str,
    user_jwt: &'a str,
    model_uid: &'a str,
    messages: &'a [ChatHistoryItem],
    tools: &'a [ToolDef],
    cascade_id: &'a str,
    prompt_id: &'a str,
    session_id: &'a str,
    request_id: u64,
    trigger_id: &'a str,
    max_output_tokens: u64,
}

fn build_get_chat_message_request(args: BuildArgs<'_>) -> Vec<u8> {
    let metadata = build_metadata(&MetadataInput {
        api_key: args.api_key,
        user_jwt: Some(args.user_jwt),
        session_id: args.session_id,
        request_id: args.request_id,
        trigger_id: args.trigger_id,
    });
    let collapsed = collapse_system_into_user(args.messages);
    let mut parts = vec![encode_message(1, &metadata)];
    for message in collapsed {
        parts.push(encode_message(3, &encode_chat_message_prompt(&message)));
    }
    parts.push(encode_varint_field(7, 5));
    parts.push(encode_message(
        8,
        &encode_completion_configuration(args.max_output_tokens),
    ));
    for tool in args.tools {
        parts.push(encode_message(10, &encode_tool_def(tool)));
    }
    parts.push(encode_string(16, args.cascade_id));
    parts.push(encode_string(21, args.model_uid));
    parts.push(encode_string(22, args.prompt_id));
    parts.into_iter().flatten().collect()
}

fn encode_completion_configuration(max_output_tokens: u64) -> Vec<u8> {
    [
        encode_varint_field(1, 1),
        encode_varint_field(2, 64_000),
        encode_varint_field(3, max_output_tokens),
        encode_fixed64_field(5, 0.7),
        encode_fixed64_field(6, 0.95),
        encode_varint_field(7, 50),
        encode_fixed64_field(8, 1.0),
        encode_fixed64_field(11, 1.0),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn encode_tool_def(tool: &ToolDef) -> Vec<u8> {
    let raw_desc = tool.description.as_str();
    let desc = if raw_desc.len() > MAX_TOOL_DESC_LEN {
        format!(
            "{}\n…(truncated for cloud)",
            &raw_desc[..MAX_TOOL_DESC_LEN.saturating_sub(24)]
        )
    } else {
        raw_desc.to_string()
    };
    [
        encode_string(1, &tool.name),
        encode_string(2, &desc),
        encode_string(3, &tool.parameters.to_string()),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn encode_chat_tool_call(tc: &ToolCall) -> Vec<u8> {
    [
        encode_string(1, &tc.id),
        encode_string(2, &tc.name),
        encode_string(3, &tc.arguments),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn encode_image_data(mime_type: &str, base64_data: &str) -> Vec<u8> {
    [encode_string(1, base64_data), encode_string(2, mime_type)]
        .into_iter()
        .flatten()
        .collect()
}

fn encode_chat_message_prompt(message: &ChatHistoryItem) -> Vec<u8> {
    let text = message
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let source = match message.role.as_str() {
        "assistant" => 2,
        "tool" => 4,
        _ => 1,
    };
    let mut parts = vec![
        encode_varint_field(2, source),
        encode_string(3, &text),
        encode_varint_field(4, (text.len().max(1) / 4) as u64),
        encode_varint_field(5, 1),
    ];
    if message.role == "tool" {
        if let Some(id) = &message.tool_call_id {
            parts.push(encode_string(7, id));
        }
    }
    if message.role == "assistant" {
        for tc in &message.tool_calls {
            parts.push(encode_message(6, &encode_chat_tool_call(tc)));
        }
    }
    for part in &message.content {
        if let ContentPart::Image {
            mime_type,
            base64_data,
        } = part
        {
            parts.push(encode_message(
                10,
                &encode_image_data(mime_type, base64_data),
            ));
        }
    }
    parts.into_iter().flatten().collect()
}

fn collapse_system_into_user(messages: &[ChatHistoryItem]) -> Vec<ChatHistoryItem> {
    let mut out = Vec::new();
    let mut pending_system = Vec::new();
    for message in messages {
        if message.role == "system" {
            let text = message
                .content
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() {
                pending_system.push(text);
            }
        } else if message.role == "user" && !pending_system.is_empty() {
            let user_text = message
                .content
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            let images = message
                .content
                .iter()
                .filter(|part| matches!(part, ContentPart::Image { .. }))
                .cloned()
                .collect::<Vec<_>>();
            let wrapped = format!(
                "<system>\n{}\n</system>\n{}",
                pending_system.join("\n\n"),
                user_text
            );
            let mut content = vec![ContentPart::Text { text: wrapped }];
            content.extend(images);
            out.push(ChatHistoryItem {
                role: "user".to_string(),
                content,
                tool_call_id: None,
                tool_calls: Vec::new(),
            });
            pending_system.clear();
        } else {
            out.push(message.clone());
        }
    }
    if !pending_system.is_empty() {
        out.push(ChatHistoryItem {
            role: "user".to_string(),
            content: vec![ContentPart::Text {
                text: format!("<system>\n{}\n</system>", pending_system.join("\n\n")),
            }],
            tool_call_id: None,
            tool_calls: Vec::new(),
        });
    }
    out
}

fn decode_chat_frame(proto: &[u8]) -> Vec<CloudChatEvent> {
    let mut events = Vec::new();
    for field in iter_fields(proto) {
        match (field.num, &field.value) {
            (3, FieldValue::Bytes(bytes)) => {
                let text = String::from_utf8_lossy(bytes).to_string();
                if !text.is_empty() {
                    events.push(CloudChatEvent::Text { text });
                }
            }
            (9, FieldValue::Bytes(bytes)) => {
                let text = String::from_utf8_lossy(bytes).to_string();
                if !text.is_empty() {
                    events.push(CloudChatEvent::Reasoning { text });
                }
            }
            (6, FieldValue::Bytes(bytes)) => {
                let mut id = None;
                let mut name = None;
                let mut args_delta = None;
                for sub in iter_fields(bytes) {
                    if let FieldValue::Bytes(value) = sub.value {
                        let s = String::from_utf8_lossy(&value).to_string();
                        match sub.num {
                            1 => id = Some(s),
                            2 => name = Some(s),
                            3 => args_delta = Some(s),
                            _ => {}
                        }
                    }
                }
                if let (Some(id), Some(name)) = (id, name) {
                    events.push(CloudChatEvent::ToolCallStart { id, name });
                }
                if let Some(args_delta) = args_delta {
                    events.push(CloudChatEvent::ToolCallArgs {
                        args_delta,
                        id: None,
                    });
                }
            }
            (5, FieldValue::Varint(value)) => {
                let reason = match *value {
                    10 => FinishReason::ToolCalls,
                    11 => FinishReason::ContentFilter,
                    1 | 3 => FinishReason::Length,
                    _ => FinishReason::Stop,
                };
                events.push(CloudChatEvent::Finish { reason });
            }
            (28, FieldValue::Bytes(bytes)) => {
                if let Some(usage) = decode_usage_block(bytes) {
                    events.push(usage);
                }
            }
            _ => {}
        }
    }
    events
}

fn decode_usage_block(buf: &[u8]) -> Option<CloudChatEvent> {
    let mut prompt = None;
    let mut completion = None;
    for field in iter_fields(buf) {
        if field.num != 2 {
            continue;
        }
        let FieldValue::Bytes(entry) = field.value else {
            continue;
        };
        let mut metric = None;
        let mut value = None;
        for sub in iter_fields(&entry) {
            match (&sub.value, sub.num) {
                (FieldValue::Bytes(bytes), 5) => {
                    metric = Some(String::from_utf8_lossy(bytes).to_string());
                }
                (FieldValue::Fixed64(raw), 2) => {
                    value = Some(f64::from_le_bytes(*raw).round() as u64);
                }
                _ => {}
            }
        }
        if let (Some(metric), Some(value)) = (metric, value) {
            match metric.as_str() {
                "input_tokens" => prompt = Some(value),
                "output_tokens" => completion = Some(value),
                _ => {}
            }
        }
    }
    if prompt.is_none() && completion.is_none() {
        return None;
    }
    let total = prompt.unwrap_or(0) + completion.unwrap_or(0);
    Some(CloudChatEvent::Usage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: if total > 0 { Some(total) } else { None },
    })
}

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn messages_from_json(value: &serde_json::Value) -> Vec<ChatHistoryItem> {
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    items.iter().filter_map(parse_message_item).collect()
}

fn parse_message_item(value: &serde_json::Value) -> Option<ChatHistoryItem> {
    let obj = value.as_object()?;
    let role = obj.get("role")?.as_str()?.to_string();
    let content = parse_content(obj.get("content")?);
    let tool_call_id = obj
        .get("toolCallId")
        .or_else(|| obj.get("tool_call_id"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let tool_calls = obj
        .get("toolCalls")
        .or_else(|| obj.get("tool_calls"))
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let tc = item.as_object()?;
                    Some(ToolCall {
                        id: tc.get("id")?.as_str()?.to_string(),
                        name: tc
                            .get("name")
                            .or_else(|| tc.get("function").and_then(|f| f.get("name")))
                            .and_then(|v| v.as_str())?
                            .to_string(),
                        arguments: tc
                            .get("arguments")
                            .or_else(|| tc.get("function").and_then(|f| f.get("arguments")))
                            .and_then(|v| v.as_str())
                            .unwrap_or("{}")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(ChatHistoryItem {
        role,
        content,
        tool_call_id,
        tool_calls,
    })
}

fn parse_content(value: &serde_json::Value) -> Vec<ContentPart> {
    match value {
        serde_json::Value::String(text) => vec![ContentPart::Text { text: text.clone() }],
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                let obj = item.as_object()?;
                match obj.get("type")?.as_str()? {
                    "text" => Some(ContentPart::Text {
                        text: obj.get("text")?.as_str()?.to_string(),
                    }),
                    "image" => Some(ContentPart::Image {
                        mime_type: obj
                            .get("mimeType")
                            .or_else(|| obj.get("mime_type"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("image/png")
                            .to_string(),
                        base64_data: obj
                            .get("base64Data")
                            .or_else(|| obj.get("base64_data"))
                            .and_then(|v| v.as_str())?
                            .to_string(),
                    }),
                    "image_url" => {
                        let url = obj
                            .get("imageUrl")
                            .or_else(|| obj.get("image_url"))
                            .and_then(|v| v.as_object())
                            .and_then(|v| v.get("url"))
                            .and_then(|v| v.as_str())?;
                        parse_data_url(url)
                    }
                    _ => None,
                }
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn parse_data_url(url: &str) -> Option<ContentPart> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(";base64,")?;
    Some(ContentPart::Image {
        mime_type: meta.to_string(),
        base64_data: data.to_string(),
    })
}

pub fn tools_from_json(value: &serde_json::Value) -> Vec<ToolDef> {
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let obj = item.as_object()?;
            let function = obj.get("function").and_then(|v| v.as_object());
            let name = function
                .and_then(|f| f.get("name"))
                .or_else(|| obj.get("name"))
                .and_then(|v| v.as_str())?
                .to_string();
            let description = function
                .and_then(|f| f.get("description"))
                .or_else(|| obj.get("description"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let parameters = function
                .and_then(|f| f.get("parameters"))
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            Some(ToolDef {
                name,
                description,
                parameters,
            })
        })
        .collect()
}

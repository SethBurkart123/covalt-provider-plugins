use windsurf_provider::cloud::chat::{
    messages_from_json, stream_chat_events, CloudChatRequest,
};
use windsurf_provider::models::resolve_model;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let model = args.next().unwrap_or_else(|| "swe-1.6".to_string());
    let prompt = args.collect::<Vec<_>>().join(" ");
    let prompt = if prompt.is_empty() {
        "Say hello in one short sentence.".to_string()
    } else {
        prompt
    };

    let (api_key, api_server_url) = load_credentials();

    let resolved = resolve_model(&model, None).expect("resolve model");
    eprintln!("model={model} uid={}", resolved.model_uid);

    let events = stream_chat_events(CloudChatRequest {
        api_key,
        api_server_url,
        model_uid: resolved.model_uid,
        messages: messages_from_json(&serde_json::json!([{
            "role": "user",
            "content": prompt,
        }])),
        tools: Vec::new(),
        max_output_tokens: 4096,
    })
    .await
    .expect("stream chat");

    for event in events {
        match event {
            windsurf_provider::cloud::chat::CloudChatEvent::Text { text } => {
                print!("{text}");
            }
            windsurf_provider::cloud::chat::CloudChatEvent::Reasoning { text } => {
                eprint!("\n[reasoning] {text}\n");
            }
            windsurf_provider::cloud::chat::CloudChatEvent::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
                ..
            } => {
                eprintln!(
                    "\n[usage] in={prompt_tokens:?} out={completion_tokens:?} total={total_tokens:?}"
                );
            }
            _ => {}
        }
    }
    println!();
}

fn load_credentials() -> (String, String) {
    if let Ok(api_key) = std::env::var("WINDSURF_API_KEY") {
        let api_server_url = std::env::var("WINDSURF_API_SERVER_URL")
            .unwrap_or_else(|_| "https://server.codeium.com".to_string());
        return (api_key, api_server_url);
    }
    if let Ok(api_key) = std::env::var("COVALT_WINDSURF_API_KEY") {
        let api_server_url = std::env::var("COVALT_WINDSURF_API_SERVER_URL")
            .unwrap_or_else(|_| "https://server.codeium.com".to_string());
        return (api_key, api_server_url);
    }
    if let Some(path) = opencode_credentials_path() {
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(api_key) = value.get("apiKey").and_then(|v| v.as_str()) {
                    let api_server_url = value
                        .get("apiServerUrl")
                        .and_then(|v| v.as_str())
                        .unwrap_or("https://server.codeium.com")
                        .to_string();
                    eprintln!("loaded credentials from {}", path.display());
                    return (api_key.to_string(), api_server_url);
                }
            }
        }
    }
    panic!(
        "set WINDSURF_API_KEY or sign in via opencode-windsurf-auth (credentials at ~/.config/opencode-windsurf-auth/credentials.json)"
    );
}

fn opencode_credentials_path() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|home| {
        std::path::PathBuf::from(home)
            .join(".config")
            .join("opencode-windsurf-auth")
            .join("credentials.json")
    })
}

use std::collections::{HashMap, HashSet};

use covalt_provider::{Model, Pricing, Tag, TagTone};
use reqwest::Client;

use crate::cloud::auth::{default_host, get_cached_user_jwt, normalize_host};
use crate::cloud::metadata::{build_metadata, MetadataInput};
use crate::cloud::wire::{encode_message, iter_fields, FieldValue};

const CATALOG_ENDPOINTS: &[&str] = &[
    "/exa.api_server_pb.ApiServerService/GetCascadeModelConfigs",
    "/exa.language_server_pb.LanguageServerService/GetCascadeModelConfigs",
];

#[derive(Default)]
struct CatalogConfig {
    label: String,
    model_uid: String,
    credit_multiplier: Option<f32>,
    disabled: bool,
    supports_images: bool,
    is_premium: bool,
    is_recommended: bool,
    is_new: bool,
    max_tokens: Option<i64>,
    promo: Option<PromoStatus>,
    fast: Option<FastStatus>,
    model_info: Option<ModelInfo>,
    model_cost_tier: Option<u64>,
    description: Option<String>,
    model_family: Option<String>,
    dimensions: Vec<ModelDimension>,
    disabled_reason: Option<String>,
}

#[derive(Default)]
struct PromoStatus {
    active: bool,
    end_date: Option<i64>,
    label: Option<String>,
}

#[derive(Default)]
struct FastStatus {
    active: bool,
    tooltip: Option<String>,
}

#[derive(Default)]
struct ModelInfo {
    model_uid: Option<String>,
    max_tokens: Option<i64>,
    model_name: Option<String>,
    max_output_tokens: Option<i64>,
}

#[derive(Default)]
struct ModelDimension {
    label: String,
    value: Option<f32>,
    denominator: Option<String>,
    kind: Option<u64>,
    info: Option<String>,
}

pub async fn list_catalog_models(
    api_key: &str,
    api_server_url: &str,
) -> Result<Vec<Model>, String> {
    let api_host = normalize_host(if api_server_url.is_empty() {
        default_host()
    } else {
        api_server_url
    });
    let user_jwt = get_cached_user_jwt(api_key, &api_host).await?;
    let session_id = uuid::Uuid::new_v4().to_string();
    let trigger_id = uuid::Uuid::new_v4().to_string();
    let metadata = build_metadata(&MetadataInput {
        api_key,
        user_jwt: Some(&user_jwt),
        session_id: &session_id,
        request_id: unix_millis(),
        trigger_id: &trigger_id,
    });
    let body = encode_message(1, &metadata);

    let client = Client::new();
    let mut last_error = None;
    for endpoint in CATALOG_ENDPOINTS {
        match fetch_catalog(&client, &api_host, endpoint, &body).await {
            Ok(models) if !models.is_empty() => return Ok(models),
            Ok(_) => last_error = Some(format!("{endpoint} returned no models")),
            Err(err) => last_error = Some(err),
        }
    }

    Err(last_error.unwrap_or_else(|| "GetCascadeModelConfigs returned no models".to_string()))
}

async fn fetch_catalog(
    client: &Client,
    api_host: &str,
    endpoint: &str,
    body: &[u8],
) -> Result<Vec<Model>, String> {
    let resp = client
        .post(format!("{api_host}{endpoint}"))
        .header("Content-Type", "application/proto")
        .header("Connect-Protocol-Version", "1")
        .body(body.to_vec())
        .send()
        .await
        .map_err(|err| err.to_string())?;
    let status = resp.status();
    let buf = resp.bytes().await.map_err(|err| err.to_string())?;
    if !status.is_success() {
        return Err(format!(
            "GetCascadeModelConfigs HTTP {}: {}",
            status,
            String::from_utf8_lossy(&buf[..buf.len().min(400)])
        ));
    }
    Ok(parse_catalog_response(&buf))
}

fn parse_catalog_response(buf: &[u8]) -> Vec<Model> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    for field in iter_fields(buf) {
        if field.num != 1 {
            continue;
        }
        let FieldValue::Bytes(bytes) = field.value else {
            continue;
        };
        let config = parse_client_model_config(&bytes);
        if config.disabled {
            continue;
        }
        let Some(model) = model_from_config(config) else {
            continue;
        };
        if seen.insert(model.id.clone()) {
            out.push(model);
        }
    }

    out
}

fn parse_client_model_config(buf: &[u8]) -> CatalogConfig {
    let mut config = CatalogConfig::default();
    for field in iter_fields(buf) {
        match (field.num, field.value) {
            (1, FieldValue::Bytes(bytes)) => config.label = string(bytes),
            (3, FieldValue::Fixed32(raw)) => {
                config.credit_multiplier = Some(f32::from_le_bytes(raw));
            }
            (4, FieldValue::Varint(value)) => config.disabled = value != 0,
            (5, FieldValue::Varint(value)) => config.supports_images = value != 0,
            (7, FieldValue::Varint(value)) => config.is_premium = value != 0,
            (11, FieldValue::Varint(value)) => config.is_recommended = value != 0,
            (15, FieldValue::Varint(value)) => config.is_new = value != 0,
            (18, FieldValue::Varint(value)) => config.max_tokens = Some(value as i64),
            (19, FieldValue::Bytes(bytes)) => config.promo = Some(parse_promo_status(&bytes)),
            (21, FieldValue::Bytes(bytes)) => config.fast = Some(parse_fast_status(&bytes)),
            (22, FieldValue::Bytes(bytes)) => config.model_uid = string(bytes),
            (23, FieldValue::Bytes(bytes)) => config.model_info = Some(parse_model_info(&bytes)),
            (24, FieldValue::Varint(value)) => config.model_cost_tier = Some(value),
            (27, FieldValue::Bytes(bytes)) => config.description = Some(string(bytes)),
            (30, FieldValue::Bytes(bytes)) => config.model_family = parse_model_family(&bytes),
            (32, FieldValue::Bytes(bytes)) => {
                config.dimensions.push(parse_model_dimension(&bytes));
            }
            (33, FieldValue::Bytes(bytes)) => {
                config.disabled_reason = parse_disabled_reason(&bytes)
            }
            _ => {}
        }
    }
    config
}

fn model_from_config(config: CatalogConfig) -> Option<Model> {
    let model_uid = if config.model_uid.is_empty() {
        config.model_info.as_ref()?.model_uid.clone()?
    } else {
        config.model_uid.clone()
    };
    let name = if !config.label.is_empty() {
        config.label.clone()
    } else {
        config
            .model_info
            .as_ref()
            .and_then(|info| info.model_name.clone())
            .unwrap_or_else(|| model_uid.clone())
    };
    let context_window = config
        .model_info
        .as_ref()
        .and_then(|info| info.max_tokens)
        .or(config.max_tokens);
    let max_output = config
        .model_info
        .as_ref()
        .and_then(|info| info.max_output_tokens);
    let pricing = pricing_from_dimensions(&config.dimensions);
    let mut details = HashMap::from([("Provider".to_string(), "Cognition (Windsurf)".to_string())]);
    let mut tags = Vec::new();

    if config.is_recommended {
        tags.push(tag("Recommended", TagTone::Positive));
    }
    if config.is_new {
        tags.push(tag("New", TagTone::Positive));
    }
    if let Some(promo) = config.promo.as_ref().filter(|promo| promo.active) {
        tags.push(tag("Promo", TagTone::Positive));
        if let Some(label) = &promo.label {
            details.insert("Promo".to_string(), label.clone());
        }
        if let Some(end_date) = promo.end_date {
            details.insert("Promo ends".to_string(), format_utc_date(end_date));
        }
    }
    if config.model_cost_tier == Some(4) {
        tags.push(tag("Free", TagTone::Positive));
    }
    if config.is_premium {
        tags.push(tag("Premium", TagTone::Warning));
    }
    if config.supports_images {
        tags.push(tag("Images", TagTone::Neutral));
    }
    if let Some(fast) = config.fast.as_ref().filter(|fast| fast.active) {
        tags.push(tag("Fast", TagTone::Positive));
        if let Some(tooltip) = &fast.tooltip {
            details.insert("Fast status".to_string(), tooltip.clone());
        }
    }
    if let Some(tier) = config.model_cost_tier.and_then(cost_tier_label) {
        details.insert("Cost tier".to_string(), tier.to_string());
    }
    if let Some(multiplier) = config.credit_multiplier {
        details.insert("Credit multiplier".to_string(), format_float(multiplier));
    }
    if let Some(family) = config.model_family {
        details.insert("Family".to_string(), family);
    }
    if let Some(reason) = config.disabled_reason {
        details.insert("Unavailable reason".to_string(), reason);
    }
    for dimension in &config.dimensions {
        if dimension.label.is_empty() {
            continue;
        }
        if dimension.kind == Some(1) || dimension.kind == Some(2) {
            let value = dimension
                .value
                .map(format_float)
                .unwrap_or_else(|| "unknown".to_string());
            let suffix = dimension
                .denominator
                .as_ref()
                .map(|value| format!(" / {value}"))
                .unwrap_or_default();
            details.insert(dimension.label.clone(), format!("{value}{suffix}"));
        }
        if let Some(info) = &dimension.info {
            details.insert(format!("{} info", dimension.label), info.clone());
        }
    }

    Some(Model {
        id: model_uid,
        name,
        description: config.description,
        context_window,
        max_output,
        pricing,
        tags,
        details,
        controls: Vec::new(),
    })
}

fn parse_promo_status(buf: &[u8]) -> PromoStatus {
    let mut promo = PromoStatus::default();
    for field in iter_fields(buf) {
        match (field.num, field.value) {
            (1, FieldValue::Varint(value)) => promo.active = value != 0,
            (2, FieldValue::Bytes(bytes)) => promo.end_date = parse_timestamp(&bytes),
            (3, FieldValue::Bytes(bytes)) => promo.label = Some(string(bytes)),
            _ => {}
        }
    }
    promo
}

fn parse_fast_status(buf: &[u8]) -> FastStatus {
    let mut fast = FastStatus::default();
    for field in iter_fields(buf) {
        match (field.num, field.value) {
            (1, FieldValue::Varint(value)) => fast.active = value != 0,
            (2, FieldValue::Bytes(bytes)) => fast.tooltip = Some(string(bytes)),
            _ => {}
        }
    }
    fast
}

fn parse_model_info(buf: &[u8]) -> ModelInfo {
    let mut info = ModelInfo::default();
    for field in iter_fields(buf) {
        match (field.num, field.value) {
            (4, FieldValue::Varint(value)) => info.max_tokens = Some(value as i64),
            (8, FieldValue::Bytes(bytes)) => info.model_name = Some(string(bytes)),
            (13, FieldValue::Varint(value)) => info.max_output_tokens = Some(value as i64),
            (17, FieldValue::Bytes(bytes)) => info.model_uid = Some(string(bytes)),
            _ => {}
        }
    }
    info
}

fn parse_model_family(buf: &[u8]) -> Option<String> {
    for field in iter_fields(buf) {
        if field.num == 1 {
            if let FieldValue::Bytes(bytes) = field.value {
                let family = string(bytes);
                if !family.is_empty() {
                    return Some(family);
                }
            }
        }
    }
    None
}

fn parse_model_dimension(buf: &[u8]) -> ModelDimension {
    let mut dimension = ModelDimension::default();
    for field in iter_fields(buf) {
        match (field.num, field.value) {
            (1, FieldValue::Bytes(bytes)) => dimension.label = string(bytes),
            (2, FieldValue::Fixed32(raw)) => dimension.value = Some(f32::from_le_bytes(raw)),
            (3, FieldValue::Bytes(bytes)) => dimension.denominator = Some(string(bytes)),
            (6, FieldValue::Varint(value)) => dimension.kind = Some(value),
            (7, FieldValue::Bytes(bytes)) => dimension.info = Some(string(bytes)),
            _ => {}
        }
    }
    dimension
}

fn parse_disabled_reason(buf: &[u8]) -> Option<String> {
    let mut short = None;
    let mut description = None;
    for field in iter_fields(buf) {
        match (field.num, field.value) {
            (1, FieldValue::Bytes(bytes)) => short = Some(string(bytes)),
            (2, FieldValue::Bytes(bytes)) => description = Some(string(bytes)),
            _ => {}
        }
    }
    description.or(short)
}

fn parse_timestamp(buf: &[u8]) -> Option<i64> {
    for field in iter_fields(buf) {
        if field.num == 1 {
            if let FieldValue::Varint(value) = field.value {
                return Some(value as i64);
            }
        }
    }
    None
}

fn pricing_from_dimensions(dimensions: &[ModelDimension]) -> Option<Pricing> {
    let mut pricing = Pricing::default();
    let mut denominator = None;
    for dimension in dimensions {
        let Some(value) = dimension.value else {
            continue;
        };
        let label = dimension.label.to_ascii_lowercase();
        if label.contains("cached") {
            pricing.cached_input = Some(value as f64);
        } else if label.contains("input") {
            pricing.input = Some(value as f64);
        } else if label.contains("output") {
            pricing.output = Some(value as f64);
        }
        denominator = denominator.or_else(|| dimension.denominator.clone());
    }
    if pricing.input.is_none() && pricing.output.is_none() && pricing.cached_input.is_none() {
        return None;
    }
    if let Some(value) = denominator {
        pricing.per = value;
    }
    Some(pricing)
}

fn cost_tier_label(value: u64) -> Option<&'static str> {
    match value {
        1 => Some("Low"),
        2 => Some("Medium"),
        3 => Some("High"),
        4 => Some("Free"),
        _ => None,
    }
}

fn tag(label: &str, tone: TagTone) -> Tag {
    Tag {
        label: label.to_string(),
        tone,
    }
}

fn string(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).to_string()
}

fn format_float(value: f32) -> String {
    let value = value as f64;
    if (value.fract()).abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

fn format_utc_date(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

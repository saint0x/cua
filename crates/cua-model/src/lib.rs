use anyhow::{bail, Context};
use base64::Engine;
use cua_core::{
    now_wall_ms, CursorState, FrameEncoding, FrameEnvelope, FramePayload, Rect, SCHEMA_VERSION,
};
use image::{codecs::png::PngEncoder, ImageBuffer, ImageEncoder, Rgba};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, REFERER};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Cursor;
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalCandidate {
    pub provider: String,
    pub model: String,
    pub max_output_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateCheck {
    pub provider: String,
    pub model: String,
    pub available: bool,
    pub input_modalities: Vec<String>,
    pub output_modalities: Vec<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalCase {
    pub id: String,
    pub prompt: String,
    pub expected_action_kind: String,
    pub fixture: Option<EvalFixture>,
    pub oracle: EvalOracle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalFixture {
    pub kind: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalOracle {
    pub action: String,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub text: Option<String>,
    pub tolerance_px: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalConfig {
    pub live: bool,
    pub max_calls: usize,
    pub candidates: Vec<EvalCandidate>,
    pub cases: Vec<EvalCase>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalResult {
    pub model: String,
    pub case_id: String,
    pub live: bool,
    pub latency_ms: u128,
    pub score: f64,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub finish_reason: Option<String>,
    pub response_excerpt: String,
    pub failure_classification: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalModelSummary {
    pub model: String,
    pub cases: usize,
    pub errors: usize,
    pub average_score: f64,
    pub average_latency_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalReport {
    pub live: bool,
    pub max_calls: usize,
    pub candidate_checks: Vec<CandidateCheck>,
    pub results: Vec<EvalResult>,
    pub summaries: Vec<EvalModelSummary>,
    pub winner: Option<String>,
}

impl Default for EvalConfig {
    fn default() -> Self {
        Self {
            live: false,
            max_calls: 8,
            candidates: vec![
                EvalCandidate {
                    provider: "openrouter".to_string(),
                    model: "google/gemini-3.5-flash-lite".to_string(),
                    max_output_tokens: 256,
                },
                EvalCandidate {
                    provider: "openrouter".to_string(),
                    model: "google/gemini-3.7-flash".to_string(),
                    max_output_tokens: 256,
                },
                EvalCandidate {
                    provider: "openrouter".to_string(),
                    model: "openai/gpt-5.4-mini".to_string(),
                    max_output_tokens: 256,
                },
                EvalCandidate {
                    provider: "openrouter".to_string(),
                    model: "openai/gpt-5-mini".to_string(),
                    max_output_tokens: 256,
                },
            ],
            cases: vec![
                EvalCase {
                    id: "click_continue_button_fixture".to_string(),
                    prompt: "Use the screenshot fixture. Click the blue Continue button. Return only JSON shaped like {\"action\":\"mouse_click\",\"x\":640,\"y\":360}. Use integer coordinates.".to_string(),
                    expected_action_kind: "mouse_click".to_string(),
                    fixture: Some(EvalFixture {
                        kind: "continue_button".to_string(),
                        width: 1280,
                        height: 720,
                    }),
                    oracle: EvalOracle {
                        action: "mouse_click".to_string(),
                        x: Some(640),
                        y: Some(360),
                        text: None,
                        tolerance_px: Some(90),
                    },
                },
                EvalCase {
                    id: "type_hello_in_field_fixture".to_string(),
                    prompt: "Use the screenshot fixture. The text field is already focused. Type hello into it. Return only JSON shaped like {\"action\":\"key_type\",\"text\":\"hello\"}.".to_string(),
                    expected_action_kind: "key_type".to_string(),
                    fixture: Some(EvalFixture {
                        kind: "focused_text_field".to_string(),
                        width: 1280,
                        height: 720,
                    }),
                    oracle: EvalOracle {
                        action: "key_type".to_string(),
                        x: None,
                        y: None,
                        text: Some("hello".to_string()),
                        tolerance_px: None,
                    },
                },
                EvalCase {
                    id: "click_top_right_close_fixture".to_string(),
                    prompt: "Use the screenshot fixture. Click the red close control in the top-right toolbar. Return only JSON shaped like {\"action\":\"mouse_click\",\"x\":1118,\"y\":118}. Use integer coordinates.".to_string(),
                    expected_action_kind: "mouse_click".to_string(),
                    fixture: Some(EvalFixture {
                        kind: "top_right_close".to_string(),
                        width: 1280,
                        height: 720,
                    }),
                    oracle: EvalOracle {
                        action: "mouse_click".to_string(),
                        x: Some(1118),
                        y: Some(118),
                        text: None,
                        tolerance_px: Some(70),
                    },
                },
                EvalCase {
                    id: "click_sidebar_settings_fixture".to_string(),
                    prompt: "Use the screenshot fixture. Click the selected Settings row in the left sidebar. Return only JSON shaped like {\"action\":\"mouse_click\",\"x\":188,\"y\":418}. Use integer coordinates.".to_string(),
                    expected_action_kind: "mouse_click".to_string(),
                    fixture: Some(EvalFixture {
                        kind: "sidebar_settings".to_string(),
                        width: 1280,
                        height: 720,
                    }),
                    oracle: EvalOracle {
                        action: "mouse_click".to_string(),
                        x: Some(188),
                        y: Some(418),
                        text: None,
                        tolerance_px: Some(80),
                    },
                },
                EvalCase {
                    id: "type_search_query_fixture".to_string(),
                    prompt: "Use the screenshot fixture. The search field is focused. Type open settings into it. Return only JSON shaped like {\"action\":\"key_type\",\"text\":\"open settings\"}.".to_string(),
                    expected_action_kind: "key_type".to_string(),
                    fixture: Some(EvalFixture {
                        kind: "focused_search_field".to_string(),
                        width: 1280,
                        height: 720,
                    }),
                    oracle: EvalOracle {
                        action: "key_type".to_string(),
                        x: None,
                        y: None,
                        text: Some("open settings".to_string()),
                        tolerance_px: None,
                    },
                },
            ],
        }
    }
}

impl EvalCase {
    fn fixture_frame(&self) -> anyhow::Result<Option<FramePayload>> {
        let Some(fixture) = &self.fixture else {
            return Ok(None);
        };
        let mut image = ImageBuffer::<Rgba<u8>, Vec<u8>>::from_pixel(
            fixture.width,
            fixture.height,
            Rgba([244, 246, 248, 255]),
        );
        draw_fixture(&mut image, &fixture.kind);
        let mut bytes = Vec::new();
        let encoder = PngEncoder::new(Cursor::new(&mut bytes));
        encoder
            .write_image(
                &image,
                image.width(),
                image.height(),
                image::ExtendedColorType::Rgba8,
            )
            .context("encode eval fixture")?;
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        let byte_len = bytes.len();
        Ok(Some(FramePayload {
            envelope: FrameEnvelope {
                schema_version: SCHEMA_VERSION.to_string(),
                frame_id: 0,
                timestamp_mono_ns: 0,
                timestamp_wall_ms: now_wall_ms(),
                display_id: format!("eval-fixture-{}", fixture.kind),
                display_width: fixture.width,
                display_height: fixture.height,
                width: fixture.width,
                height: fixture.height,
                scale_factor: 1.0,
                pixel_format: "rgba8".to_string(),
                encoding: FrameEncoding::Png,
                byte_len,
                sha256,
                cursor: CursorState {
                    x: 0.0,
                    y: 0.0,
                    visible: false,
                    included_in_frame: false,
                },
                damage_rects: vec![Rect {
                    x: 0,
                    y: 0,
                    width: fixture.width,
                    height: fixture.height,
                }],
            },
            bytes_base64: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
        }))
    }
}

fn draw_fixture(image: &mut ImageBuffer<Rgba<u8>, Vec<u8>>, kind: &str) {
    match kind {
        "continue_button" => {
            fill_rect(image, 455, 310, 370, 100, Rgba([21, 101, 216, 255]));
            fill_rect(image, 475, 330, 330, 60, Rgba([39, 126, 241, 255]));
        }
        "focused_text_field" => {
            fill_rect(image, 330, 300, 620, 92, Rgba([255, 255, 255, 255]));
            stroke_rect(image, 330, 300, 620, 92, Rgba([27, 104, 212, 255]));
            fill_rect(image, 360, 326, 3, 40, Rgba([12, 18, 28, 255]));
        }
        "top_right_close" => {
            fill_rect(image, 94, 86, 1092, 72, Rgba([255, 255, 255, 255]));
            stroke_rect(image, 94, 86, 1092, 72, Rgba([182, 190, 202, 255]));
            fill_rect(image, 1086, 92, 64, 52, Rgba([222, 52, 72, 255]));
            fill_rect(image, 1104, 110, 28, 16, Rgba([255, 234, 238, 255]));
        }
        "sidebar_settings" => {
            fill_rect(image, 80, 70, 250, 580, Rgba([232, 236, 242, 255]));
            fill_rect(image, 110, 382, 168, 72, Rgba([36, 104, 212, 255]));
            fill_rect(image, 136, 408, 34, 20, Rgba([255, 255, 255, 255]));
            fill_rect(image, 190, 408, 58, 18, Rgba([255, 255, 255, 255]));
        }
        "focused_search_field" => {
            fill_rect(image, 260, 118, 760, 76, Rgba([255, 255, 255, 255]));
            stroke_rect(image, 260, 118, 760, 76, Rgba([27, 104, 212, 255]));
            fill_rect(image, 296, 144, 3, 32, Rgba([12, 18, 28, 255]));
        }
        _ => {}
    }
}

fn fill_rect(
    image: &mut ImageBuffer<Rgba<u8>, Vec<u8>>,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    color: Rgba<u8>,
) {
    for py in y..(y + height).min(image.height()) {
        for px in x..(x + width).min(image.width()) {
            image.put_pixel(px, py, color);
        }
    }
}

fn stroke_rect(
    image: &mut ImageBuffer<Rgba<u8>, Vec<u8>>,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    color: Rgba<u8>,
) {
    fill_rect(image, x, y, width, 3, color);
    fill_rect(image, x, y + height.saturating_sub(3), width, 3, color);
    fill_rect(image, x, y, 3, height, color);
    fill_rect(image, x + width.saturating_sub(3), y, 3, height, color);
}

pub async fn run_eval(
    config: EvalConfig,
    frame: Option<FramePayload>,
    api_key: Option<String>,
) -> Vec<EvalResult> {
    let mut results = Vec::new();
    let mut calls = 0usize;
    for candidate in config.candidates {
        for case in &config.cases {
            if calls >= config.max_calls {
                return results;
            }
            calls += 1;
            let started = Instant::now();
            if !config.live {
                results.push(EvalResult {
                    model: candidate.model.clone(),
                    case_id: case.id.clone(),
                    live: false,
                    latency_ms: started.elapsed().as_millis(),
                    score: 0.5,
                    prompt_tokens: None,
                    completion_tokens: None,
                    total_tokens: None,
                    finish_reason: None,
                    response_excerpt:
                        "dry-run: live provider calls disabled; candidate queued for bounded eval"
                            .to_string(),
                    failure_classification: None,
                    error: None,
                });
                continue;
            }
            let case_frame = case
                .fixture_frame()
                .unwrap_or_else(|_| None)
                .or_else(|| frame.clone());
            let outcome =
                call_openrouter(&candidate, case, case_frame.as_ref(), api_key.as_deref()).await;
            match outcome {
                Ok(output) => {
                    let score = score_response(&output.text, &case.oracle);
                    results.push(EvalResult {
                        model: candidate.model.clone(),
                        case_id: case.id.clone(),
                        live: true,
                        latency_ms: started.elapsed().as_millis(),
                        score,
                        prompt_tokens: output.usage.prompt_tokens,
                        completion_tokens: output.usage.completion_tokens,
                        total_tokens: output.usage.total_tokens,
                        finish_reason: output.finish_reason,
                        response_excerpt: output.text.chars().take(240).collect(),
                        failure_classification: None,
                        error: None,
                    });
                }
                Err(err) => {
                    let error = err.to_string();
                    results.push(EvalResult {
                        model: candidate.model.clone(),
                        case_id: case.id.clone(),
                        live: true,
                        latency_ms: started.elapsed().as_millis(),
                        score: 0.0,
                        prompt_tokens: None,
                        completion_tokens: None,
                        total_tokens: None,
                        finish_reason: None,
                        response_excerpt: String::new(),
                        failure_classification: Some(classify_failure(&error).to_string()),
                        error: Some(error),
                    })
                }
            }
        }
    }
    results
}

pub async fn run_eval_report(
    config: EvalConfig,
    frame: Option<FramePayload>,
    api_key: Option<String>,
) -> EvalReport {
    let max_calls = config.max_calls;
    let live = config.live;
    let candidate_checks = if live {
        validate_candidates(&config.candidates).await
    } else {
        config
            .candidates
            .iter()
            .map(|candidate| CandidateCheck {
                provider: candidate.provider.clone(),
                model: candidate.model.clone(),
                available: true,
                input_modalities: Vec::new(),
                output_modalities: Vec::new(),
                reason: Some("catalog validation skipped for dry-run eval".to_string()),
            })
            .collect()
    };
    let mut runnable_config = config;
    if live {
        runnable_config.candidates.retain(|candidate| {
            candidate_checks
                .iter()
                .any(|check| check.model == candidate.model && check.available)
        });
    }
    let results = run_eval(runnable_config, frame, api_key).await;
    let summaries = summarize_results(&results);
    let winner = summaries.first().map(|summary| summary.model.clone());
    EvalReport {
        live,
        max_calls,
        candidate_checks,
        results,
        summaries,
        winner,
    }
}

pub async fn validate_candidates(candidates: &[EvalCandidate]) -> Vec<CandidateCheck> {
    let catalog = fetch_openrouter_catalog().await;
    candidates
        .iter()
        .map(|candidate| {
            if candidate.provider != "openrouter" {
                return CandidateCheck {
                    provider: candidate.provider.clone(),
                    model: candidate.model.clone(),
                    available: false,
                    input_modalities: Vec::new(),
                    output_modalities: Vec::new(),
                    reason: Some(format!("unsupported provider {}", candidate.provider)),
                };
            }
            match &catalog {
                Ok(models) => models
                    .iter()
                    .find(|model| model.id == candidate.model)
                    .map(|model| CandidateCheck {
                        provider: candidate.provider.clone(),
                        model: candidate.model.clone(),
                        available: model.input_modalities().iter().any(|m| m == "image")
                            && model.output_modalities().iter().any(|m| m == "text"),
                        input_modalities: model.input_modalities().to_vec(),
                        output_modalities: model.output_modalities().to_vec(),
                        reason: None,
                    })
                    .unwrap_or(CandidateCheck {
                        provider: candidate.provider.clone(),
                        model: candidate.model.clone(),
                        available: false,
                        input_modalities: Vec::new(),
                        output_modalities: Vec::new(),
                        reason: Some("model not found in OpenRouter catalog".to_string()),
                    }),
                Err(error) => CandidateCheck {
                    provider: candidate.provider.clone(),
                    model: candidate.model.clone(),
                    available: false,
                    input_modalities: Vec::new(),
                    output_modalities: Vec::new(),
                    reason: Some(format!("catalog lookup failed: {error}")),
                },
            }
        })
        .collect()
}

#[derive(Debug, Clone, Deserialize)]
struct OpenRouterCatalog {
    data: Vec<OpenRouterModel>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenRouterModel {
    id: String,
    #[serde(default)]
    architecture: OpenRouterArchitecture,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct OpenRouterArchitecture {
    #[serde(default)]
    input_modalities: Vec<String>,
    #[serde(default)]
    output_modalities: Vec<String>,
}

impl OpenRouterModel {
    fn input_modalities(&self) -> &[String] {
        &self.architecture.input_modalities
    }

    fn output_modalities(&self) -> &[String] {
        &self.architecture.output_modalities
    }
}

async fn fetch_openrouter_catalog() -> anyhow::Result<Vec<OpenRouterModel>> {
    let response = reqwest::Client::new()
        .get("https://openrouter.ai/api/v1/models?input_modalities=text,image&output_modalities=text")
        .send()
        .await
        .context("fetch OpenRouter model catalog")?;
    let status = response.status();
    let catalog: OpenRouterCatalog = response.json().await.context("decode OpenRouter catalog")?;
    if !status.is_success() {
        bail!("OpenRouter catalog returned {status}");
    }
    Ok(catalog.data)
}

pub fn summarize_results(results: &[EvalResult]) -> Vec<EvalModelSummary> {
    let mut groups: BTreeMap<String, Vec<&EvalResult>> = BTreeMap::new();
    for result in results {
        groups.entry(result.model.clone()).or_default().push(result);
    }
    let mut summaries = groups
        .into_iter()
        .map(|(model, rows)| {
            let cases = rows.len();
            let errors = rows.iter().filter(|row| row.error.is_some()).count();
            let average_score = rows.iter().map(|row| row.score).sum::<f64>() / cases.max(1) as f64;
            let average_latency_ms =
                rows.iter().map(|row| row.latency_ms as f64).sum::<f64>() / cases.max(1) as f64;
            EvalModelSummary {
                model,
                cases,
                errors,
                average_score,
                average_latency_ms,
            }
        })
        .collect::<Vec<_>>();
    summaries.sort_by(|left, right| {
        right
            .average_score
            .partial_cmp(&left.average_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.errors.cmp(&right.errors))
            .then_with(|| {
                left.average_latency_ms
                    .partial_cmp(&right.average_latency_ms)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    summaries
}

async fn call_openrouter(
    candidate: &EvalCandidate,
    case: &EvalCase,
    frame: Option<&FramePayload>,
    api_key: Option<&str>,
) -> anyhow::Result<ProviderOutput> {
    if candidate.provider != "openrouter" {
        bail!("unsupported provider {}", candidate.provider);
    }
    let api_key = api_key.context("OPENROUTER_API_KEY is required for live eval")?;
    let mut content = vec![serde_json::json!({"type": "text", "text": case.prompt})];
    if let Some(frame) = frame.and_then(|f| f.bytes_base64.as_ref()) {
        content.push(serde_json::json!({
            "type": "image_url",
            "image_url": {"url": format!("data:image/png;base64,{frame}")},
        }));
    }
    let body = serde_json::json!({
        "model": candidate.model,
        "messages": [{"role": "user", "content": content}],
        "max_tokens": candidate.max_output_tokens,
        "temperature": 0,
    });
    let client = reqwest::Client::new();
    let mut last_error = None;
    for attempt in 0..2 {
        let response = client
            .post("https://openrouter.ai/api/v1/chat/completions")
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .header(CONTENT_TYPE, "application/json")
            .header(REFERER, "http://localhost/cua")
            .json(&body)
            .send()
            .await
            .context("send openrouter request")?;
        let status = response.status();
        let value: serde_json::Value = response
            .json()
            .await
            .context("decode openrouter response")?;
        if status.is_success() {
            return Ok(ProviderOutput {
                text: value["choices"][0]["message"]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                finish_reason: value["choices"][0]["finish_reason"]
                    .as_str()
                    .map(str::to_string),
                usage: Usage {
                    prompt_tokens: value["usage"]["prompt_tokens"].as_u64(),
                    completion_tokens: value["usage"]["completion_tokens"].as_u64(),
                    total_tokens: value["usage"]["total_tokens"].as_u64(),
                },
            });
        }
        let retryable = status.as_u16() == 429 || status.is_server_error();
        last_error = Some(format!("openrouter {status}: {value}"));
        if !retryable || attempt == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    bail!(last_error.unwrap_or_else(|| "openrouter request failed".to_string()))
}

#[derive(Debug, Clone)]
struct ProviderOutput {
    text: String,
    finish_reason: Option<String>,
    usage: Usage,
}

#[derive(Debug, Clone, Default)]
struct Usage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

fn classify_failure(error: &str) -> &'static str {
    let lower = error.to_ascii_lowercase();
    if lower.contains("401") || lower.contains("403") {
        "auth"
    } else if lower.contains("429") {
        "rate_limit"
    } else if lower.contains("timeout") || lower.contains("timed out") {
        "timeout"
    } else if lower.contains("not found") || lower.contains("404") {
        "model_unavailable"
    } else if lower.contains("500") || lower.contains("502") || lower.contains("503") {
        "provider_transient"
    } else {
        "unknown"
    }
}

fn score_response(text: &str, oracle: &EvalOracle) -> f64 {
    let stripped = strip_json_fence(text);
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&stripped) else {
        return 0.0;
    };
    let mut score = 0.3;
    if action_name(&value).as_deref() == Some(oracle.action.as_str()) {
        score += 0.35;
    }
    if let (Some(expected_x), Some(expected_y)) = (oracle.x, oracle.y) {
        let tolerance = oracle.tolerance_px.unwrap_or(0) as i64;
        let x_ok = value
            .get("x")
            .and_then(|x| x.as_i64())
            .map(|x| (x - expected_x as i64).abs() <= tolerance)
            .unwrap_or(false);
        let y_ok = value
            .get("y")
            .and_then(|y| y.as_i64())
            .map(|y| (y - expected_y as i64).abs() <= tolerance)
            .unwrap_or(false);
        if x_ok && y_ok {
            score += 0.35;
        }
    } else if let Some(expected_text) = &oracle.text {
        if value.get("text").and_then(|text| text.as_str()) == Some(expected_text.as_str()) {
            score += 0.35;
        }
    } else {
        score += 0.35;
    }
    score
}

fn action_name(value: &serde_json::Value) -> Option<String> {
    value
        .get("action")
        .or_else(|| value.get("kind"))
        .and_then(|action| action.as_str())
        .map(str::to_string)
}

fn strip_json_fence(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(inner) = trimmed
        .strip_prefix("```json")
        .and_then(|s| s.strip_suffix("```"))
    {
        return inner.trim().to_string();
    }
    if let Some(inner) = trimmed
        .strip_prefix("```")
        .and_then(|s| s.strip_suffix("```"))
    {
        return inner.trim().to_string();
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::{score_response, summarize_results, EvalCase, EvalOracle, EvalResult};

    #[test]
    fn scores_exact_json_contract() {
        let score = score_response(
            r#"{"action":"mouse_click","x":640,"y":360}"#,
            &click_oracle(),
        );
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn scores_fenced_json_contract() {
        let score = score_response(
            "```json\n{\"action\":\"key_type\",\"text\":\"hello\"}\n```",
            &type_oracle(),
        );
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn generated_fixture_has_image_bytes() {
        let case = EvalCase {
            id: "fixture".to_string(),
            prompt: String::new(),
            expected_action_kind: "mouse_click".to_string(),
            fixture: Some(super::EvalFixture {
                kind: "continue_button".to_string(),
                width: 320,
                height: 180,
            }),
            oracle: click_oracle(),
        };
        let frame = case.fixture_frame().unwrap().unwrap();
        assert_eq!(frame.envelope.width, 320);
        assert!(frame.bytes_base64.unwrap().len() > 100);
    }

    #[test]
    fn summary_prefers_score_then_latency() {
        let rows = vec![
            EvalResult {
                model: "slow".to_string(),
                case_id: "a".to_string(),
                live: true,
                latency_ms: 1000,
                score: 1.0,
                prompt_tokens: Some(10),
                completion_tokens: Some(5),
                total_tokens: Some(15),
                finish_reason: Some("stop".to_string()),
                response_excerpt: String::new(),
                failure_classification: None,
                error: None,
            },
            EvalResult {
                model: "fast".to_string(),
                case_id: "a".to_string(),
                live: true,
                latency_ms: 100,
                score: 1.0,
                prompt_tokens: Some(10),
                completion_tokens: Some(5),
                total_tokens: Some(15),
                finish_reason: Some("stop".to_string()),
                response_excerpt: String::new(),
                failure_classification: None,
                error: None,
            },
        ];
        assert_eq!(summarize_results(&rows)[0].model, "fast");
    }

    fn click_oracle() -> EvalOracle {
        EvalOracle {
            action: "mouse_click".to_string(),
            x: Some(640),
            y: Some(360),
            text: None,
            tolerance_px: Some(10),
        }
    }

    fn type_oracle() -> EvalOracle {
        EvalOracle {
            action: "key_type".to_string(),
            x: None,
            y: None,
            text: Some("hello".to_string()),
            tolerance_px: None,
        }
    }
}

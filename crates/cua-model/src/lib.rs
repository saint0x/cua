use anyhow::{bail, Context};
use cua_core::FramePayload;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, REFERER};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
    pub response_excerpt: String,
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
                    id: "click_center_button".to_string(),
                    prompt: "Given a desktop screenshot with a centered confirmation button, return only JSON: {\"action\":\"mouse_click\",\"x\":640,\"y\":360}. Use integer coordinates.".to_string(),
                    expected_action_kind: "mouse_click".to_string(),
                },
                EvalCase {
                    id: "type_short_text".to_string(),
                    prompt: "Given a focused text field, return only JSON for typing hello: {\"action\":\"key_type\",\"text\":\"hello\"}.".to_string(),
                    expected_action_kind: "key_type".to_string(),
                },
            ],
        }
    }
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
                    response_excerpt:
                        "dry-run: live provider calls disabled; candidate queued for bounded eval"
                            .to_string(),
                    error: None,
                });
                continue;
            }
            let outcome =
                call_openrouter(&candidate, case, frame.as_ref(), api_key.as_deref()).await;
            match outcome {
                Ok(text) => {
                    let score = score_response(&text, &case.expected_action_kind);
                    results.push(EvalResult {
                        model: candidate.model.clone(),
                        case_id: case.id.clone(),
                        live: true,
                        latency_ms: started.elapsed().as_millis(),
                        score,
                        response_excerpt: text.chars().take(240).collect(),
                        error: None,
                    });
                }
                Err(err) => results.push(EvalResult {
                    model: candidate.model.clone(),
                    case_id: case.id.clone(),
                    live: true,
                    latency_ms: started.elapsed().as_millis(),
                    score: 0.0,
                    response_excerpt: String::new(),
                    error: Some(err.to_string()),
                }),
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
) -> anyhow::Result<String> {
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
    if !status.is_success() {
        bail!("openrouter {status}: {value}");
    }
    Ok(value["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string())
}

fn score_response(text: &str, expected_action: &str) -> f64 {
    let stripped = strip_json_fence(text);
    let normalized = stripped.to_ascii_lowercase();
    let mut score = 0.0;
    if normalized.contains(expected_action) {
        score += 0.6;
    }
    if serde_json::from_str::<serde_json::Value>(&stripped).is_ok() {
        score += 0.3;
    }
    if normalized.contains("x") || normalized.contains("text") {
        score += 0.1;
    }
    score
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
    use super::{score_response, summarize_results, EvalResult};

    #[test]
    fn scores_exact_json_contract() {
        let score = score_response(r#"{"action":"mouse_click","x":640,"y":360}"#, "mouse_click");
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn scores_fenced_json_contract() {
        let score = score_response(
            "```json\n{\"action\":\"key_type\",\"text\":\"hello\"}\n```",
            "key_type",
        );
        assert!((score - 1.0).abs() < f64::EPSILON);
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
                response_excerpt: String::new(),
                error: None,
            },
            EvalResult {
                model: "fast".to_string(),
                case_id: "a".to_string(),
                live: true,
                latency_ms: 100,
                score: 1.0,
                response_excerpt: String::new(),
                error: None,
            },
        ];
        assert_eq!(summarize_results(&rows)[0].model, "fast");
    }
}

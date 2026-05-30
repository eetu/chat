use std::sync::Arc;

use actix_web_lab::sse;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::AppState;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    /// Base64-encoded images (no `data:` prefix). Forwarded to Ollama on
    /// vision-capable models. Omitted from the wire when empty so non-vision
    /// models don't choke on the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
struct OllamaChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct OllamaChunkMessage {
    #[serde(default)]
    content: String,
}

#[derive(Debug, Deserialize)]
struct OllamaChunk {
    #[serde(default)]
    message: Option<OllamaChunkMessage>,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    done_reason: Option<String>,
    /// Tokens generated for this turn. Present on the terminal chunk
    /// only.
    #[serde(default)]
    eval_count: Option<u32>,
    /// Generation wall-clock time, in nanoseconds. Used together with
    /// `eval_count` to compute tokens/sec.
    #[serde(default)]
    eval_duration: Option<u64>,
    /// Prompt-side token count. Surfaced in the UI so the user has some
    /// sense of context size.
    #[serde(default)]
    prompt_eval_count: Option<u32>,
}

/// Generation stats from Ollama's final NDJSON chunk. Forwarded as a
/// `stats` SSE event so the UI can render tokens + tokens/sec under the
/// assistant bubble.
#[derive(Debug, Clone, Copy, Default)]
pub struct ChatStats {
    pub tokens: u32,
    pub prompt_tokens: u32,
    pub tokens_per_sec: f32,
}

pub async fn list_models(state: &AppState) -> Result<serde_json::Value, reqwest::Error> {
    let url = format!("{}/api/tags", state.settings.ollama_url.trim_end_matches('/'));
    state.http_client.get(&url).send().await?.error_for_status()?.json().await
}

/// Hint Ollama to unload a model. Posts a noop generation with
/// `keep_alive: 0`, which makes Ollama unload the named model right after
/// (or, when it isn't currently loaded, immediately). Used before invoking
/// ComfyUI on memory-constrained hosts where Kontext FP8 (~12 GB) plus a
/// chat / refiner model would otherwise blow past available VRAM/RAM.
/// Best-effort — log on failure, never propagate.
pub async fn evict(state: &AppState, model: &str) {
    let url = format!("{}/api/generate", state.settings.ollama_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "prompt": "",
        "keep_alive": 0,
        "stream": false,
    });
    match state.http_client.post(&url).json(&body).send().await {
        Ok(res) => {
            if let Err(e) = res.error_for_status() {
                tracing::warn!("evict({model}) upstream returned error: {e}");
            }
        }
        Err(e) => tracing::warn!("evict({model}) request failed: {e}"),
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ModelCapabilities {
    pub vision: bool,
    pub tools: bool,
    pub chat: bool,
    pub capabilities: Vec<String>,
    pub families: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ShowResponse {
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    details: Option<ShowDetails>,
}

#[derive(Debug, Deserialize)]
struct ShowDetails {
    #[serde(default)]
    families: Option<Vec<String>>,
}

/// Query Ollama `/api/show` and normalise the response into our caps shape.
/// Falls back to inferring `vision` from `details.families` when the
/// `capabilities` array is absent (older Ollama versions).
pub async fn show_capabilities(
    state: &AppState,
    model: &str,
) -> Result<ModelCapabilities, reqwest::Error> {
    let url = format!("{}/api/show", state.settings.ollama_url.trim_end_matches('/'));
    let res = state
        .http_client
        .post(&url)
        .json(&serde_json::json!({ "name": model }))
        .send()
        .await?
        .error_for_status()?;
    let parsed: ShowResponse = res.json().await?;
    let families = parsed
        .details
        .and_then(|d| d.families)
        .unwrap_or_default();
    let vision_family = families
        .iter()
        .any(|f| matches!(f.as_str(), "clip" | "mllama" | "vision" | "siglip"));
    let vision = parsed.capabilities.iter().any(|c| c == "vision") || vision_family;
    let tools = parsed.capabilities.iter().any(|c| c == "tools");
    // Default `chat` to true when the capabilities array is empty (older
    // Ollama versions never populated it), so existing models keep working.
    let chat = parsed.capabilities.is_empty()
        || parsed.capabilities.iter().any(|c| c == "completion");
    Ok(ModelCapabilities {
        vision,
        tools,
        chat,
        capabilities: parsed.capabilities,
        families,
    })
}

#[derive(Debug, Deserialize)]
struct OllamaChatResponse {
    message: ChatMessage,
}

/// Ask the model for a 3-6 word title summarizing one back-and-forth.
/// Non-streaming; returns the cleaned title or an error.
pub async fn summarize_title(
    state: Arc<AppState>,
    model: &str,
    user_msg: &str,
    assistant_msg: &str,
) -> Result<String, reqwest::Error> {
    let user_excerpt: String = user_msg.chars().take(500).collect();
    let asst_excerpt: String = assistant_msg.chars().take(500).collect();

    let messages = vec![
        ChatMessage {
            role: "system".into(),
            content:
                "You name conversations. Reply with a 3 to 6 word lowercase title \
                 summarizing the conversation. No quotes. No punctuation. \
                 No markdown. No prefixes like 'title:'. Just the title."
                    .into(),
            images: None,
        },
        ChatMessage {
            role: "user".into(),
            content: format!(
                "user: {user_excerpt}\n\nassistant: {asst_excerpt}\n\n\
                 Reply with only the title."
            ),
            images: None,
        },
    ];

    let url = format!("{}/api/chat", state.settings.ollama_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": false,
    });

    let res = state
        .http_client
        .post(&url)
        .json(&body)
        .send()
        .await?
        .error_for_status()?;

    let parsed: OllamaChatResponse = res.json().await?;
    Ok(sanitize_title(&parsed.message.content))
}

/// Ask a vision-capable chat model to describe a generated image. The
/// description is used as context for follow-up image-gen turns when the
/// user has the prompt-refiner toggle off.
pub async fn describe_image(
    state: Arc<AppState>,
    model: &str,
    image_b64: &str,
) -> Result<String, reqwest::Error> {
    let messages = vec![
        ChatMessage {
            role: "system".into(),
            content: "Describe the supplied image in one paragraph as if it \
                were a detailed image-generation prompt. Cover subject, \
                composition, framing, lighting, materials, mood, and any \
                distinctive style. Plain text. No preamble, no quotes, no \
                markdown, no commentary."
                .into(),
            images: None,
        },
        ChatMessage {
            role: "user".into(),
            content: "describe this image".into(),
            images: Some(vec![image_b64.to_string()]),
        },
    ];

    let url = format!("{}/api/chat", state.settings.ollama_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": false,
    });
    let res = state
        .http_client
        .post(&url)
        .json(&body)
        .send()
        .await?
        .error_for_status()?;
    let parsed: OllamaChatResponse = res.json().await?;
    Ok(parsed.message.content.trim().to_string())
}

/// Expand a user's image-gen prompt into a detailed prompt, using the
/// conversation history so follow-ups ("make it night", "same scene in
/// winter") build on prior turns. The `system_prompt` parameter selects
/// the persona/voice the rewriter should adopt — see
/// `handlers::personas`. Best-effort: callers fall back to the original
/// prompt on error.
/// Rewritten positive prompt plus a list of failure modes to drive the
/// negative branch of an inpaint workflow. The negative is best-effort:
/// when the refiner doesn't return JSON, we fall back to treating the
/// whole reply as the positive and leave negative empty.
#[derive(Debug, Clone, Default)]
pub struct RefinedPrompt {
    pub positive: String,
    pub negative: String,
}

pub async fn refine_image_prompt(
    state: Arc<AppState>,
    model: &str,
    system_prompt: &str,
    history: &[ChatMessage],
) -> Result<RefinedPrompt, reqwest::Error> {
    // Wrap the persona prompt in an output-shape instruction asking for
    // strict JSON. Negative-prompt support only kicks in on workflows
    // that run real CFG (today: Flux Fill inpaint); for everything else
    // the field is generated but ignored, which is cheap insurance for
    // when more CFG-aware paths land.
    let composite_system = format!(
        "{system_prompt}\n\nReturn STRICT JSON with two string fields:\n\
         - \"positive\": the rewritten prompt, following all the rules above. \
         One paragraph, plain text, no markdown.\n\
         - \"negative\": a short comma-separated list (6-10 items) of failure modes \
         the image generator should avoid for this kind of request — typical diffusion \
         glitches such as anatomical errors, duplicated objects, broken geometry, \
         floating elements, extra fingers, melted faces, illegible text, etc. \
         Terse phrases, no full sentences.\n\
         Output the JSON object only — no markdown fences, no preamble, no commentary."
    );
    let mut messages = Vec::with_capacity(history.len() + 1);
    messages.push(ChatMessage {
        role: "system".into(),
        content: composite_system,
        images: None,
    });
    messages.extend(history.iter().cloned());

    let url = format!("{}/api/chat", state.settings.ollama_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": false,
        // Force JSON output where the upstream supports it. Ollama
        // honours `format: "json"` on most chat-completion models;
        // older models that ignore it still surface JSON in the body
        // because the system prompt asks for it directly.
        "format": "json",
    });

    let res = state
        .http_client
        .post(&url)
        .json(&body)
        .send()
        .await?
        .error_for_status()?;
    let parsed: OllamaChatResponse = res.json().await?;
    Ok(parse_refined_prompt(parsed.message.content.trim()))
}

/// Pull `{positive, negative}` from the refiner reply. Tolerates
/// markdown code fences and replies that aren't quite JSON — when
/// parsing fails we treat the whole reply as the positive prompt and
/// leave negative empty so the caller still gets the legacy behaviour.
fn parse_refined_prompt(raw: &str) -> RefinedPrompt {
    let trimmed = raw.trim();
    let stripped = strip_code_fence(trimmed);
    #[derive(serde::Deserialize)]
    struct Shape {
        #[serde(default)]
        positive: String,
        #[serde(default)]
        negative: String,
    }
    match serde_json::from_str::<Shape>(stripped) {
        Ok(s) => RefinedPrompt {
            positive: s.positive.trim().to_string(),
            negative: s.negative.trim().to_string(),
        },
        Err(e) => {
            tracing::warn!(
                "refiner JSON parse failed: {e}; treating reply as positive-only"
            );
            RefinedPrompt {
                positive: trimmed.to_string(),
                negative: String::new(),
            }
        }
    }
}

fn strip_code_fence(s: &str) -> &str {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("```json") {
        return rest.trim_start().trim_end_matches("```").trim();
    }
    if let Some(rest) = s.strip_prefix("```") {
        return rest.trim_start().trim_end_matches("```").trim();
    }
    s
}

fn sanitize_title(raw: &str) -> String {
    let cleaned: String = raw
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '.' || c == '`')
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace() || matches!(*c, '-' | '_' | '/'))
        .collect();
    cleaned.split_whitespace().take(8).collect::<Vec<_>>().join(" ")
}

#[derive(Debug, Deserialize)]
struct EmbedResponse {
    #[serde(default)]
    embedding: Vec<f32>,
}

#[derive(Debug, Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<TagsModel>,
}

#[derive(Debug, Deserialize)]
struct TagsModel {
    #[serde(default)]
    name: String,
}

/// Return installed Ollama models whose capabilities include
/// "embedding". Hits `/api/tags` for the list, then `/api/show` per
/// model (capability cache is keyed by name, so repeat calls are
/// cheap). Errors on /api/show are logged and the model is skipped.
pub async fn list_embedding_models(state: &AppState) -> Result<Vec<String>, reqwest::Error> {
    let url = format!("{}/api/tags", state.settings.ollama_url.trim_end_matches('/'));
    let res = state.http_client.get(&url).send().await?.error_for_status()?;
    let parsed: TagsResponse = res.json().await?;
    let mut out = Vec::new();
    for m in parsed.models {
        if m.name.is_empty() {
            continue;
        }
        let caps = if let Some(cached) = state.caps_cache.get(&m.name).await {
            cached
        } else {
            match show_capabilities(state, &m.name).await {
                Ok(c) => {
                    state.caps_cache.set(m.name.clone(), c.clone()).await;
                    c
                }
                Err(e) => {
                    tracing::debug!("show_capabilities({}) failed: {e}", m.name);
                    continue;
                }
            }
        };
        if caps.capabilities.iter().any(|c| c == "embedding") {
            out.push(m.name);
        }
    }
    Ok(out)
}

/// Call Ollama's `/api/embeddings` endpoint. Returns the float vector
/// or an empty Vec when the upstream came back malformed.
pub async fn embed_text(
    state: &AppState,
    model: &str,
    text: &str,
) -> Result<Vec<f32>, reqwest::Error> {
    let url = format!(
        "{}/api/embeddings",
        state.settings.ollama_url.trim_end_matches('/')
    );
    let body = serde_json::json!({ "model": model, "prompt": text });
    let res = state
        .http_client
        .post(&url)
        .json(&body)
        .send()
        .await?
        .error_for_status()?;
    let parsed: EmbedResponse = res.json().await?;
    Ok(parsed.embedding)
}

pub fn resolve_model(state: &AppState, requested: Option<&str>) -> String {
    state
        .settings
        .ollama_model_lock
        .clone()
        .or_else(|| requested.map(str::to_string))
        .unwrap_or_else(|| "llama3.1".into())
}

#[derive(Debug)]
pub struct StreamOutcome {
    /// Accumulated assistant text — populated even when the stream was
    /// aborted before completion.
    pub content: String,
    /// `true` if Ollama emitted `done: true`; `false` if the client
    /// disconnected, the user pressed stop, or the upstream stream ended
    /// without a terminal chunk.
    pub completed: bool,
    /// Generation stats, only populated on a clean `done: true` final
    /// chunk. None for aborted streams.
    pub stats: Option<ChatStats>,
}

/// Stream a chat completion. `on_delta` is called for each non-empty content
/// chunk; returning `false` aborts the loop (client gone / user stop) and
/// the partial `content` is returned in the outcome so it can still be
/// persisted.
pub async fn stream_chat<F>(
    state: Arc<AppState>,
    model: &str,
    messages: Vec<ChatMessage>,
    mut on_delta: F,
) -> Result<StreamOutcome, ChatStreamError>
where
    F: FnMut(&str) -> bool,
{
    let url = format!("{}/api/chat", state.settings.ollama_url.trim_end_matches('/'));
    let body = OllamaChatRequest { model, messages: &messages, stream: true };

    let res = state
        .http_client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(ChatStreamError::Http)?
        .error_for_status()
        .map_err(ChatStreamError::Http)?;

    let mut stream = res.bytes_stream();
    let mut buf = Vec::<u8>::new();
    let mut full = String::new();

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(ChatStreamError::Http)?;
        buf.extend_from_slice(&bytes);

        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let line = &line[..line.len() - 1];
            if line.is_empty() {
                continue;
            }
            let parsed: OllamaChunk = match serde_json::from_slice(line) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("malformed ndjson chunk: {e}");
                    continue;
                }
            };
            if let Some(msg) = parsed.message {
                if !msg.content.is_empty() {
                    full.push_str(&msg.content);
                    if !on_delta(&msg.content) {
                        return Ok(StreamOutcome {
                            content: full,
                            completed: false,
                            stats: None,
                        });
                    }
                }
            }
            if parsed.done {
                tracing::debug!(reason = ?parsed.done_reason, "ollama chunk done");
                let stats = match (parsed.eval_count, parsed.eval_duration) {
                    (Some(tokens), Some(ns)) if ns > 0 => Some(ChatStats {
                        tokens,
                        prompt_tokens: parsed.prompt_eval_count.unwrap_or(0),
                        tokens_per_sec: (tokens as f64 * 1.0e9 / ns as f64) as f32,
                    }),
                    _ => None,
                };
                return Ok(StreamOutcome { content: full, completed: true, stats });
            }
        }
    }
    Ok(StreamOutcome {
        content: full,
        completed: false,
        stats: None,
    })
}

#[derive(thiserror::Error, Debug)]
pub enum ChatStreamError {
    #[error("upstream http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("upstream returned no image data")]
    EmptyImage,
    #[error("comfyui job timed out before producing output")]
    ComfyTimeout,
    #[error("client disconnected — comfyui job cancelled")]
    Cancelled,
}

/// Helper to wrap a delta into an SSE event with `event: delta`.
pub fn sse_delta(content: &str) -> sse::Event {
    sse::Event::Data(sse::Data::new(content).event("delta"))
}

/// Helper to emit a typed JSON event.
pub fn sse_json(event: &str, value: &serde_json::Value) -> sse::Event {
    sse::Event::Data(sse::Data::new(value.to_string()).event(event))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_strict_json_refiner_reply() {
        let r = parse_refined_prompt(
            r#"{"positive": "a cat in a tree at golden hour", "negative": "extra fingers, blurry, deformed paws"}"#,
        );
        assert_eq!(r.positive, "a cat in a tree at golden hour");
        assert_eq!(r.negative, "extra fingers, blurry, deformed paws");
    }

    #[test]
    fn parses_json_inside_code_fence() {
        let r = parse_refined_prompt(
            "```json\n{\"positive\":\"p\",\"negative\":\"n\"}\n```",
        );
        assert_eq!(r.positive, "p");
        assert_eq!(r.negative, "n");
    }

    #[test]
    fn falls_back_to_positive_only_when_not_json() {
        let r = parse_refined_prompt("a plain paragraph that's not json at all");
        assert_eq!(r.positive, "a plain paragraph that's not json at all");
        assert_eq!(r.negative, "");
    }

    #[test]
    fn handles_missing_negative_field() {
        let r = parse_refined_prompt(r#"{"positive": "only positive given"}"#);
        assert_eq!(r.positive, "only positive given");
        assert_eq!(r.negative, "");
    }
}

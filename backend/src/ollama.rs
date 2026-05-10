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
    pub image_gen: bool,
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
    let image_gen = parsed.capabilities.iter().any(|c| c == "image");
    // Default `chat` to true when the capabilities array is empty (older
    // Ollama versions never populated it), so existing models keep working.
    let chat = parsed.capabilities.is_empty()
        || parsed.capabilities.iter().any(|c| c == "completion");
    Ok(ModelCapabilities {
        vision,
        tools,
        chat,
        image_gen,
        capabilities: parsed.capabilities,
        families,
    })
}

#[derive(Debug, Deserialize)]
struct ImagesResponse {
    data: Vec<ImagesDatum>,
}

#[derive(Debug, Deserialize)]
struct ImagesDatum {
    #[serde(default)]
    b64_json: Option<String>,
}

/// Call Ollama's experimental OpenAI-compatible image generation endpoint.
/// Returns the raw base64 (no `data:` prefix).
pub async fn generate_image(
    state: &AppState,
    model: &str,
    prompt: &str,
) -> Result<String, ChatStreamError> {
    let url = format!(
        "{}/v1/images/generations",
        state.settings.ollama_url.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "model": model,
        "prompt": prompt,
        "size": "1024x1024",
        "response_format": "b64_json",
    });
    let res = state
        .http_client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(ChatStreamError::Http)?
        .error_for_status()
        .map_err(ChatStreamError::Http)?;
    let parsed: ImagesResponse = res.json().await.map_err(ChatStreamError::Http)?;
    parsed
        .data
        .into_iter()
        .find_map(|d| d.b64_json)
        .ok_or(ChatStreamError::EmptyImage)
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
pub async fn refine_image_prompt(
    state: Arc<AppState>,
    model: &str,
    system_prompt: &str,
    history: &[ChatMessage],
) -> Result<String, reqwest::Error> {
    let mut messages = Vec::with_capacity(history.len() + 1);
    messages.push(ChatMessage {
        role: "system".into(),
        content: system_prompt.to_string(),
        images: None,
    });
    messages.extend(history.iter().cloned());

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
                        return Ok(StreamOutcome { content: full, completed: false });
                    }
                }
            }
            if parsed.done {
                tracing::debug!(reason = ?parsed.done_reason, "ollama chunk done");
                return Ok(StreamOutcome { content: full, completed: true });
            }
        }
    }
    Ok(StreamOutcome { content: full, completed: false })
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

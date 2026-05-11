use std::sync::Arc;

use actix_web::{web, HttpResponse};
use actix_web_lab::sse;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::auth::AuthUser;
use crate::comfyui;
use crate::ollama::{self, ChatMessage};
use crate::personas;
use crate::storage::StorageError;
use crate::AppState;

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub upstream: bool,
    pub model_locked: bool,
    pub auth: &'static str,
    pub refiner_available: bool,
    /// True when an img2img (image-edit) backend is wired up. Today
    /// that's ComfyUI Kontext via COMFYUI_URL. The UI uses this to
    /// surface an "img2img" indicator on attachment-bearing image turns,
    /// since the picker model is bypassed in that branch.
    pub img2img_available: bool,
    /// True when WHISPER_URL is set so the UI can show the mic button
    /// for voice-to-text input. Backend forwards audio to whisper.cpp.
    pub voice_in_available: bool,
    /// True when PIPER_URL is set so the UI can show the read-aloud
    /// affordance on assistant messages.
    pub voice_out_available: bool,
}

pub async fn status(state: web::Data<Arc<AppState>>) -> HttpResponse {
    let upstream = ollama::list_models(&state).await.is_ok();
    let auth = if state.settings.dev_auth {
        "dev"
    } else if state.settings.oidc.is_some() {
        "oidc"
    } else {
        "none"
    };
    HttpResponse::Ok().json(StatusResponse {
        upstream,
        model_locked: state.settings.ollama_model_lock.is_some(),
        auth,
        refiner_available: state.settings.prompt_refiner_model.is_some(),
        img2img_available: state.settings.comfyui_url.is_some(),
        voice_in_available: state.settings.whisper_url.is_some(),
        voice_out_available: state.settings.piper_url.is_some(),
    })
}

#[derive(Debug, Deserialize)]
pub struct CapsQuery {
    pub model: String,
}

pub async fn model_caps(
    state: web::Data<Arc<AppState>>,
    _user: AuthUser,
    q: web::Query<CapsQuery>,
) -> HttpResponse {
    let key = q.model.clone();
    if let Some(cached) = state.caps_cache.get(&key).await {
        return HttpResponse::Ok().json(cached);
    }
    match ollama::show_capabilities(&state, &key).await {
        Ok(caps) => {
            state.caps_cache.set(key, caps.clone()).await;
            HttpResponse::Ok().json(caps)
        }
        Err(e) => {
            tracing::warn!("show_capabilities failed for {key}: {e}");
            HttpResponse::BadGateway().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

pub async fn list_models(state: web::Data<Arc<AppState>>) -> HttpResponse {
    if let Some(locked) = &state.settings.ollama_model_lock {
        return HttpResponse::Ok().json(serde_json::json!({
            "models": [{ "name": locked, "locked": true }]
        }));
    }
    match ollama::list_models(&state).await {
        Ok(v) => HttpResponse::Ok().json(v),
        Err(e) => {
            tracing::error!("list_models upstream failed: {e}");
            HttpResponse::BadGateway().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateConvBody {
    pub title: Option<String>,
    pub model: Option<String>,
}

pub async fn list_conversations(
    state: web::Data<Arc<AppState>>,
    user: AuthUser,
) -> HttpResponse {
    match state.storage.list_conversations(&user.sub) {
        Ok(rows) => HttpResponse::Ok().json(rows),
        Err(e) => storage_err(e),
    }
}

pub async fn create_conversation(
    state: web::Data<Arc<AppState>>,
    user: AuthUser,
    body: web::Json<CreateConvBody>,
) -> HttpResponse {
    let title = body.title.clone().unwrap_or_else(|| "new chat".into());
    match state
        .storage
        .create_conversation(&user.sub, &title, body.model.as_deref())
    {
        Ok(c) => HttpResponse::Ok().json(c),
        Err(e) => storage_err(e),
    }
}

pub async fn delete_conversation(
    state: web::Data<Arc<AppState>>,
    user: AuthUser,
    path: web::Path<String>,
) -> HttpResponse {
    match state.storage.delete_conversation(&user.sub, &path) {
        Ok(()) => HttpResponse::NoContent().finish(),
        Err(e) => storage_err(e),
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdateConvBody {
    /// Manual title override. Trimmed; rejected when empty or wildly
    /// long so a stray paste can't fill the sidebar with a paragraph.
    pub title: Option<String>,
}

pub async fn update_conversation(
    state: web::Data<Arc<AppState>>,
    user: AuthUser,
    path: web::Path<String>,
    body: web::Json<UpdateConvBody>,
) -> HttpResponse {
    let id = path.into_inner();
    if let Some(raw) = &body.title {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return HttpResponse::BadRequest()
                .json(serde_json::json!({"error": "title must not be empty"}));
        }
        if trimmed.chars().count() > 120 {
            return HttpResponse::BadRequest()
                .json(serde_json::json!({"error": "title too long (max 120 chars)"}));
        }
        if let Err(e) = state
            .storage
            .set_conversation_title(&user.sub, &id, trimmed)
        {
            return storage_err(e);
        }
    }
    match state.storage.get_conversation(&user.sub, &id) {
        Ok(c) => HttpResponse::Ok().json(c),
        Err(e) => storage_err(e),
    }
}

#[derive(Debug, Serialize)]
struct MessageDto {
    id: i64,
    role: String,
    content: String,
    created_at: i64,
    /// Number of image attachments. Bytes are fetched lazily via
    /// `/api/conversations/{cid}/messages/{mid}/image/{idx}` so chat list
    /// responses stay small.
    #[serde(skip_serializing_if = "is_zero")]
    image_count: usize,
    status: String,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

fn message_to_dto(m: crate::storage::Message) -> MessageDto {
    MessageDto {
        id: m.id,
        role: m.role,
        content: m.content,
        created_at: m.created_at,
        image_count: m.image_count,
        status: m.status,
    }
}

pub async fn get_messages(
    state: web::Data<Arc<AppState>>,
    user: AuthUser,
    path: web::Path<String>,
) -> HttpResponse {
    match state.storage.list_messages(&user.sub, &path) {
        Ok(rows) => {
            let dtos: Vec<MessageDto> = rows.into_iter().map(message_to_dto).collect();
            HttpResponse::Ok().json(dtos)
        }
        Err(e) => storage_err(e),
    }
}

pub async fn get_message_image(
    state: web::Data<Arc<AppState>>,
    user: AuthUser,
    req: actix_web::HttpRequest,
    path: web::Path<(String, i64, usize)>,
) -> HttpResponse {
    let (conv_id, msg_id, idx) = path.into_inner();
    let etag = format!("\"m{msg_id}i{idx}\"");
    if let Some(h) = req.headers().get("if-none-match") {
        if h.to_str().map(|v| v == etag).unwrap_or(false) {
            return HttpResponse::NotModified().finish();
        }
    }
    match state
        .storage
        .get_message_image_bytes(&user.sub, &conv_id, msg_id, idx)
    {
        Ok((bytes, mime)) => HttpResponse::Ok()
            .content_type(mime)
            .insert_header(("Cache-Control", "private, max-age=86400, immutable"))
            .insert_header(("ETag", etag))
            .body(bytes),
        Err(e) => storage_err(e),
    }
}

pub async fn delete_message_from(
    state: web::Data<Arc<AppState>>,
    user: AuthUser,
    path: web::Path<(String, i64)>,
) -> HttpResponse {
    let (conv_id, msg_id) = path.into_inner();
    match state
        .storage
        .delete_message_and_after(&user.sub, &conv_id, msg_id)
    {
        Ok(()) => HttpResponse::NoContent().finish(),
        Err(e) => storage_err(e),
    }
}

/// Cancel a pending image generation. Used by the UI when the user clicks
/// stop on a row whose original SSE connection isn't held by the current
/// tab (e.g. after a reload). Posts `/interrupt` to ComfyUI and drops the
/// placeholder row. The original SSE task — if still alive elsewhere —
/// will see its Storage update no-op, then notice the prompt missing from
/// `/history` and exit.
pub async fn cancel_pending_message(
    state: web::Data<Arc<AppState>>,
    user: AuthUser,
    path: web::Path<(String, i64)>,
) -> HttpResponse {
    let (conv_id, msg_id) = path.into_inner();
    comfyui::interrupt_active(state.get_ref()).await;
    match state
        .storage
        .delete_message_and_after(&user.sub, &conv_id, msg_id)
    {
        Ok(()) => HttpResponse::NoContent().finish(),
        Err(e) => storage_err(e),
    }
}

#[derive(Debug, Deserialize)]
pub struct ChatBody {
    pub conv_id: String,
    pub content: String,
    pub model: Option<String>,
    /// Base64-encoded images attached to this user turn (vision models
    /// only). Stripped automatically if the resolved model isn't vision-
    /// capable.
    #[serde(default)]
    pub images: Option<Vec<String>>,
    /// "chat" (default) or "image". When "image", the prompt is forwarded
    /// to Ollama's image generation endpoint instead of the chat stream.
    #[serde(default)]
    pub mode: Option<String>,
    /// For image mode only. When true (default) the configured refiner
    /// model rewrites the user's prompt before generation. When false,
    /// the user's prompt is sent verbatim and the refiner instead
    /// describes the generated image so subsequent turns have context.
    #[serde(default)]
    pub refine: Option<bool>,
    /// Persona id selecting the voice the refiner adopts. See
    /// `personas::list()`. Unknown / missing → default persona.
    #[serde(default)]
    pub persona: Option<String>,
    /// Retry mode: when set, the backend deletes this assistant row
    /// (assumed `status='error'` or stuck pending) and re-runs generation
    /// off the existing latest user message instead of appending a new
    /// user turn. Used by the in-bubble "retry" button so the original
    /// user prompt + attached image stays in place.
    #[serde(default)]
    pub retry_assistant_id: Option<i64>,
}

pub async fn list_personas() -> HttpResponse {
    HttpResponse::Ok().json(personas::list())
}

/// List voices currently loaded on the piper-tts daemon. Proxies the
/// upstream's own `/voices` endpoint so the UI can pick a voice that
/// actually exists instead of guessing.
pub async fn list_voices(state: web::Data<Arc<AppState>>, _user: AuthUser) -> HttpResponse {
    let Some(base) = state.settings.piper_url.as_deref() else {
        return HttpResponse::Ok().json(serde_json::json!({"voices": []}));
    };
    match state
        .http_client
        .get(format!("{}/voices", base.trim_end_matches('/')))
        .send()
        .await
    {
        Ok(res) if res.status().is_success() => match res.json::<serde_json::Value>().await {
            Ok(v) => HttpResponse::Ok().json(v),
            Err(e) => {
                tracing::warn!("piper /voices decode failed: {e}");
                HttpResponse::Ok().json(serde_json::json!({"voices": []}))
            }
        },
        Ok(res) => {
            tracing::warn!("piper /voices upstream {}", res.status());
            HttpResponse::Ok().json(serde_json::json!({"voices": []}))
        }
        Err(e) => {
            tracing::warn!("piper /voices request failed: {e}");
            HttpResponse::Ok().json(serde_json::json!({"voices": []}))
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct TranscribeQuery {
    /// Optional ISO-639-1 language hint forwarded to whisper.cpp.
    /// Omitting it lets the model auto-detect.
    pub lang: Option<String>,
}

/// Forward an audio blob to whisper.cpp's `/inference` endpoint and
/// return the transcript. The frontend records via MediaRecorder (webm
/// /opus by default) and POSTs the raw blob with its Content-Type
/// header; we wrap that into a multipart form whisper.cpp expects.
pub async fn transcribe(
    state: web::Data<Arc<AppState>>,
    user: AuthUser,
    req: actix_web::HttpRequest,
    query: web::Query<TranscribeQuery>,
    body: web::Bytes,
) -> HttpResponse {
    if state.settings.chat_rate_per_min > 0 && !state.chat_limit.check(&user.sub) {
        return HttpResponse::TooManyRequests()
            .insert_header(("Retry-After", "60"))
            .json(serde_json::json!({"error": "too many requests"}));
    }
    let Some(base) = state.settings.whisper_url.as_deref() else {
        return HttpResponse::ServiceUnavailable()
            .json(serde_json::json!({"error": "voice input not configured"}));
    };
    if body.is_empty() {
        return HttpResponse::BadRequest()
            .json(serde_json::json!({"error": "empty audio"}));
    }
    let content_type = req
        .headers()
        .get("content-type")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("audio/webm")
        .to_string();
    let (mime, filename) = match content_type.as_str() {
        s if s.starts_with("audio/webm") => ("audio/webm", "recording.webm"),
        s if s.starts_with("audio/ogg") => ("audio/ogg", "recording.ogg"),
        s if s.starts_with("audio/wav") || s.starts_with("audio/wave") => {
            ("audio/wav", "recording.wav")
        }
        s if s.starts_with("audio/mp4") || s.starts_with("audio/m4a") => {
            ("audio/mp4", "recording.m4a")
        }
        s if s.starts_with("audio/mpeg") => ("audio/mpeg", "recording.mp3"),
        _ => ("audio/webm", "recording.webm"),
    };
    let part = match reqwest::multipart::Part::bytes(body.to_vec())
        .file_name(filename)
        .mime_str(mime)
    {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("whisper part build failed: {e}");
            return HttpResponse::InternalServerError().finish();
        }
    };
    let mut form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("response_format", "json");
    if let Some(lang) = query.lang.as_deref().filter(|s| !s.is_empty()) {
        form = form.text("language", lang.to_string());
    }
    let request = state
        .http_client
        .post(format!("{}/inference", base.trim_end_matches('/')))
        .multipart(form);
    let res = match request.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("whisper request failed: {e}");
            return HttpResponse::BadGateway()
                .json(serde_json::json!({"error": e.to_string()}));
        }
    };
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        tracing::warn!("whisper upstream {status}: {text}");
        return HttpResponse::BadGateway()
            .json(serde_json::json!({"error": format!("upstream {status}")}));
    }
    let parsed: serde_json::Value = match res.json().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("whisper decode failed: {e}");
            return HttpResponse::BadGateway()
                .json(serde_json::json!({"error": "malformed upstream response"}));
        }
    };
    let text = parsed
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    HttpResponse::Ok().json(serde_json::json!({ "text": text }))
}

#[derive(Debug, Deserialize)]
pub struct TtsBody {
    pub text: String,
    /// Piper voice slug — e.g. `en_US-amy-medium`, `fi_FI-harri-medium`.
    /// Forwarded verbatim; omitted when empty so the upstream uses its
    /// configured default voice.
    #[serde(default)]
    pub voice: Option<String>,
}

/// Synthesize speech via piper-tts and return the resulting WAV. The
/// upstream's HTTP API is `POST /` with `{"text": "..."}` returning the
/// audio body directly — we proxy that, set a precise Content-Type so
/// the browser's <audio> element handles it natively, and clip
/// pathological inputs so a giant assistant turn doesn't churn the
/// synth for minutes.
pub async fn tts(
    state: web::Data<Arc<AppState>>,
    user: AuthUser,
    body: web::Json<TtsBody>,
) -> HttpResponse {
    if state.settings.chat_rate_per_min > 0 && !state.chat_limit.check(&user.sub) {
        return HttpResponse::TooManyRequests()
            .insert_header(("Retry-After", "60"))
            .json(serde_json::json!({"error": "too many requests"}));
    }
    let Some(base) = state.settings.piper_url.as_deref() else {
        return HttpResponse::ServiceUnavailable()
            .json(serde_json::json!({"error": "tts not configured"}));
    };
    let text = body.text.trim();
    if text.is_empty() {
        return HttpResponse::BadRequest()
            .json(serde_json::json!({"error": "empty text"}));
    }
    // 8000 chars ≈ several minutes of speech — well above any sensible
    // assistant turn and tight enough to keep piper from melting on a
    // pathological paste.
    let clipped: String = text.chars().take(8_000).collect();
    let mut payload = serde_json::json!({ "text": clipped });
    if let Some(voice) = body.voice.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload["voice"] = serde_json::Value::String(voice.to_string());
    }
    let res = match state
        .http_client
        .post(format!("{}/", base.trim_end_matches('/')))
        .json(&payload)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("piper request failed: {e}");
            return HttpResponse::BadGateway()
                .json(serde_json::json!({"error": e.to_string()}));
        }
    };
    if !res.status().is_success() {
        let status = res.status();
        let snippet = res.text().await.unwrap_or_default();
        tracing::warn!("piper upstream {status}: {snippet}");
        return HttpResponse::BadGateway()
            .json(serde_json::json!({"error": format!("upstream {status}")}));
    }
    let bytes = match res.bytes().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("piper body read failed: {e}");
            return HttpResponse::BadGateway()
                .json(serde_json::json!({"error": "malformed upstream response"}));
        }
    };
    HttpResponse::Ok()
        .content_type("audio/wav")
        .insert_header(("Cache-Control", "no-store"))
        .body(bytes)
}

pub async fn chat(
    state: web::Data<Arc<AppState>>,
    user: AuthUser,
    body: web::Json<ChatBody>,
) -> Result<sse::Sse<ReceiverStream<Result<sse::Event, std::convert::Infallible>>>, actix_web::Error>
{
    if state.settings.chat_rate_per_min > 0 && !state.chat_limit.check(&user.sub) {
        return Err(rate_limited_actix());
    }
    let conv = state
        .storage
        .get_conversation(&user.sub, &body.conv_id)
        .map_err(storage_actix_err)?;

    let images: Vec<String> = body.images.clone().unwrap_or_default();
    if let Some(retry_id) = body.retry_assistant_id {
        // Drop the failed assistant row; the prior user message stays in
        // place and becomes the prompt source for this turn. Title was
        // set on the original send, no rename here.
        state
            .storage
            .delete_message_and_after(&user.sub, &conv.id, retry_id)
            .map_err(storage_actix_err)?;
    } else {
        state
            .storage
            .append_message(&user.sub, &conv.id, "user", &body.content, &images)
            .map_err(storage_actix_err)?;

        let title_seed = body.content.chars().take(60).collect::<String>();
        let _ = state.storage.rename_if_default(&conv.id, title_seed.trim());
    }

    let history = state
        .storage
        .list_messages(&user.sub, &conv.id)
        .map_err(storage_actix_err)?;
    // Drop in-flight (pending) rows so we never feed the model a turn that
    // hasn't completed yet. Only carry images on user turns — assistant
    // image-gen rows hold PNG bytes we don't want re-sent to chat models.
    // Images for user-role rows are fetched from message_images and base64
    // -encoded only when forwarding to Ollama (no longer sit in the in-
    // memory message list).
    let messages: Vec<ChatMessage> = history
        .into_iter()
        .filter(|m| m.status != "pending")
        .map(|m| {
            let is_user = m.role == "user";
            let images = if is_user && m.image_count > 0 {
                match state.storage.get_message_images_b64(m.id) {
                    Ok(v) if !v.is_empty() => Some(v),
                    Ok(_) => None,
                    Err(e) => {
                        tracing::warn!("history image load failed for msg {}: {e}", m.id);
                        None
                    }
                }
            } else {
                None
            };
            ChatMessage {
                role: m.role,
                content: m.content,
                images,
            }
        })
        .collect();

    // Resolve the model in this priority: env-locked → client-supplied →
    // existing conversation model → fallback. Then persist on the
    // conversation so subsequent turns and reopens default to it.
    let requested = body.model.as_deref().or(conv.model.as_deref());
    let model = ollama::resolve_model(&state, requested);
    if state.settings.ollama_model_lock.is_none()
        && conv.model.as_deref() != Some(model.as_str())
    {
        if let Err(e) = state
            .storage
            .set_conversation_model(&user.sub, &conv.id, &model)
        {
            tracing::warn!("failed to persist conversation model: {e}");
        }
    }
    let (tx, rx) = mpsc::channel::<Result<sse::Event, std::convert::Infallible>>(64);

    let user_sub = user.sub.clone();
    let conv_id = conv.id.clone();
    let user_first = body.content.clone();
    let model_for_rename = model.clone();
    let state_clone: Arc<AppState> = state.get_ref().clone();
    let image_mode = body.mode.as_deref() == Some("image");

    if image_mode {
        let pending_id = state
            .storage
            .append_pending_assistant(&user.sub, &conv.id)
            .map_err(storage_actix_err)?;
        let prompt = body.content.clone();
        let model_for_gen = model.clone();
        let refiner_model = state.settings.prompt_refiner_model.clone();
        let refiner_history = messages.clone();
        // Default refine on; only honour the client toggle when a refiner is
        // actually configured server-side.
        let refine_enabled = refiner_model.is_some() && body.refine.unwrap_or(true);
        let persona_system = personas::system_prompt(body.persona.as_deref());
        // Route to ComfyUI Kontext when the user attached one or more
        // reference images AND a ComfyUI host is configured. Otherwise
        // fall back to the Ollama text→image path even with an
        // attachment (Ollama will ignore it on a non-vision image
        // model). Multiple attachments stack as additional Kontext
        // references via chained ReferenceLatent nodes.
        let kontext_inputs: Vec<String> = match (
            state.settings.comfyui_url.as_ref(),
            body.images.as_ref(),
        ) {
            (Some(_), Some(v)) if !v.is_empty() => v.clone(),
            _ => Vec::new(),
        };
        let use_kontext = !kontext_inputs.is_empty();
        let image_sem = state.image_sem.clone();
        tokio::spawn(async move {
            // Hold a permit for the duration of this image job. With the
            // default semaphore size of 1 this serialises GPU work so we
            // don't OOM when two clients send at once. Emit a one-shot
            // `queued` SSE event when we couldn't take a permit instantly
            // so the UI can show a waiting-for-gpu state. Permit drops on
            // return via `_permit`; if the semaphore is closed the job is
            // silently abandoned — should never happen in practice.
            let _permit = match image_sem.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(tokio::sync::TryAcquireError::NoPermits) => {
                    let _ = tx
                        .send(Ok(ollama::sse_json(
                            "queued",
                            &serde_json::json!({}),
                        )))
                        .await;
                    match image_sem.acquire_owned().await {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::error!("image semaphore closed: {e}");
                            return;
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("image semaphore closed: {e}");
                    return;
                }
            };
            // VRAM coordination: Kontext FP8 (~12 GB) cannot share host
            // memory with chat / refiner models on a 24 GB box. Tell
            // Ollama to drop the chat model before invoking ComfyUI.
            // Evict only after holding the permit so concurrent senders
            // don't trigger redundant eviction churn while queued.
            if use_kontext {
                ollama::evict(&state_clone, &model_for_gen).await;
            }
            let refined = if refine_enabled {
                match refiner_model.as_deref() {
                    Some(m) => {
                        let r = ollama::refine_image_prompt(
                            state_clone.clone(),
                            m,
                            &persona_system,
                            &refiner_history,
                        )
                        .await;
                        if use_kontext {
                            ollama::evict(&state_clone, m).await;
                        }
                        match r {
                            Ok(r) if !r.is_empty() => Some(r),
                            Ok(_) => None,
                            Err(e) => {
                                tracing::warn!(
                                    "prompt refinement failed: {e} (using original)"
                                );
                                None
                            }
                        }
                    }
                    None => None,
                }
            } else {
                None
            };

            // When refinement is off, fall back to feeding the latest prior
            // assistant caption as context so follow-ups have something to
            // build on. Caption text comes from earlier describe_image runs
            // (or earlier refined prompts).
            let context_prefix = if refine_enabled {
                None
            } else {
                refiner_history
                    .iter()
                    .rev()
                    .find(|m| m.role == "assistant" && !m.content.is_empty())
                    .map(|m| m.content.clone())
            };
            let composed_prompt = match (&refined, &context_prefix) {
                (Some(r), _) => r.clone(),
                (None, Some(ctx)) => format!(
                    "Context from the previous image: {ctx}\n\nNew request: {prompt}"
                ),
                (None, None) => prompt.clone(),
            };
            let final_prompt = composed_prompt.as_str();

            // Wire cancellation: when the SSE client disconnects, the
            // mpsc receiver drops and `tx.closed()` resolves. We feed
            // that into ComfyUI's poll loop so we can stop wasting GPU
            // cycles on a render no one is waiting for.
            let cancel_tx = tx.clone();
            let cancel_fut = async move {
                cancel_tx.closed().await;
            };
            // Forward live progress + preview frames from ComfyUI's
            // WebSocket onto the same SSE channel. `try_send` is fire-
            // and-forget — preview frames are noisy and dropping a few
            // when the client is slow is preferable to head-of-line
            // blocking the actual generation result.
            let progress_tx = tx.clone();
            let progress_cb: comfyui::ProgressCallback = std::sync::Arc::new(
                move |evt: comfyui::ProgressEvent| {
                    let payload = match evt {
                        comfyui::ProgressEvent::Progress { value, max } => ollama::sse_json(
                            "progress",
                            &serde_json::json!({"value": value, "max": max}),
                        ),
                        comfyui::ProgressEvent::Preview { mime, b64 } => ollama::sse_json(
                            "preview",
                            &serde_json::json!({"mime": mime, "b64": b64}),
                        ),
                    };
                    let _ = progress_tx.try_send(Ok(payload));
                },
            );
            let gen_result = if !kontext_inputs.is_empty() {
                comfyui::generate_kontext(
                    &state_clone,
                    final_prompt,
                    &kontext_inputs,
                    cancel_fut,
                    Some(progress_cb),
                )
                .await
            } else {
                ollama::generate_image(&state_clone, &model_for_gen, final_prompt).await
            };
            match gen_result {
                Ok(b64) => {
                    // Caption either holds the refined prompt (refine on) or
                    // a vision description of what was rendered (refine off
                    // + refiner available) so the next turn has context.
                    let caption = if let Some(r) = refined {
                        r
                    } else if let Some(m) = refiner_model.as_deref() {
                        let d = match ollama::describe_image(
                            state_clone.clone(),
                            m,
                            &b64,
                        )
                        .await
                        {
                            Ok(d) => d,
                            Err(e) => {
                                tracing::warn!("image description failed: {e}");
                                String::new()
                            }
                        };
                        if use_kontext {
                            ollama::evict(&state_clone, m).await;
                        }
                        d
                    } else {
                        String::new()
                    };
                    if let Err(e) = state_clone.storage.complete_message(
                        pending_id,
                        &caption,
                        std::slice::from_ref(&b64),
                    ) {
                        tracing::error!("failed to persist generated image: {e}");
                    }
                    let _ = tx
                        .send(Ok(ollama::sse_json(
                            "done",
                            &serde_json::json!({"conv_id": conv_id}),
                        )))
                        .await;
                }
                Err(ollama::ChatStreamError::Cancelled) => {
                    // Client went away (stop button, tab close, navigation).
                    // Drop the placeholder row entirely so a retry doesn't
                    // pile up half-finished bubbles. The ComfyUI worker has
                    // already been told to interrupt inside generate_kontext.
                    if let Err(e) = state_clone.storage.delete_message_and_after(
                        &user_sub,
                        &conv_id,
                        pending_id,
                    ) {
                        tracing::warn!(
                            "failed to drop cancelled pending row {pending_id}: {e}"
                        );
                    }
                }
                Err(e) => {
                    let public = friendly_image_error(&e);
                    tracing::error!("image generation error: {e}");
                    if let Err(persist_err) =
                        state_clone.storage.fail_message(pending_id, &public)
                    {
                        tracing::error!("failed to mark message errored: {persist_err}");
                    }
                    let _ = tx
                        .send(Ok(ollama::sse_json(
                            "error",
                            &serde_json::json!({"message": public}),
                        )))
                        .await;
                }
            }
            // Match ollama's keep_alive:0 pattern — POST /free after every
            // ComfyUI job (success, error, or cancellation) so Kontext +
            // T5 + CLIP + VAE don't sit resident between requests. Next
            // job reloads from disk in ~10–15 s but the chat / refiner
            // models get the RAM back in the meantime.
            if use_kontext {
                comfyui::free_memory(&state_clone).await;
            }
        });

        let stream = ReceiverStream::new(rx);
        return Ok(sse::Sse::from_stream(stream).with_keep_alive(std::time::Duration::from_secs(30)));
    }

    tokio::spawn(async move {
        let tx_for_delta = tx.clone();
        // Race the upstream stream against `tx.closed()` so a client that
        // disconnects (stop button, navigation, tab close) tears down the
        // reqwest stream immediately. Without this, Ollama keeps generating
        // until the next chunk write hits a broken pipe — which can be a
        // multi-second hold on bigger models.
        let tx_closed = tx.clone();
        let result = tokio::select! {
            r = ollama::stream_chat(state_clone.clone(), &model, messages, |delta| {
                tx_for_delta.try_send(Ok(ollama::sse_delta(delta))).is_ok()
            }) => r,
            _ = tx_closed.closed() => {
                tracing::info!("chat SSE client gone — dropping upstream stream");
                return;
            }
        };

        match result {
            Ok(outcome) => {
                if !outcome.content.is_empty() {
                    if let Err(e) = state_clone.storage.append_message(
                        &user_sub,
                        &conv_id,
                        "assistant",
                        &outcome.content,
                        &[],
                    ) {
                        tracing::error!("failed to persist assistant message: {e}");
                    }
                }
                if outcome.completed {
                    if let Some(stats) = outcome.stats {
                        let _ = tx
                            .send(Ok(ollama::sse_json(
                                "stats",
                                &serde_json::json!({
                                    "tokens": stats.tokens,
                                    "prompt_tokens": stats.prompt_tokens,
                                    "tokens_per_sec": stats.tokens_per_sec,
                                }),
                            )))
                            .await;
                    }
                    let _ = tx
                        .send(Ok(ollama::sse_json(
                            "done",
                            &serde_json::json!({"conv_id": conv_id}),
                        )))
                        .await;

                    // After the very first complete turn (user + assistant
                    // = 2 messages), ask the model for a 3-6 word title and
                    // overwrite the eager truncate. Failures are non-fatal.
                    let count = state_clone
                        .storage
                        .list_messages(&user_sub, &conv_id)
                        .map(|v| v.len())
                        .unwrap_or(0);
                    if count == 2 && !outcome.content.is_empty() {
                        let st = state_clone.clone();
                        let usub = user_sub.clone();
                        let cid = conv_id.clone();
                        let m = model_for_rename.clone();
                        let user_msg = user_first.clone();
                        let asst_msg = outcome.content.clone();
                        tokio::spawn(async move {
                            match ollama::summarize_title(st.clone(), &m, &user_msg, &asst_msg)
                                .await
                            {
                                Ok(title) if !title.is_empty() => {
                                    if let Err(e) =
                                        st.storage.set_conversation_title(&usub, &cid, &title)
                                    {
                                        tracing::warn!("auto-rename persist failed: {e}");
                                    } else {
                                        tracing::info!("auto-renamed {cid} → {title:?}");
                                    }
                                }
                                Ok(_) => {}
                                Err(e) => tracing::warn!("auto-rename summary failed: {e}"),
                            }
                        });
                    }
                } else {
                    tracing::info!(
                        "stream stopped early (client gone or user stop), persisted {} chars",
                        outcome.content.len()
                    );
                }
            }
            Err(e) => {
                tracing::error!("stream error: {e}");
                let _ = tx
                    .send(Ok(ollama::sse_json(
                        "error",
                        &serde_json::json!({"message": e.to_string()}),
                    )))
                    .await;
            }
        }
    });

    let stream = ReceiverStream::new(rx);
    Ok(sse::Sse::from_stream(stream).with_keep_alive(std::time::Duration::from_secs(30)))
}

fn storage_err(e: StorageError) -> HttpResponse {
    match e {
        StorageError::NotFound => HttpResponse::NotFound().finish(),
        StorageError::Forbidden => HttpResponse::Forbidden().finish(),
        StorageError::Invalid(msg) => HttpResponse::BadRequest().json(serde_json::json!({
            "error": msg,
        })),
        StorageError::Sqlite(err) => {
            tracing::error!("sqlite error: {err}");
            HttpResponse::InternalServerError().finish()
        }
    }
}

fn friendly_image_error(e: &ollama::ChatStreamError) -> String {
    match e {
        ollama::ChatStreamError::EmptyImage => {
            "image generator returned no data — the upstream runner likely \
             crashed. check ollama logs."
                .to_string()
        }
        ollama::ChatStreamError::ComfyTimeout => {
            "image edit timed out — the comfyui job didn't finish in time"
                .to_string()
        }
        ollama::ChatStreamError::Cancelled => {
            // Reachable only if the friendly path is invoked for a Cancelled
            // result (it currently isn't — the handler short-circuits earlier).
            "cancelled".to_string()
        }
        ollama::ChatStreamError::Http(err) => {
            if err.is_timeout() {
                "image generation timed out".to_string()
            } else if err.is_decode() {
                "upstream returned an unreadable response — image runner may \
                 have crashed mid-stream"
                    .to_string()
            } else if let Some(status) = err.status() {
                format!("upstream returned {status}")
            } else {
                format!("upstream error: {err}")
            }
        }
    }
}

fn rate_limited_actix() -> actix_web::Error {
    actix_web::error::InternalError::from_response(
        "rate limited",
        HttpResponse::TooManyRequests()
            .insert_header(("Retry-After", "60"))
            .json(serde_json::json!({"error": "too many requests"})),
    )
    .into()
}

fn storage_actix_err(e: StorageError) -> actix_web::Error {
    match e {
        StorageError::NotFound => actix_web::error::ErrorNotFound("not found"),
        StorageError::Forbidden => actix_web::error::ErrorForbidden("forbidden"),
        StorageError::Invalid(msg) => actix_web::error::ErrorBadRequest(msg),
        StorageError::Sqlite(err) => {
            tracing::error!("sqlite error: {err}");
            actix_web::error::ErrorInternalServerError("storage error")
        }
    }
}

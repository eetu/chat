use std::sync::Arc;

use actix_web::{web, HttpResponse};
use actix_web_lab::sse;
use futures_util::TryStreamExt;
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
    /// True when EMBEDDING_MODEL is set so the UI can offer document
    /// uploads and surface a RAG indicator.
    pub rag_available: bool,
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
    // RAG is gated on Ollama actually exposing at least one
    // embedding-capable model. Cached coarsely so /status polls don't
    // re-walk /api/show for every model every 20 seconds. Upstream
    // failures fall back to "not available" rather than poisoning the
    // surface with a stale-true.
    const EMBED_TTL: std::time::Duration = std::time::Duration::from_secs(60);
    let rag_available = {
        let cached = {
            let guard = state.embed_models_available.lock().await;
            guard.and_then(|(at, value)| {
                if at.elapsed() < EMBED_TTL {
                    Some(value)
                } else {
                    None
                }
            })
        };
        match cached {
            Some(v) => v,
            None => {
                let probed = ollama::list_embedding_models(&state)
                    .await
                    .map(|m| !m.is_empty())
                    .unwrap_or(false);
                *state.embed_models_available.lock().await =
                    Some((std::time::Instant::now(), probed));
                probed
            }
        }
    };
    HttpResponse::Ok().json(StatusResponse {
        upstream,
        model_locked: state.settings.ollama_model_lock.is_some(),
        auth,
        refiner_available: state.settings.prompt_refiner_model.is_some(),
        img2img_available: state.settings.comfyui_url.is_some(),
        voice_in_available: state.settings.whisper_url.is_some(),
        voice_out_available: state.settings.piper_url.is_some(),
        rag_available,
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
    let raw = match ollama::list_models(&state).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("list_models upstream failed: {e}");
            return HttpResponse::BadGateway()
                .json(serde_json::json!({"error": e.to_string()}));
        }
    };
    // Filter out embedding-only models — they have no chat surface and
    // just clutter the picker. Capability lookups go through the
    // existing caps cache so this stays cheap on warm paths.
    let mut out: Vec<serde_json::Value> = Vec::new();
    if let Some(arr) = raw.get("models").and_then(|v| v.as_array()) {
        for m in arr {
            let Some(name) = m.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            let caps = if let Some(c) = state.caps_cache.get(name).await {
                c
            } else {
                match ollama::show_capabilities(&state, name).await {
                    Ok(c) => {
                        state
                            .caps_cache
                            .set(name.to_string(), c.clone())
                            .await;
                        c
                    }
                    Err(_) => ollama::ModelCapabilities::default(),
                }
            };
            let has_embedding = caps.capabilities.iter().any(|c| c == "embedding");
            let has_chat = caps.capabilities.is_empty()
                || caps.capabilities.iter().any(|c| c == "completion");
            if has_embedding && !has_chat {
                continue;
            }
            out.push(m.clone());
        }
    }
    HttpResponse::Ok().json(serde_json::json!({ "models": out }))
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
    /// True when this row has an inpaint mask attached, fetchable via
    /// `/api/conversations/{cid}/messages/{mid}/mask`. Drives the
    /// mask-overlay rendering in the UI without a HEAD probe.
    #[serde(skip_serializing_if = "is_false")]
    has_mask: bool,
    status: String,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn message_to_dto(m: crate::storage::Message) -> MessageDto {
    MessageDto {
        id: m.id,
        role: m.role,
        content: m.content,
        created_at: m.created_at,
        image_count: m.image_count,
        has_mask: m.has_mask,
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

/// Fetch the inpaint mask attached to a single message. Same shape as
/// `get_message_image` but reads the kind='mask' row instead. Used by
/// the edit / regenerate paths so the resend can re-supply the mask to
/// `/api/chat` without round-tripping through the user.
pub async fn get_message_mask(
    state: web::Data<Arc<AppState>>,
    user: AuthUser,
    req: actix_web::HttpRequest,
    path: web::Path<(String, i64)>,
) -> HttpResponse {
    let (conv_id, msg_id) = path.into_inner();
    let etag = format!("\"m{msg_id}-mask\"");
    if let Some(h) = req.headers().get("if-none-match") {
        if h.to_str().map(|v| v == etag).unwrap_or(false) {
            return HttpResponse::NotModified().finish();
        }
    }
    match state
        .storage
        .get_message_mask_bytes(&user.sub, &conv_id, msg_id)
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
    /// Regenerate-from-user mode: when set, the backend trims every
    /// row strictly after this user turn (the assistant reply, if
    /// any, and anything later) and re-runs generation off the
    /// existing user message. Used by the regenerate button under
    /// user bubbles so the user can rerun even when no assistant
    /// reply landed (e.g. they hit stop pre-stream).
    #[serde(default)]
    pub regenerate_from_user: Option<i64>,
    /// Image-mode sub-routing. `"txt2img"` and `"img2img"` are derivable
    /// from `images` alone, but `"inpaint"` carries a mask alongside a
    /// single base image and needs an explicit signal so the handler
    /// doesn't have to guess intent. Omitting it preserves the old
    /// "images.len() decides" behaviour. Ignored when `mode != "image"`.
    #[serde(default)]
    pub sub_mode: Option<String>,
    /// Base64-encoded inpaint mask aligned to `images[0]` — white pixels
    /// (red channel) mark the region to repaint, black pixels keep the
    /// original. Required when `sub_mode == "inpaint"`. Ignored
    /// otherwise.
    #[serde(default)]
    pub mask: Option<String>,
    /// Override for the negative-prompt branch of the inpaint workflow.
    /// When present and non-empty, takes precedence over the refiner's
    /// auto-generated negative. Empty / missing leaves the field to the
    /// refiner (when enabled) or to "" (no negative). Only effective on
    /// workflows that run real CFG — Flux Fill / inpaint today; Kontext
    /// at cfg=1 ignores the negative branch entirely.
    #[serde(default)]
    pub negative: Option<String>,
}

pub async fn list_personas() -> HttpResponse {
    HttpResponse::Ok().json(personas::list())
}

#[derive(Debug, Deserialize)]
pub struct DocumentUploadBody {
    pub name: String,
    pub mime: Option<String>,
    /// Base64-encoded raw bytes of the file. Backend sniffs the
    /// content (PDF vs UTF-8 text) and extracts text accordingly.
    pub content_b64: String,
    /// Optional override of the embedding model. Falls back to
    /// `EMBEDDING_MODEL` from settings when unset.
    pub model: Option<String>,
}

pub async fn list_embedding_models(
    state: web::Data<Arc<AppState>>,
    _user: AuthUser,
) -> HttpResponse {
    match ollama::list_embedding_models(&state).await {
        Ok(models) => HttpResponse::Ok().json(serde_json::json!({ "models": models })),
        Err(e) => {
            tracing::warn!("list_embedding_models failed: {e}");
            HttpResponse::BadGateway()
                .json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// Ingest a text document for retrieval-augmented chat: chunk, embed
/// each chunk via Ollama, persist alongside the document row. Accepts
/// raw text in JSON for now — PDF / markdown stays a follow-up.
pub async fn upload_document(
    state: web::Data<Arc<AppState>>,
    user: AuthUser,
    body: web::Json<DocumentUploadBody>,
) -> HttpResponse {
    let model = match body
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(m) => m.to_string(),
        None => match state.settings.embedding_model.as_deref() {
            Some(m) => m.to_string(),
            None => {
                return HttpResponse::BadRequest().json(serde_json::json!({
                    "error": "embedding model required — pick one in settings",
                }));
            }
        },
    };
    if state.settings.chat_rate_per_min > 0 && !state.chat_limit.check(&user.sub) {
        return HttpResponse::TooManyRequests()
            .insert_header(("Retry-After", "60"))
            .json(serde_json::json!({"error": "too many requests"}));
    }
    let name = body.name.trim();
    if name.is_empty() {
        return HttpResponse::BadRequest()
            .json(serde_json::json!({"error": "name required"}));
    }
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    let raw = match B64.decode(body.content_b64.as_bytes()) {
        Ok(b) => b,
        Err(e) => {
            return HttpResponse::BadRequest().json(
                serde_json::json!({"error": format!("base64 decode: {e}")}),
            );
        }
    };
    if raw.is_empty() {
        return HttpResponse::BadRequest()
            .json(serde_json::json!({"error": "empty document"}));
    }
    if raw.len() > state.settings.max_document_bytes {
        let mb = state.settings.max_document_bytes / (1024 * 1024);
        return HttpResponse::PayloadTooLarge()
            .json(serde_json::json!({
                "error": format!("file exceeds {mb} MB cap"),
            }));
    }
    // Sniff PDF magic, otherwise decode as UTF-8 text. PDF extraction
    // is best-effort: image-only PDFs (no embedded text layer) come
    // back empty and the upload is rejected below.
    let is_pdf = raw.starts_with(b"%PDF-");
    let (extracted_text, detected_mime): (String, &'static str) = if is_pdf {
        match pdf_extract::extract_text_from_mem(&raw) {
            Ok(t) => (t, "application/pdf"),
            Err(e) => {
                tracing::warn!("pdf extract failed for {name}: {e}");
                return HttpResponse::BadRequest()
                    .json(serde_json::json!({"error": "could not extract pdf text"}));
            }
        }
    } else {
        match std::str::from_utf8(&raw) {
            Ok(s) => (s.to_string(), "text/plain"),
            Err(_) => {
                return HttpResponse::BadRequest()
                    .json(serde_json::json!({"error": "unsupported binary format"}));
            }
        }
    };
    let content = extracted_text.trim();
    if content.is_empty() {
        return HttpResponse::BadRequest()
            .json(serde_json::json!({"error": "no extractable text"}));
    }
    let mime = body
        .mime
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(detected_mime)
        .to_string();

    let chunks = crate::rag::chunk_text(content);
    if chunks.is_empty() {
        return HttpResponse::BadRequest()
            .json(serde_json::json!({"error": "nothing to index"}));
    }

    let mut embedded: Vec<(String, Vec<f32>)> = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        match crate::ollama::embed_text(&state, &model, &chunk).await {
            Ok(vec) if !vec.is_empty() => embedded.push((chunk, vec)),
            Ok(_) => {
                tracing::warn!("embed_text returned empty vector; skipping chunk");
            }
            Err(e) => {
                tracing::error!("embed_text failed: {e}");
                return HttpResponse::BadGateway()
                    .json(serde_json::json!({"error": format!("embed failed: {e}")}));
            }
        }
    }

    if embedded.is_empty() {
        return HttpResponse::BadGateway()
            .json(serde_json::json!({"error": "no chunks embedded"}));
    }

    let doc_id = match state.storage.create_document(
        &user.sub,
        name,
        &mime,
        raw.len() as i64,
        &model,
    ) {
        Ok(id) => id,
        Err(e) => return storage_err(e),
    };
    if let Err(e) = state.storage.insert_chunks(doc_id, &embedded) {
        // Best-effort rollback: drop the doc row so the user doesn't
        // see a half-ingested entry in the list.
        let _ = state.storage.delete_document(&user.sub, doc_id);
        return storage_err(e);
    }
    match state.storage.list_documents(&user.sub) {
        Ok(docs) => {
            let me = docs.into_iter().find(|d| d.id == doc_id);
            HttpResponse::Ok().json(me)
        }
        Err(e) => storage_err(e),
    }
}

pub async fn list_documents(
    state: web::Data<Arc<AppState>>,
    user: AuthUser,
) -> HttpResponse {
    match state.storage.list_documents(&user.sub) {
        Ok(docs) => HttpResponse::Ok().json(docs),
        Err(e) => storage_err(e),
    }
}

pub async fn delete_document(
    state: web::Data<Arc<AppState>>,
    user: AuthUser,
    path: web::Path<i64>,
) -> HttpResponse {
    match state.storage.delete_document(&user.sub, path.into_inner()) {
        Ok(()) => HttpResponse::NoContent().finish(),
        Err(e) => storage_err(e),
    }
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Full-text search across the user's messages. Each hit references
/// the source conversation + a short snippet so the UI can render a
/// command-palette style result list without a second fetch.
pub async fn search(
    state: web::Data<Arc<AppState>>,
    user: AuthUser,
    q: web::Query<SearchQuery>,
) -> HttpResponse {
    let trimmed = q.q.trim();
    if trimmed.is_empty() {
        return HttpResponse::Ok().json(serde_json::json!({ "hits": [] }));
    }
    // 30 is plenty for a palette — beyond that the list scrolls past
    // useful and the FTS5 ranker's tail isn't very informative.
    let limit = q.limit.unwrap_or(20).clamp(1, 50);
    match state.storage.search(&user.sub, trimmed, limit) {
        Ok(hits) => HttpResponse::Ok().json(serde_json::json!({ "hits": hits })),
        Err(e) => storage_err(e),
    }
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
    // Request opus over ogg from the upstream — ~32 kbps VBR for voice
    // is roughly 10× smaller than raw PCM WAV and decodes inside the
    // browser's MediaSource path so playback starts as soon as the
    // first packets land.
    let res = match state
        .http_client
        .post(format!("{}/", base.trim_end_matches('/')))
        .query(&[("format", "opus")])
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
    // Stream the upstream body through to the client instead of
    // buffering. The piper server emits chunked WAV (RIFF header with
    // size = 0xFFFFFFFF) — or whatever transcoded codec is configured
    // — so the browser can start decoding while later samples are
    // still being synthesised. Buffering here would defeat the
    // streaming entirely on long replies. The upstream Content-Type
    // is forwarded verbatim so the frontend can pick MediaSource when
    // it's a codec the browser supports.
    let upstream_ct = res
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("audio/wav")
        .to_string();
    let stream = res
        .bytes_stream()
        .map_err(|e| actix_web::error::ErrorBadGateway(e.to_string()));
    HttpResponse::Ok()
        .content_type(upstream_ct)
        .insert_header(("Cache-Control", "no-store"))
        .streaming(stream)
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
    let mask: Option<String> = body
        .mask
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let sub_mode = body
        .sub_mode
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let inpaint_requested = sub_mode == Some("inpaint") || mask.is_some();
    if inpaint_requested {
        if body.mode.as_deref() != Some("image") {
            return Err(actix_web::error::ErrorBadRequest(
                "inpaint requires mode=\"image\"",
            ));
        }
        if images.len() != 1 {
            return Err(actix_web::error::ErrorBadRequest(
                "inpaint requires exactly one base image",
            ));
        }
        if mask.is_none() {
            return Err(actix_web::error::ErrorBadRequest(
                "inpaint requires a mask",
            ));
        }
    }
    if let Some(retry_id) = body.retry_assistant_id {
        // Drop the failed assistant row; the prior user message stays in
        // place and becomes the prompt source for this turn. Title was
        // set on the original send, no rename here.
        state
            .storage
            .delete_message_and_after(&user.sub, &conv.id, retry_id)
            .map_err(storage_actix_err)?;
    } else if let Some(user_id) = body.regenerate_from_user {
        // Trim everything after this user turn and re-run generation
        // off it. The user row itself stays put.
        state
            .storage
            .delete_messages_after(&user.sub, &conv.id, user_id)
            .map_err(storage_actix_err)?;
    } else {
        state
            .storage
            .append_message(
                &user.sub,
                &conv.id,
                "user",
                &body.content,
                &images,
                mask.as_deref(),
            )
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
    // Captured outside the spawn so the closure can read the override
    // without borrowing the request body.
    let body_negative: Option<String> = body
        .negative
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

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
        // Image-mode dispatch. Three branches share the same SSE
        // plumbing below:
        // - inpaint  → ComfyUI Flux Fill with a base + mask (the
        //              request validation above guarantees exactly one
        //              image and a present mask).
        // - kontext  → ComfyUI Flux Kontext with ≥1 reference image.
        // - txt2img  → ComfyUI Z-Image Turbo (no attachments).
        // All three run on ComfyUI; there is no Ollama image fallback.
        let comfy_available = state.settings.comfyui_url.is_some();
        if !comfy_available {
            // No ComfyUI host → no image generation. Mark the pending
            // row errored and surface a clean SSE error rather than
            // falling back to Ollama's unstable imagegen path.
            if let Err(e) = state
                .storage
                .fail_message(pending_id, "image generation unavailable")
            {
                tracing::error!("failed to mark image message errored: {e}");
            }
            let _ = tx
                .send(Ok(ollama::sse_json(
                    "error",
                    &serde_json::json!({"message": "image generation unavailable"}),
                )))
                .await;
            let stream = ReceiverStream::new(rx);
            return Ok(
                sse::Sse::from_stream(stream).with_keep_alive(std::time::Duration::from_secs(30))
            );
        }
        let kontext_inputs: Vec<String> = match (comfy_available, body.images.as_ref()) {
            (true, Some(v)) if !v.is_empty() => v.clone(),
            _ => Vec::new(),
        };
        let inpaint_inputs: Option<(String, String)> = match (
            comfy_available,
            inpaint_requested,
            kontext_inputs.first(),
            mask.as_ref(),
        ) {
            (true, true, Some(base), Some(m)) => Some((base.clone(), m.clone())),
            _ => None,
        };
        // Every image path now runs on ComfyUI (inpaint / kontext /
        // txt2img), so the evict-before + free-after VRAM dance applies
        // unconditionally. Guaranteed true here — !comfy_available
        // returned above.
        let use_comfy = comfy_available;
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
            // VRAM coordination: Flux Kontext / Fill (~12 GB) cannot
            // share host memory with chat / refiner models on a 24 GB
            // box. Tell Ollama to drop the chat model before invoking
            // ComfyUI. Evict only after holding the permit so concurrent
            // senders don't trigger redundant eviction churn while queued.
            if use_comfy {
                ollama::evict(&state_clone, &model_for_gen).await;
            }
            let refined: Option<ollama::RefinedPrompt> = if refine_enabled {
                match refiner_model.as_deref() {
                    Some(m) => {
                        let r = ollama::refine_image_prompt(
                            state_clone.clone(),
                            m,
                            &persona_system,
                            &refiner_history,
                        )
                        .await;
                        if use_comfy {
                            ollama::evict(&state_clone, m).await;
                        }
                        match r {
                            Ok(r) if !r.positive.is_empty() => Some(r),
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
            let composed_prompt = match (refined.as_ref(), &context_prefix) {
                (Some(r), _) => r.positive.clone(),
                (None, Some(ctx)) => format!(
                    "Context from the previous image: {ctx}\n\nNew request: {prompt}"
                ),
                (None, None) => prompt.clone(),
            };
            // Negative-prompt precedence: explicit client override wins;
            // otherwise the refiner's generated negative (when refine
            // ran); otherwise a baseline default so image gen always has
            // one. Inpaint and Z-Image txt2img both run real CFG, so the
            // negative influences sampling. Kontext still squashes it
            // (cfg=1) but accepts the argument harmlessly.
            let negative_text: String = body_negative
                .clone()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| {
                    refined
                        .as_ref()
                        .map(|r| r.negative.clone())
                        .filter(|s| !s.trim().is_empty())
                })
                .unwrap_or_else(|| personas::DEFAULT_NEGATIVE.to_string());
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
            let gen_result = if let Some((base, mask_bytes)) = inpaint_inputs {
                comfyui::generate_inpaint(
                    &state_clone,
                    final_prompt,
                    &negative_text,
                    &base,
                    &mask_bytes,
                    chat_shared::DEFAULT_INPAINT_STEPS,
                    cancel_fut,
                    Some(progress_cb),
                )
                .await
            } else if !kontext_inputs.is_empty() {
                comfyui::generate_kontext(
                    &state_clone,
                    final_prompt,
                    &kontext_inputs,
                    chat_shared::DEFAULT_KONTEXT_STEPS,
                    cancel_fut,
                    Some(progress_cb),
                )
                .await
            } else {
                // No reference image / mask → pure text-to-image via
                // ComfyUI Z-Image Turbo. The refiner's generated
                // negative now feeds a real-CFG path (cfg 2.0), so the
                // negative actually influences the render.
                comfyui::generate_txt2img(
                    &state_clone,
                    final_prompt,
                    &negative_text,
                    chat_shared::DEFAULT_TXT2IMG_STEPS,
                    cancel_fut,
                    Some(progress_cb),
                )
                .await
            };
            match gen_result {
                Ok(b64) => {
                    // Caption either holds the refined prompt (refine on) or
                    // a vision description of what was rendered (refine off
                    // + refiner available) so the next turn has context.
                    let caption = if let Some(r) = refined {
                        r.positive
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
                        if use_comfy {
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
                    // First try the clean path — drop the placeholder row
                    // so a retry doesn't pile up half-finished bubbles.
                    // Falls back to marking the row errored when delete
                    // can't see it (observed in prod with a misleading
                    // "not found"): without the fallback the row stays
                    // stuck pending forever and the UI keeps polling it,
                    // re-triggering the leave-confirm on every nav.
                    match state_clone.storage.delete_message_and_after(
                        &user_sub,
                        &conv_id,
                        pending_id,
                    ) {
                        Ok(()) => {}
                        Err(e) => {
                            tracing::warn!(
                                "delete after cancel failed for msg {pending_id} \
                                 (user={user_sub}, conv={conv_id}): {e}; \
                                 falling back to fail_message"
                            );
                            if let Err(e2) =
                                state_clone.storage.fail_message(pending_id, "cancelled")
                            {
                                tracing::error!(
                                    "fail_message fallback failed for msg {pending_id}: {e2}"
                                );
                            }
                        }
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
            if use_comfy {
                comfyui::free_memory(&state_clone).await;
            }
        });

        let stream = ReceiverStream::new(rx);
        return Ok(sse::Sse::from_stream(stream).with_keep_alive(std::time::Duration::from_secs(30)));
    }

    let rag_user_text = body.content.clone();
    tokio::spawn(async move {
        let tx_for_delta = tx.clone();
        // RAG injection: if the user owns any documents, embed the new
        // user turn with each distinct embedding model used at upload
        // time (cross-model vectors aren't comparable), rank chunks by
        // cosine, then merge into a single top-k system message.
        // Failures stay non-fatal — the assistant just sees the prompt
        // without retrieved context.
        let mut messages = messages;
        let stored = state_clone.storage.load_user_chunks(&user_sub).ok();
        if let Some(chunks) = stored.filter(|c| !c.is_empty()) {
            use std::collections::HashMap;
            let mut by_model: HashMap<String, Vec<&crate::storage::StoredChunk>> =
                HashMap::new();
            for ch in &chunks {
                if ch.embedding_model.is_empty() {
                    continue;
                }
                by_model
                    .entry(ch.embedding_model.clone())
                    .or_default()
                    .push(ch);
            }
            let mut ranked: Vec<(f32, &crate::storage::StoredChunk)> = Vec::new();
            for (model_name, group) in by_model {
                match ollama::embed_text(
                    &state_clone,
                    &model_name,
                    &rag_user_text,
                )
                .await
                {
                    Ok(query_vec) if !query_vec.is_empty() => {
                        for ch in group {
                            ranked.push((
                                crate::rag::cosine(&query_vec, &ch.embedding),
                                ch,
                            ));
                        }
                    }
                    Ok(_) => {
                        tracing::warn!(
                            "rag: empty query embedding for {model_name}"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            "rag: embed failed ({model_name}): {e}"
                        );
                    }
                }
            }
            ranked.sort_by(|a, b| {
                b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
            });
            let top: Vec<_> = ranked
                .into_iter()
                .filter(|(score, _)| *score > 0.3)
                .take(state_clone.settings.rag_top_k)
                .collect();
            if !top.is_empty() {
                let mut prompt = String::from(
                    "relevant context from the user's documents — cite the \
                     source name when you reference it:\n\n",
                );
                for (_, ch) in &top {
                    prompt.push_str(&format!(
                        "[{}]\n{}\n\n",
                        ch.document_name, ch.content
                    ));
                }
                tracing::debug!(count = top.len(), "rag: injected chunks");
                // Surface which documents were consulted so the UI can
                // show a sources chip under the assistant turn. Dedup
                // by name + report the best matching score per doc.
                let mut sources: Vec<(String, f32)> = Vec::new();
                for (score, ch) in &top {
                    if let Some(entry) = sources
                        .iter_mut()
                        .find(|(name, _)| name == &ch.document_name)
                    {
                        if *score > entry.1 {
                            entry.1 = *score;
                        }
                    } else {
                        sources.push((ch.document_name.clone(), *score));
                    }
                }
                let sources_json: Vec<serde_json::Value> = sources
                    .iter()
                    .map(|(name, score)| {
                        serde_json::json!({
                            "name": name,
                            "score": score,
                        })
                    })
                    .collect();
                let _ = tx
                    .send(Ok(ollama::sse_json(
                        "context",
                        &serde_json::json!({ "sources": sources_json }),
                    )))
                    .await;
                messages.insert(
                    0,
                    ChatMessage {
                        role: "system".into(),
                        content: prompt,
                        images: None,
                    },
                );
            }
        }
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
                        None,
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

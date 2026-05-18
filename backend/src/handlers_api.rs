//! Stateless `/api/v1/*` endpoints exposed to the MCP bridge.
//!
//! These differ from the session-cookie-gated chat surface in three ways:
//!
//! 1. Auth is a static Bearer token (`CHAT_MCP_API_KEY`) via the
//!    `ApiKey` extractor. No per-user data isolation — this is a
//!    system-level credential, not a user session.
//! 2. Nothing is persisted. Generation goes straight from ComfyUI to
//!    the SSE response; the caller (an MCP server) decides what to do
//!    with the bytes.
//! 3. The chat handler couples generation to conversation lifecycle —
//!    auto-rename, pending rows, the cancel route. None of that
//!    applies here. The SSE stream IS the request lifecycle.
//!
//! The SSE shape mirrors the chat surface so a future frontend could
//! share parsing code, but the event set is a strict subset:
//! `progress`, `preview`, `done`, `error`.
//!
//! Concurrency: jobs hold the same `image_sem` permit as chat-driven
//! image gen so an MCP caller can't bypass the GPU serialisation and
//! OOM the host. If a permit isn't immediately available, a `queued`
//! event is emitted just like the chat path does.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use actix_web::{web, HttpResponse};
use actix_web_lab::sse;
use chat_shared::{
    ErrorPayload, ImageModelEntry, ImageModelsResponse, ImageResponse, Img2ImgRequest,
    InpaintRequest, PreviewPayload, ProgressPayload, Txt2ImgRequest, DEFAULT_INPAINT_STEPS,
    DEFAULT_KONTEXT_STEPS, MAX_INPAINT_STEPS, MAX_KONTEXT_STEPS, MIN_STEPS,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::auth::ApiKey;
use crate::comfyui;
use crate::ollama::{self, ChatStreamError};
use crate::AppState;
use uuid::Uuid;

/// SSE channel depth. Large enough to absorb a burst of preview frames
/// without dropping when the consumer is slow but small enough that a
/// stalled consumer doesn't pin a megabyte of latent previews in RAM.
const SSE_CHANNEL: usize = 16;

/// POST /api/v1/img2img
///
/// Flux Kontext reference-image edit. Returns an SSE stream emitting
/// `progress` / `preview` events until the render completes, then a
/// `done` event with the rendered PNG as base64. On failure a final
/// `error` event is emitted with a human-readable message before the
/// stream closes.
pub async fn img2img(
    state: web::Data<Arc<AppState>>,
    _auth: ApiKey,
    body: web::Json<Img2ImgRequest>,
) -> Result<sse::Sse<ReceiverStream<Result<sse::Event, Infallible>>>, actix_web::Error> {
    if state.settings.comfyui_url.is_none() {
        return Err(actix_web::error::ErrorServiceUnavailable(
            "comfyui not configured",
        ));
    }
    if body.images.is_empty() {
        return Err(actix_web::error::ErrorBadRequest(
            "images: at least one image required",
        ));
    }
    if body.prompt.trim().is_empty() {
        return Err(actix_web::error::ErrorBadRequest("prompt: required"));
    }

    let steps = clamp_steps(body.steps, DEFAULT_KONTEXT_STEPS, MAX_KONTEXT_STEPS);
    let (tx, rx) = mpsc::channel::<Result<sse::Event, Infallible>>(SSE_CHANNEL);

    let state_clone = state.get_ref().clone();
    let payload = body.into_inner();
    tokio::spawn(async move {
        let _permit = match acquire_image_permit(&state_clone, &tx).await {
            Some(p) => p,
            None => return,
        };
        // Note: chat handler evicts the just-finished Ollama model
        // before invoking ComfyUI because it knows which one is hot.
        // The MCP surface doesn't — eviction would need an enumerate
        // step via /api/ps. For now we let Ollama's own keep_alive
        // timer reclaim memory; if MCP traffic starts colliding with
        // chat traffic on the 24 GB host, add an evict_all helper.
        let progress_cb = make_progress_cb(tx.clone());
        let cancel_tx = tx.clone();
        let cancel_fut = cancel_tx.closed();
        let result = comfyui::generate_kontext(
            &state_clone,
            &payload.prompt,
            &payload.images,
            steps,
            cancel_fut,
            Some(progress_cb),
        )
        .await;
        finish(&tx, &state_clone, result).await;
    });

    Ok(sse_stream(rx))
}

/// POST /api/v1/txt2img
///
/// Pure text-to-image generation via Ollama's
/// `/v1/images/generations`. Returns the same SSE event shape as the
/// ComfyUI paths, minus `progress` / `preview` — Ollama's image
/// surface isn't streaming, so the agent sees `queued` (if applicable)
/// followed by `done` or `error`.
///
/// `model` is optional; falls back to `ollama::resolve_model(None)`
/// (which itself prefers `OLLAMA_MODEL`).
pub async fn txt2img(
    state: web::Data<Arc<AppState>>,
    _auth: ApiKey,
    body: web::Json<Txt2ImgRequest>,
) -> Result<sse::Sse<ReceiverStream<Result<sse::Event, Infallible>>>, actix_web::Error> {
    if body.prompt.trim().is_empty() {
        return Err(actix_web::error::ErrorBadRequest("prompt: required"));
    }

    let (tx, rx) = mpsc::channel::<Result<sse::Event, Infallible>>(SSE_CHANNEL);
    let state_clone = state.get_ref().clone();
    let payload = body.into_inner();
    let model = ollama::resolve_model(&state_clone, payload.model.as_deref());

    tokio::spawn(async move {
        let _permit = match acquire_image_permit(&state_clone, &tx).await {
            Some(p) => p,
            None => return,
        };
        // Race against the SSE client disconnect. Ollama has no
        // public cancel; dropping the reqwest future closes TCP, so
        // the bytes never land in the agent's pipe even though the
        // upstream may keep generating server-side until done.
        let cancel_tx = tx.clone();
        let result: Result<String, ChatStreamError> = tokio::select! {
            r = ollama::generate_image(&state_clone, &model, &payload.prompt) => r,
            _ = cancel_tx.closed() => {
                tracing::info!("api/v1/txt2img cancelled by client");
                Err(ChatStreamError::Cancelled)
            }
        };
        match result {
            Ok(image_b64) => {
                let uuid = state_clone
                    .image_buffer
                    .insert(&image_b64, state_clone.settings.image_buffer_limit)
                    .await
                    .map(|id| id.to_string())
                    .unwrap_or_else(|e| {
                        tracing::warn!(
                            "image buffer insert failed (returning inline only): {e}"
                        );
                        String::new()
                    });
                let _ = tx
                    .send(Ok(ollama::sse_json(
                        "done",
                        &serde_json::to_value(ImageResponse { image_b64, uuid })
                            .unwrap_or_default(),
                    )))
                    .await;
            }
            Err(e) => {
                let msg = match &e {
                    ChatStreamError::Cancelled => "cancelled".to_string(),
                    ChatStreamError::ComfyTimeout => "comfyui timed out".to_string(),
                    ChatStreamError::EmptyImage => "no image produced".to_string(),
                    ChatStreamError::Http(err) => format!("upstream error: {err}"),
                };
                tracing::warn!("api/v1/txt2img failed: {e}");
                let _ = tx
                    .send(Ok(ollama::sse_json(
                        "error",
                        &serde_json::to_value(ErrorPayload { message: msg })
                            .unwrap_or_default(),
                    )))
                    .await;
            }
        }
        // Free the model so the next request — txt2img, img2img, or a
        // chat turn — isn't fighting for VRAM. Chat handler does the
        // same after its image path.
        ollama::evict(&state_clone, &model).await;
    });

    Ok(sse_stream(rx))
}

/// GET /api/v1/images/{uuid}.png
///
/// Streams the bytes of a previously-rendered image from the in-memory
/// buffer. The `.png` suffix is cosmetic — it makes the URL look like
/// a file when curl'd into `-O`, but the lookup ignores it.
///
/// 404 on unknown / expired ids. The blob's TTL is set by
/// `CHAT_IMAGE_BUFFER_TTL_SECS` (default 30 min).
pub async fn get_image(
    state: web::Data<Arc<AppState>>,
    _auth: ApiKey,
    path: web::Path<String>,
) -> HttpResponse {
    let raw = path.into_inner();
    // Tolerate `<uuid>.png` and bare `<uuid>` — the extension is for
    // URL aesthetics and tools that infer mime from filename.
    let id_str = raw.strip_suffix(".png").unwrap_or(&raw);
    let Ok(id) = Uuid::parse_str(id_str) else {
        return HttpResponse::NotFound().finish();
    };
    let ttl = Duration::from_secs(state.settings.image_buffer_ttl_secs);
    match state.image_buffer.get(id, ttl).await {
        Some(blob) => HttpResponse::Ok()
            .content_type(blob.mime)
            .insert_header((
                "Cache-Control",
                "private, max-age=300, must-revalidate",
            ))
            .body(blob.bytes),
        None => HttpResponse::NotFound().finish(),
    }
}

/// GET /api/v1/models/image
///
/// Returns the subset of installed Ollama models that advertise the
/// `image` capability via `/api/show`. Caps go through the existing
/// `CapsCache` so this stays cheap when called repeatedly. The agent
/// is expected to call this once before invoking `txt2img` so it can
/// pick a real model name rather than guessing one.
pub async fn list_image_models(
    state: web::Data<Arc<AppState>>,
    _auth: ApiKey,
) -> HttpResponse {
    if let Some(locked) = &state.settings.ollama_model_lock {
        // Locked model takes the same shortcut as the chat surface.
        // Whether it's image-capable depends on the deployment; we
        // surface it regardless so the agent can attempt a call.
        return HttpResponse::Ok().json(ImageModelsResponse {
            models: vec![ImageModelEntry {
                name: locked.clone(),
                families: vec![],
            }],
        });
    }
    let raw = match ollama::list_models(&state).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("api/v1 list_image_models upstream failed: {e}");
            return HttpResponse::BadGateway()
                .json(serde_json::json!({"error": e.to_string()}));
        }
    };
    let mut out: Vec<ImageModelEntry> = Vec::new();
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
                        state.caps_cache.set(name.to_string(), c.clone()).await;
                        c
                    }
                    Err(_) => ollama::ModelCapabilities::default(),
                }
            };
            if caps.capabilities.iter().any(|c| c == "image") {
                out.push(ImageModelEntry {
                    name: name.to_string(),
                    families: caps.families.clone(),
                });
            }
        }
    }
    HttpResponse::Ok().json(ImageModelsResponse { models: out })
}

/// POST /api/v1/inpaint
///
/// Flux Fill masked repaint. Same SSE shape as `img2img`.
pub async fn inpaint(
    state: web::Data<Arc<AppState>>,
    _auth: ApiKey,
    body: web::Json<InpaintRequest>,
) -> Result<sse::Sse<ReceiverStream<Result<sse::Event, Infallible>>>, actix_web::Error> {
    if state.settings.comfyui_url.is_none() {
        return Err(actix_web::error::ErrorServiceUnavailable(
            "comfyui not configured",
        ));
    }
    if body.prompt.trim().is_empty() {
        return Err(actix_web::error::ErrorBadRequest("prompt: required"));
    }
    if body.image.is_empty() || body.mask.is_empty() {
        return Err(actix_web::error::ErrorBadRequest(
            "image and mask are required",
        ));
    }

    let steps = clamp_steps(body.steps, DEFAULT_INPAINT_STEPS, MAX_INPAINT_STEPS);
    let (tx, rx) = mpsc::channel::<Result<sse::Event, Infallible>>(SSE_CHANNEL);

    let state_clone = state.get_ref().clone();
    let payload = body.into_inner();
    let negative = payload.negative_prompt.unwrap_or_default();
    tokio::spawn(async move {
        let _permit = match acquire_image_permit(&state_clone, &tx).await {
            Some(p) => p,
            None => return,
        };
        // Note: chat handler evicts the just-finished Ollama model
        // before invoking ComfyUI because it knows which one is hot.
        // The MCP surface doesn't — eviction would need an enumerate
        // step via /api/ps. For now we let Ollama's own keep_alive
        // timer reclaim memory; if MCP traffic starts colliding with
        // chat traffic on the 24 GB host, add an evict_all helper.
        let progress_cb = make_progress_cb(tx.clone());
        let cancel_tx = tx.clone();
        let cancel_fut = cancel_tx.closed();
        let result = comfyui::generate_inpaint(
            &state_clone,
            &payload.prompt,
            &negative,
            &payload.image,
            &payload.mask,
            steps,
            cancel_fut,
            Some(progress_cb),
        )
        .await;
        finish(&tx, &state_clone, result).await;
    });

    Ok(sse_stream(rx))
}

fn clamp_steps(requested: Option<u32>, default: u32, max: u32) -> u32 {
    requested.unwrap_or(default).clamp(MIN_STEPS, max)
}

fn sse_stream(
    rx: mpsc::Receiver<Result<sse::Event, Infallible>>,
) -> sse::Sse<ReceiverStream<Result<sse::Event, Infallible>>> {
    sse::Sse::from_stream(ReceiverStream::new(rx))
        .with_keep_alive(std::time::Duration::from_secs(15))
}

/// Hold an `image_sem` permit for the lifetime of one job. Emits a
/// `queued` SSE event when the permit isn't immediately available so
/// the MCP bridge can surface a "waiting for GPU" status to the agent.
/// Returns `None` (and the spawned task should exit) when the
/// semaphore has been closed — only happens at shutdown.
async fn acquire_image_permit(
    state: &Arc<AppState>,
    tx: &mpsc::Sender<Result<sse::Event, Infallible>>,
) -> Option<tokio::sync::OwnedSemaphorePermit> {
    let sem = state.image_sem.clone();
    match sem.clone().try_acquire_owned() {
        Ok(p) => Some(p),
        Err(tokio::sync::TryAcquireError::NoPermits) => {
            let _ = tx
                .send(Ok(ollama::sse_json("queued", &serde_json::json!({}))))
                .await;
            match sem.acquire_owned().await {
                Ok(p) => Some(p),
                Err(e) => {
                    tracing::error!("image semaphore closed: {e}");
                    None
                }
            }
        }
        Err(e) => {
            tracing::error!("image semaphore closed: {e}");
            None
        }
    }
}

/// Build a progress callback that forwards ComfyUI's WebSocket events
/// onto the SSE channel. `try_send` is used so a slow consumer drops
/// preview frames rather than back-pressuring sampling — the agent
/// gets the final image regardless and previews are best-effort UX.
fn make_progress_cb(
    tx: mpsc::Sender<Result<sse::Event, Infallible>>,
) -> comfyui::ProgressCallback {
    Arc::new(move |evt| {
        let payload = match evt {
            comfyui::ProgressEvent::Progress { value, max } => ollama::sse_json(
                "progress",
                &serde_json::to_value(ProgressPayload { value, max }).unwrap_or_default(),
            ),
            comfyui::ProgressEvent::Preview { mime, b64 } => ollama::sse_json(
                "preview",
                &serde_json::to_value(PreviewPayload {
                    mime: mime.to_string(),
                    b64,
                })
                .unwrap_or_default(),
            ),
        };
        let _ = tx.try_send(Ok(payload));
    })
}

/// Common tail for both endpoints: translate the generator result into
/// a final `done`/`error` SSE event and trigger ComfyUI's `/free` so
/// the model unloads between MCP calls (matching chat handler
/// behaviour). Memory free is best-effort — failures only warn.
async fn finish(
    tx: &mpsc::Sender<Result<sse::Event, Infallible>>,
    state: &Arc<AppState>,
    result: Result<String, ChatStreamError>,
) {
    match result {
        Ok(image_b64) => {
            // Stash the rendered PNG in the in-memory buffer so the
            // caller can fetch it via GET /api/v1/images/{uuid}.png
            // instead of inlining the base64 in their context.
            let uuid = match state
                .image_buffer
                .insert(&image_b64, state.settings.image_buffer_limit)
                .await
            {
                Ok(id) => id.to_string(),
                Err(e) => {
                    tracing::warn!(
                        "image buffer insert failed (returning inline only): {e}"
                    );
                    String::new()
                }
            };
            let body = ImageResponse { image_b64, uuid };
            let _ = tx
                .send(Ok(ollama::sse_json(
                    "done",
                    &serde_json::to_value(body).unwrap_or_default(),
                )))
                .await;
        }
        Err(e) => {
            let msg = match &e {
                ChatStreamError::Cancelled => "cancelled".to_string(),
                ChatStreamError::ComfyTimeout => "comfyui timed out".to_string(),
                ChatStreamError::EmptyImage => "no image produced".to_string(),
                ChatStreamError::Http(err) => format!("upstream error: {err}"),
            };
            tracing::warn!("api/v1 image job failed: {e}");
            let _ = tx
                .send(Ok(ollama::sse_json(
                    "error",
                    &serde_json::to_value(ErrorPayload { message: msg })
                        .unwrap_or_default(),
                )))
                .await;
        }
    }
    comfyui::free_memory(state).await;
}


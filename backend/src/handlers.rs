use std::sync::Arc;

use actix_web::{web, HttpResponse};
use actix_web_lab::sse;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::auth::AuthUser;
use crate::ollama::{self, ChatMessage};
use crate::storage::StorageError;
use crate::AppState;

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub upstream: bool,
    pub model_locked: bool,
    pub auth: &'static str,
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

pub async fn get_messages(
    state: web::Data<Arc<AppState>>,
    user: AuthUser,
    path: web::Path<String>,
) -> HttpResponse {
    match state.storage.list_messages(&user.sub, &path) {
        Ok(rows) => HttpResponse::Ok().json(rows),
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
}

pub async fn chat(
    state: web::Data<Arc<AppState>>,
    user: AuthUser,
    body: web::Json<ChatBody>,
) -> Result<sse::Sse<ReceiverStream<Result<sse::Event, std::convert::Infallible>>>, actix_web::Error>
{
    let conv = state
        .storage
        .get_conversation(&user.sub, &body.conv_id)
        .map_err(storage_actix_err)?;

    let images: Vec<String> = body.images.clone().unwrap_or_default();
    state
        .storage
        .append_message(&user.sub, &conv.id, "user", &body.content, &images)
        .map_err(storage_actix_err)?;

    let title_seed = body.content.chars().take(60).collect::<String>();
    let _ = state.storage.rename_if_default(&conv.id, title_seed.trim());

    let history = state
        .storage
        .list_messages(&user.sub, &conv.id)
        .map_err(storage_actix_err)?;
    // Drop in-flight (pending) rows so we never feed the model a turn that
    // hasn't completed yet. Only carry images on user turns — assistant
    // image-gen rows hold base64 PNGs we don't want re-sent to chat models.
    let messages: Vec<ChatMessage> = history
        .into_iter()
        .filter(|m| m.status != "pending")
        .map(|m| {
            let is_user = m.role == "user";
            ChatMessage {
                role: m.role,
                content: m.content,
                images: if is_user && !m.images.is_empty() {
                    Some(m.images)
                } else {
                    None
                },
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
        tokio::spawn(async move {
            let refined = match refiner_model.as_deref() {
                Some(m) => match ollama::refine_image_prompt(
                    state_clone.clone(),
                    m,
                    &refiner_history,
                )
                .await
                {
                    Ok(r) if !r.is_empty() => Some(r),
                    Ok(_) => None,
                    Err(e) => {
                        tracing::warn!("prompt refinement failed: {e} (using original)");
                        None
                    }
                },
                None => None,
            };
            let final_prompt = refined.as_deref().unwrap_or(prompt.as_str());

            match ollama::generate_image(&state_clone, &model_for_gen, final_prompt).await {
                Ok(b64) => {
                    let caption = refined.unwrap_or_default();
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
        });

        let stream = ReceiverStream::new(rx);
        return Ok(sse::Sse::from_stream(stream).with_keep_alive(std::time::Duration::from_secs(30)));
    }

    tokio::spawn(async move {
        let tx_for_delta = tx.clone();
        let result = ollama::stream_chat(state_clone.clone(), &model, messages, |delta| {
            tx_for_delta.try_send(Ok(ollama::sse_delta(delta))).is_ok()
        })
        .await;

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

fn storage_actix_err(e: StorageError) -> actix_web::Error {
    match e {
        StorageError::NotFound => actix_web::error::ErrorNotFound("not found"),
        StorageError::Forbidden => actix_web::error::ErrorForbidden("forbidden"),
        StorageError::Sqlite(err) => {
            tracing::error!("sqlite error: {err}");
            actix_web::error::ErrorInternalServerError("storage error")
        }
    }
}

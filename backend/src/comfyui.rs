//! ComfyUI client for Flux Kontext img2img.
//!
//! ComfyUI exposes a JSON-graph "workflow" API: a request describes the
//! node graph, returns a `prompt_id`, and outputs land in `/history/{id}`
//! once the worker has rendered them. We embed a fixed Kontext graph
//! template here and only substitute the prompt text, the uploaded
//! reference image filename, and a random sampler seed.
//!
//! Memory note: Kontext FP8 (~12 GB) cannot coexist with chat / refiner
//! models (~18 GB) on a 24 GB unified-memory host. The caller
//! (`handlers::chat`) calls `ollama::evict` before invoking us; we don't
//! coordinate eviction internally.

use std::sync::Arc;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::StreamExt;
use serde::Deserialize;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use uuid::Uuid;

use crate::ollama::ChatStreamError;
use crate::AppState;

/// Progress events forwarded from ComfyUI's WebSocket to the SSE client
/// while a job is sampling. Backend wraps these in callbacks the handler
/// pipes onto the existing chat stream.
#[derive(Debug, Clone)]
pub enum ProgressEvent {
    /// Sampler step counter. `value` reaches `max` at completion.
    Progress { value: u32, max: u32 },
    /// In-progress latent decode emitted by ComfyUI when the daemon was
    /// launched with `--preview-method`. Bytes are pre-base64'd to keep
    /// the SSE payload trivially serialisable.
    Preview { mime: &'static str, b64: String },
}

/// Trait alias for the closure handlers pass in to receive progress.
/// Held in an `Arc` so the spawned WS watcher can outlive the caller's
/// stack frame.
pub type ProgressCallback = Arc<dyn Fn(ProgressEvent) + Send + Sync + 'static>;

/// Save-node id in the workflow template — used to find the output image
/// in the history response.
const SAVE_NODE_ID: &str = "9";

/// Cap how long we wait for ComfyUI to finish a single job. On Apple
/// Silicon (MPS) Flux Kontext Q6_K samples at ~75 s/step at 1024² —
/// 20 steps lands around 25 minutes, plus model load. 30 minutes covers
/// a cold start plus full sample run while still bounding the request.
/// Cancellation (client disconnect → POST /interrupt) is the primary
/// stop mechanism; this only catches truly stuck jobs.
const POLL_TIMEOUT: Duration = Duration::from_secs(1800);
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Flux Kontext img2img workflow. Placeholders: `{prompt}` (JSON-escaped
/// already at substitution time), `{filename}` (uploaded reference image
/// name returned by `/upload/image`), `{seed}` (non-negative u64 as
/// stringified integer).
const WORKFLOW_TEMPLATE: &str = r##"{
  "3": {
    "class_type": "KSampler",
    "inputs": {
      "seed": {seed}, "steps": 8, "cfg": 1.0,
      "sampler_name": "euler", "scheduler": "simple", "denoise": 1.0,
      "model": ["60", 0], "positive": ["35", 0],
      "negative": ["135", 0], "latent_image": ["124", 0]
    }
  },
  "8":   { "class_type": "VAEDecode",
           "inputs": { "samples": ["3", 0], "vae": ["39", 0] } },
  "9":   { "class_type": "SaveImage",
           "inputs": { "filename_prefix": "chat_kontext",
                       "images": ["8", 0] } },
  "31":  { "class_type": "UnetLoaderGGUF",
           "inputs": { "unet_name": "flux1-kontext-dev-Q6_K.gguf" } },
  "60":  { "class_type": "LoraLoaderModelOnly",
           "inputs": { "model": ["31", 0],
                       "lora_name": "Hyper-FLUX.1-dev-8steps-lora.safetensors",
                       "strength_model": 0.125 } },
  "38":  { "class_type": "DualCLIPLoaderGGUF",
           "inputs": { "clip_name1": "clip_l.safetensors",
                       "clip_name2": "t5-v1_1-xxl-encoder-Q5_K_M.gguf",
                       "type": "flux" } },
  "39":  { "class_type": "VAELoader",
           "inputs": { "vae_name": "ae.safetensors" } },
  "42":  { "class_type": "LoadImage",
           "inputs": { "image": "{filename}" } },
  "50":  { "class_type": "ImageScaleToTotalPixels",
           "inputs": { "image": ["42", 0],
                       "upscale_method": "lanczos",
                       "megapixels": 0.59,
                       "resolution_steps": 64 } },
  "6":   { "class_type": "CLIPTextEncode",
           "inputs": { "text": "{prompt}", "clip": ["38", 0] } },
  "35":  { "class_type": "FluxGuidance",
           "inputs": { "guidance": 2.5, "conditioning": ["177", 0] } },
  "177": { "class_type": "ReferenceLatent",
           "inputs": { "conditioning": ["6", 0], "latent": ["124", 0] } },
  "124": { "class_type": "VAEEncode",
           "inputs": { "pixels": ["50", 0], "vae": ["39", 0] } },
  "135": { "class_type": "ConditioningZeroOut",
           "inputs": { "conditioning": ["6", 0] } }
}"##;

#[derive(Debug, Deserialize)]
struct UploadResponse {
    name: String,
}

#[derive(Debug, Deserialize)]
struct PromptResponse {
    prompt_id: String,
}

/// Run a Flux Kontext img2img job.
///
/// `prompt` is the natural-language edit instruction. `input_image_b64` is
/// the user's reference image (no `data:` prefix). `cancel` resolves when
/// the caller wants the job aborted (typically `mpsc::Sender::closed()`
/// firing because the SSE client went away). Returns the rendered PNG as
/// base64 — same shape as `ollama::generate_image` so the calling branch
/// can persist it via `Storage::complete_message` without further
/// branching. On cancellation we best-effort POST `/interrupt` and remove
/// the prompt from the queue before returning `ChatStreamError::Cancelled`.
pub async fn generate_kontext<F>(
    state: &AppState,
    prompt: &str,
    input_image_b64: &str,
    cancel: F,
    on_progress: Option<ProgressCallback>,
) -> Result<String, ChatStreamError>
where
    F: std::future::Future<Output = ()>,
{
    let base = state
        .settings
        .comfyui_url
        .as_deref()
        .ok_or(ChatStreamError::EmptyImage)?
        .trim_end_matches('/')
        .to_string();
    let client_id = Uuid::new_v4().to_string();

    // 1. Upload reference image.
    let bytes = STANDARD.decode(input_image_b64).map_err(|e| {
        tracing::warn!("base64 decode of user image failed: {e}");
        ChatStreamError::EmptyImage
    })?;
    let upload_filename = format!("chat-{}.png", Uuid::new_v4());
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(upload_filename.clone())
        .mime_str("image/png")
        .map_err(ChatStreamError::Http)?;
    let form = reqwest::multipart::Form::new()
        .text("overwrite", "true")
        .text("subfolder", "chat")
        .part("image", part);
    let upload: UploadResponse = state
        .http_client
        .post(format!("{base}/upload/image"))
        .multipart(form)
        .send()
        .await
        .map_err(ChatStreamError::Http)?
        .error_for_status()
        .map_err(ChatStreamError::Http)?
        .json()
        .await
        .map_err(ChatStreamError::Http)?;

    // 2. Build workflow + queue prompt. The uploaded subfolder is
    // included in the LoadImage filename per ComfyUI convention
    // (`subfolder/name`).
    let load_name = format!("chat/{}", upload.name);
    let seed: u64 = (Uuid::new_v4().as_u128() as u64) & 0x7fff_ffff_ffff_ffff;
    let prompt_escaped = json_escape(prompt);
    let load_escaped = json_escape(&load_name);
    let workflow_str = WORKFLOW_TEMPLATE
        .replace("{prompt}", &prompt_escaped)
        .replace("{filename}", &load_escaped)
        .replace("{seed}", &seed.to_string());
    let workflow: serde_json::Value = serde_json::from_str(&workflow_str).map_err(|e| {
        tracing::error!("malformed kontext workflow after substitution: {e}");
        ChatStreamError::EmptyImage
    })?;

    let queued: PromptResponse = state
        .http_client
        .post(format!("{base}/prompt"))
        .json(&serde_json::json!({
            "prompt": workflow,
            "client_id": client_id,
        }))
        .send()
        .await
        .map_err(ChatStreamError::Http)?
        .error_for_status()
        .map_err(ChatStreamError::Http)?
        .json()
        .await
        .map_err(ChatStreamError::Http)?;

    // 2.5 Subscribe to ComfyUI's WebSocket for live progress + previews.
    // The guard's Drop fires on every return path below — sends the
    // cancel signal then aborts the spawned task so we never leak a WS
    // connection across requests.
    let _watcher_guard = if let Some(cb) = on_progress.as_ref().cloned() {
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        let url = base.clone();
        let pid = queued.prompt_id.clone();
        let cid = client_id.clone();
        let handle = tokio::spawn(async move {
            let watcher_cancel = async move {
                let _ = cancel_rx.await;
            };
            watch_progress(url, pid, cid, watcher_cancel, move |evt| cb(evt)).await;
        });
        Some(WatcherGuard {
            cancel_tx: Some(cancel_tx),
            handle: Some(handle),
        })
    } else {
        None
    };

    // 3. Poll history until outputs land. Race the poll against the
    // cancellation signal — when the SSE client disconnects we want to
    // stop wasting compute on a render whose output nobody will see.
    tokio::pin!(cancel);
    let history_url = format!("{base}/history/{}", queued.prompt_id);
    let deadline = std::time::Instant::now() + POLL_TIMEOUT;
    let output_meta = loop {
        if std::time::Instant::now() >= deadline {
            interrupt_and_dequeue(state, &base, &queued.prompt_id).await;
            return Err(ChatStreamError::ComfyTimeout);
        }
        tokio::select! {
            _ = &mut cancel => {
                tracing::info!(
                    "comfyui job {} cancelled by client",
                    queued.prompt_id
                );
                interrupt_and_dequeue(state, &base, &queued.prompt_id).await;
                return Err(ChatStreamError::Cancelled);
            }
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
        }
        let res = match state.http_client.get(&history_url).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("comfyui history poll transient error: {e}");
                continue;
            }
        };
        let body: serde_json::Value = match res.json().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("comfyui history poll decode error: {e}");
                continue;
            }
        };
        let entry = body.get(&queued.prompt_id);
        if let Some(images) = entry
            .and_then(|e| e.get("outputs"))
            .and_then(|o| o.get(SAVE_NODE_ID))
            .and_then(|n| n.get("images"))
            .and_then(|a| a.as_array())
        {
            if let Some(first) = images.first() {
                break first.clone();
            }
        }
        // If the history entry exists but has a status with errors, bail
        // early instead of waiting out the timeout.
        if let Some(status) = entry.and_then(|e| e.get("status")) {
            if status.get("status_str").and_then(|s| s.as_str()) == Some("error") {
                tracing::error!("comfyui job reported error: {status}");
                return Err(ChatStreamError::EmptyImage);
            }
        }
    };

    let filename = output_meta
        .get("filename")
        .and_then(|v| v.as_str())
        .ok_or(ChatStreamError::EmptyImage)?;
    let subfolder = output_meta
        .get("subfolder")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let kind = output_meta
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("output");

    // 4. Fetch rendered PNG bytes.
    let view = state
        .http_client
        .get(format!("{base}/view"))
        .query(&[
            ("filename", filename),
            ("subfolder", subfolder),
            ("type", kind),
        ])
        .send()
        .await
        .map_err(ChatStreamError::Http)?
        .error_for_status()
        .map_err(ChatStreamError::Http)?
        .bytes()
        .await
        .map_err(ChatStreamError::Http)?;

    if view.is_empty() {
        return Err(ChatStreamError::EmptyImage);
    }
    Ok(STANDARD.encode(&view))
}

fn json_escape(s: &str) -> String {
    let v = serde_json::Value::String(s.to_string()).to_string();
    // Strip the surrounding quotes serde_json adds.
    v[1..v.len() - 1].to_string()
}

/// Drop guard for the WS watcher task. Fires on every return path of
/// `generate_kontext` so the watcher never outlives its job.
struct WatcherGuard {
    cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for WatcherGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.cancel_tx.take() {
            let _ = tx.send(());
        }
        // Drop trait can't `await`; abort is the only synchronous way to
        // tear the task down. The watcher's WS read will return an error
        // on the next poll and exit cleanly.
        if let Some(h) = self.handle.take() {
            h.abort();
        }
    }
}

/// Subscribe to ComfyUI's `/ws` endpoint and forward progress + preview
/// frames matching `target_prompt_id` to `on_event`. Exits when `cancel`
/// resolves or when the WS stream errors / closes. Failures are logged
/// and swallowed — progress is best-effort UX, never a generation gate.
async fn watch_progress<F>(
    base_url: String,
    target_prompt_id: String,
    client_id: String,
    cancel: F,
    on_event: impl Fn(ProgressEvent) + Send + Sync + 'static,
) where
    F: std::future::Future<Output = ()>,
{
    let ws_url = ws_url_from(&base_url, &client_id);
    let (mut stream, _) = match tokio_tungstenite::connect_async(&ws_url).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("comfyui ws connect failed ({ws_url}): {e}");
            return;
        }
    };
    // ComfyUI's binary preview frames don't carry a prompt_id — they're
    // emitted for whichever job is currently executing on the worker.
    // Track that via the JSON `executing` events so we only forward
    // previews that belong to *our* job.
    let mut is_ours = false;
    tokio::pin!(cancel);
    loop {
        let frame = tokio::select! {
            _ = &mut cancel => return,
            f = stream.next() => f,
        };
        let Some(frame) = frame else { return };
        let frame = match frame {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("comfyui ws read error: {e}");
                return;
            }
        };
        match frame {
            WsMessage::Text(text) => {
                let parsed: serde_json::Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let kind = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");
                let data = parsed.get("data").cloned().unwrap_or(serde_json::Value::Null);
                match kind {
                    "executing" => {
                        let pid = data.get("prompt_id").and_then(|v| v.as_str());
                        is_ours = pid == Some(target_prompt_id.as_str());
                    }
                    "execution_start" | "execution_cached" => {
                        let pid = data.get("prompt_id").and_then(|v| v.as_str());
                        if pid == Some(target_prompt_id.as_str()) {
                            is_ours = true;
                        }
                    }
                    "execution_success" | "execution_error" | "execution_interrupted" => {
                        let pid = data.get("prompt_id").and_then(|v| v.as_str());
                        if pid == Some(target_prompt_id.as_str()) {
                            return;
                        }
                    }
                    "progress" => {
                        let pid = data.get("prompt_id").and_then(|v| v.as_str());
                        // Newer ComfyUI tags progress with prompt_id;
                        // older versions don't — fall back to is_ours.
                        let matches = match pid {
                            Some(p) => p == target_prompt_id,
                            None => is_ours,
                        };
                        if !matches {
                            continue;
                        }
                        let value =
                            data.get("value").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                        let max =
                            data.get("max").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                        on_event(ProgressEvent::Progress { value, max });
                    }
                    _ => {}
                }
            }
            WsMessage::Binary(bytes) => {
                if !is_ours || bytes.len() < 8 {
                    continue;
                }
                let event_type =
                    u32::from_be_bytes(bytes[0..4].try_into().expect("4 bytes"));
                let img_type =
                    u32::from_be_bytes(bytes[4..8].try_into().expect("4 bytes"));
                // BinaryEventTypes.PREVIEW_IMAGE = 1 in ComfyUI's server.py.
                if event_type != 1 {
                    continue;
                }
                let mime = match img_type {
                    1 => "image/jpeg",
                    2 => "image/png",
                    _ => continue,
                };
                let b64 = STANDARD.encode(&bytes[8..]);
                on_event(ProgressEvent::Preview { mime, b64 });
            }
            WsMessage::Close(_) => return,
            _ => {}
        }
    }
}

fn ws_url_from(http_url: &str, client_id: &str) -> String {
    let stripped = http_url.trim_end_matches('/');
    let (scheme, rest) = if let Some(s) = stripped.strip_prefix("https://") {
        ("wss", s)
    } else if let Some(s) = stripped.strip_prefix("http://") {
        ("ws", s)
    } else {
        ("ws", stripped)
    };
    format!("{scheme}://{rest}/ws?clientId={client_id}")
}

/// Best-effort cancel: drop the prompt from the queue (no-op if it has
/// already started executing) and interrupt the currently running job.
/// ComfyUI's `/interrupt` is global — there's only one worker — so a
/// stale interrupt on a different job is the worst case here. Acceptable
/// for a single-tenant box. Errors are logged and swallowed; callers
/// have already decided to abort.
pub(crate) async fn interrupt_and_dequeue(state: &AppState, base: &str, prompt_id: &str) {
    if let Err(e) = state
        .http_client
        .post(format!("{base}/queue"))
        .json(&serde_json::json!({ "delete": [prompt_id] }))
        .send()
        .await
    {
        tracing::warn!("comfyui queue delete failed: {e}");
    }
    if let Err(e) = state
        .http_client
        .post(format!("{base}/interrupt"))
        .send()
        .await
    {
        tracing::warn!("comfyui interrupt failed: {e}");
    }
}

/// Standalone cancel entry point used by the explicit cancel route, for
/// the case where the user wants to stop a generation from a tab that
/// isn't holding the original SSE connection (e.g. after a page reload).
pub async fn interrupt_active(state: &AppState) {
    let Some(base) = state
        .settings
        .comfyui_url
        .as_deref()
        .map(|u| u.trim_end_matches('/').to_string())
    else {
        return;
    };
    if let Err(e) = state
        .http_client
        .post(format!("{base}/interrupt"))
        .send()
        .await
    {
        tracing::warn!("comfyui interrupt failed: {e}");
    }
}

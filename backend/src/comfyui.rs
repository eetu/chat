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

/// Save-node id in the Kontext workflow template — used to find the
/// output image in the history response.
const KONTEXT_SAVE_NODE_ID: &str = "9";

/// Save-node id in the Flux Fill inpaint workflow.
const INPAINT_SAVE_NODE_ID: &str = "11";

/// Save-node id in the Z-Image Turbo txt2img workflow.
const TXT2IMG_SAVE_NODE_ID: &str = "9";

/// Cap how long we wait for ComfyUI to finish a single job. On Apple
/// Silicon (MPS) Flux Kontext Q6_K samples at ~75 s/step at 1024² —
/// 20 steps lands around 25 minutes, plus model load. 30 minutes covers
/// a cold start plus full sample run while still bounding the request.
/// Cancellation (client disconnect → POST /interrupt) is the primary
/// stop mechanism; this only catches truly stuck jobs.
const POLL_TIMEOUT: Duration = Duration::from_secs(1800);
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Build a Flux Kontext workflow for one or more reference images.
///
/// Each image gets its own `LoadImage` → `ImageScaleToTotalPixels` →
/// `VAEEncode` pipeline. `ReferenceLatent` nodes are chained so every
/// encoded latent feeds the conditioning that ultimately drives Flux
/// Guidance — this is how Kontext composes multiple reference inputs.
/// The first image's encoded latent doubles as `KSampler.latent_image`
/// (Kontext's denoising target), matching the upstream single-image
/// graph.
fn build_workflow(prompt: &str, filenames: &[String], seed: u64, steps: u32) -> serde_json::Value {
    use serde_json::json;
    let mut wf = serde_json::Map::new();

    // Model loaders + text encoder are shared across all branches.
    wf.insert(
        "31".into(),
        json!({
            "class_type": "UnetLoaderGGUF",
            "inputs": { "unet_name": "flux1-kontext-dev-Q6_K.gguf" }
        }),
    );
    wf.insert(
        "60".into(),
        json!({
            "class_type": "LoraLoaderModelOnly",
            "inputs": {
                "model": ["31", 0],
                "lora_name": "Hyper-FLUX.1-dev-8steps-lora.safetensors",
                "strength_model": 0.125
            }
        }),
    );
    wf.insert(
        "38".into(),
        json!({
            "class_type": "DualCLIPLoaderGGUF",
            "inputs": {
                "clip_name1": "clip_l.safetensors",
                "clip_name2": "t5-v1_1-xxl-encoder-Q5_K_M.gguf",
                "type": "flux"
            }
        }),
    );
    wf.insert(
        "39".into(),
        json!({ "class_type": "VAELoader", "inputs": { "vae_name": "ae.safetensors" } }),
    );
    wf.insert(
        "6".into(),
        json!({
            "class_type": "CLIPTextEncode",
            "inputs": { "text": prompt, "clip": ["38", 0] }
        }),
    );
    wf.insert(
        "135".into(),
        json!({
            "class_type": "ConditioningZeroOut",
            "inputs": { "conditioning": ["6", 0] }
        }),
    );

    // Per-image: load, downscale, VAE-encode, then chain a ReferenceLatent.
    let mut prev_conditioning = "6".to_string();
    for (i, filename) in filenames.iter().enumerate() {
        let load_id = format!("42_{i}");
        let scale_id = format!("50_{i}");
        let encode_id = format!("124_{i}");
        let ref_id = format!("177_{i}");
        wf.insert(
            load_id.clone(),
            json!({
                "class_type": "LoadImage",
                "inputs": { "image": filename }
            }),
        );
        wf.insert(
            scale_id.clone(),
            json!({
                "class_type": "ImageScaleToTotalPixels",
                "inputs": {
                    "image": [load_id, 0],
                    "upscale_method": "lanczos",
                    "megapixels": 0.59,
                    "resolution_steps": 64
                }
            }),
        );
        wf.insert(
            encode_id.clone(),
            json!({
                "class_type": "VAEEncode",
                "inputs": { "pixels": [scale_id, 0], "vae": ["39", 0] }
            }),
        );
        wf.insert(
            ref_id.clone(),
            json!({
                "class_type": "ReferenceLatent",
                "inputs": { "conditioning": [prev_conditioning, 0], "latent": [encode_id, 0] }
            }),
        );
        prev_conditioning = ref_id;
    }

    wf.insert(
        "35".into(),
        json!({
            "class_type": "FluxGuidance",
            "inputs": { "guidance": 2.5, "conditioning": [prev_conditioning, 0] }
        }),
    );

    // First image's encoded latent is the denoising target.
    wf.insert(
        "3".into(),
        json!({
            "class_type": "KSampler",
            "inputs": {
                "seed": seed,
                "steps": steps,
                "cfg": 1.0,
                "sampler_name": "euler",
                "scheduler": "simple",
                "denoise": 1.0,
                "model": ["60", 0],
                "positive": ["35", 0],
                "negative": ["135", 0],
                "latent_image": ["124_0", 0]
            }
        }),
    );
    wf.insert(
        "8".into(),
        json!({
            "class_type": "VAEDecode",
            "inputs": { "samples": ["3", 0], "vae": ["39", 0] }
        }),
    );
    wf.insert(
        KONTEXT_SAVE_NODE_ID.into(),
        json!({
            "class_type": "SaveImage",
            "inputs": { "filename_prefix": "chat_kontext", "images": ["8", 0] }
        }),
    );

    serde_json::Value::Object(wf)
}

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
/// `prompt` is the natural-language edit instruction. `input_images_b64`
/// is one or more user reference images (no `data:` prefix); when more
/// than one is supplied they're chained as additional Kontext references.
/// `cancel` resolves when the caller wants the job aborted (typically
/// `mpsc::Sender::closed()` firing because the SSE client went away).
/// Returns the rendered PNG as base64 — same shape as
/// `ollama::generate_image` so the calling branch can persist it via
/// `Storage::complete_message` without further branching. On
/// cancellation we best-effort POST `/interrupt` and remove the prompt
/// from the queue before returning `ChatStreamError::Cancelled`.
pub async fn generate_kontext<F>(
    state: &AppState,
    prompt: &str,
    input_images_b64: &[String],
    steps: u32,
    cancel: F,
    on_progress: Option<ProgressCallback>,
) -> Result<String, ChatStreamError>
where
    F: std::future::Future<Output = ()>,
{
    if input_images_b64.is_empty() {
        return Err(ChatStreamError::EmptyImage);
    }
    let base = state
        .settings
        .comfyui_url
        .as_deref()
        .ok_or(ChatStreamError::EmptyImage)?
        .trim_end_matches('/')
        .to_string();
    let client_id = Uuid::new_v4().to_string();

    // 1. Upload each reference image and remember its server-side name.
    let mut filenames: Vec<String> = Vec::with_capacity(input_images_b64.len());
    for (i, b64) in input_images_b64.iter().enumerate() {
        filenames.push(upload_image(state, &base, b64, i).await?);
    }

    // 2. Build workflow + queue + watch + poll + fetch — shared with
    // generate_inpaint via run_workflow.
    let seed: u64 = (Uuid::new_v4().as_u128() as u64) & 0x7fff_ffff_ffff_ffff;
    let workflow = build_workflow(prompt, &filenames, seed, steps);
    run_workflow(
        state,
        &base,
        &client_id,
        workflow,
        KONTEXT_SAVE_NODE_ID,
        cancel,
        on_progress,
    )
    .await
}

/// Upload a single base64 image to ComfyUI's `/upload/image`. MIME is
/// sniffed so the multipart Content-Type and filename extension match
/// the bytes; ComfyUI's LoadImage node otherwise rejects the file.
/// Returns the server-side filename including the `chat/` subfolder.
async fn upload_image(
    state: &AppState,
    base: &str,
    b64: &str,
    label_for_logs: usize,
) -> Result<String, ChatStreamError> {
    let bytes = STANDARD.decode(b64).map_err(|e| {
        tracing::warn!("base64 decode of comfyui upload #{label_for_logs} failed: {e}");
        ChatStreamError::EmptyImage
    })?;
    let mime = crate::image_kind::detect(&bytes).ok_or_else(|| {
        tracing::warn!(
            "rejecting unsupported image format for comfyui upload (#{label_for_logs})"
        );
        ChatStreamError::EmptyImage
    })?;
    let ext = crate::image_kind::extension(mime);
    let upload_filename = format!("chat-{}.{ext}", Uuid::new_v4());
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(upload_filename)
        .mime_str(mime)
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
    Ok(format!("chat/{}", upload.name))
}

/// Queue + watch + poll + fetch for a fully-built workflow. Shared
/// across the Kontext and Flux Fill paths since the lifecycle outside
/// of workflow construction is identical: POST /prompt, optionally
/// connect a WS watcher, poll /history for the named save node's
/// output, then GET /view for the PNG bytes. `save_node_id` lets each
/// caller pick its own SaveImage node since the templates differ.
async fn run_workflow<F>(
    state: &AppState,
    base: &str,
    client_id: &str,
    workflow: serde_json::Value,
    save_node_id: &str,
    cancel: F,
    on_progress: Option<ProgressCallback>,
) -> Result<String, ChatStreamError>
where
    F: std::future::Future<Output = ()>,
{
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

    // Subscribe to ComfyUI's WebSocket for live progress + previews.
    // The guard's Drop fires on every return path below — sends the
    // cancel signal then aborts the spawned task so we never leak a WS
    // connection across requests.
    let _watcher_guard = if let Some(cb) = on_progress.as_ref().cloned() {
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        let url = base.to_string();
        let pid = queued.prompt_id.clone();
        let cid = client_id.to_string();
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

    // Poll history until outputs land. Race the poll against the
    // cancellation signal — when the SSE client disconnects we want to
    // stop wasting compute on a render whose output nobody will see.
    tokio::pin!(cancel);
    let history_url = format!("{base}/history/{}", queued.prompt_id);
    let deadline = std::time::Instant::now() + POLL_TIMEOUT;
    let output_meta = loop {
        if std::time::Instant::now() >= deadline {
            interrupt_and_dequeue(state, base, &queued.prompt_id).await;
            return Err(ChatStreamError::ComfyTimeout);
        }
        tokio::select! {
            _ = &mut cancel => {
                tracing::info!(
                    "comfyui job {} cancelled by client",
                    queued.prompt_id
                );
                interrupt_and_dequeue(state, base, &queued.prompt_id).await;
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
            .and_then(|o| o.get(save_node_id))
            .and_then(|n| n.get("images"))
            .and_then(|a| a.as_array())
        {
            if let Some(first) = images.first() {
                break first.clone();
            }
        }
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

/// Build the Flux Fill masked-inpaint workflow.
///
/// Mirrors `files/comfyui-workflows/flux-fill-inpaint.json` on the mini
/// host: Flux.1 Fill GGUF + DualCLIP + VAE, a LoadImage for the base
/// pixels, a LoadImageMask reading the mask's red channel
/// (white=repaint, black=keep), CLIPTextEncode for both positive (the
/// user prompt) and a deliberately empty negative, InpaintModelConditioning
/// to weave the mask into the conditioning + latent, KSampler at the
/// model's documented defaults (20 steps, cfg=3.5, euler), VAEDecode,
/// SaveImage.
fn build_inpaint_workflow(
    prompt: &str,
    negative: &str,
    base_filename: &str,
    mask_filename: &str,
    seed: u64,
    steps: u32,
) -> serde_json::Value {
    use serde_json::json;
    json!({
        "1": {
            "class_type": "UnetLoaderGGUF",
            "inputs": { "unet_name": "flux1-fill-dev-Q6_K.gguf" },
        },
        "2": {
            "class_type": "DualCLIPLoaderGGUF",
            "inputs": {
                "clip_name1": "t5-v1_1-xxl-encoder-Q5_K_M.gguf",
                "clip_name2": "clip_l.safetensors",
                "type": "flux",
            },
        },
        "3": {
            "class_type": "VAELoader",
            "inputs": { "vae_name": "ae.safetensors" },
        },
        "4": {
            "class_type": "LoadImage",
            "inputs": { "image": base_filename },
        },
        "5": {
            "class_type": "LoadImageMask",
            "inputs": { "image": mask_filename, "channel": "red" },
        },
        "6": {
            "class_type": "CLIPTextEncode",
            "inputs": { "text": prompt, "clip": ["2", 0] },
        },
        "7": {
            "class_type": "CLIPTextEncode",
            "inputs": { "text": negative, "clip": ["2", 0] },
        },
        "8": {
            "class_type": "InpaintModelConditioning",
            "inputs": {
                "positive": ["6", 0],
                "negative": ["7", 0],
                "vae": ["3", 0],
                "pixels": ["4", 0],
                "mask": ["5", 0],
                "noise_mask": true,
            },
        },
        "9": {
            "class_type": "KSampler",
            "inputs": {
                "model": ["1", 0],
                "positive": ["8", 0],
                "negative": ["8", 1],
                "latent_image": ["8", 2],
                "seed": seed,
                "steps": steps,
                "cfg": 3.5,
                "sampler_name": "euler",
                "scheduler": "normal",
                "denoise": 1.0,
            },
        },
        "10": {
            "class_type": "VAEDecode",
            "inputs": { "samples": ["9", 0], "vae": ["3", 0] },
        },
        "11": {
            "class_type": "SaveImage",
            "inputs": { "images": ["10", 0], "filename_prefix": "fill" },
        },
    })
}

/// Build the Z-Image Turbo text-to-image workflow.
///
/// Mirrors `files/comfyui-workflows/z-image-turbo-txt2img.json` on the
/// mini host (node ids 1–9 verbatim). Z-Image is a 16-channel model:
/// UNet + Qwen3 text encoder (loaded as `stable_diffusion` so core
/// auto-detects the QWEN3_4B → z_image TE), reusing Flux's `ae.safetensors`
/// VAE. `TextEncodeZImageOmni` carries both prompts; `EmptySD3LatentImage`
/// gives the 16-ch latent; KSampler runs euler/simple at the turbo
/// defaults (8 steps, cfg 2.0). cfg 2.0 means the negative branch is
/// live, so node 5's text actually influences output.
///
/// `auto_resize_images` is required on the text-encode nodes — the node
/// schema defaults it but the `/prompt` validator rejects the graph if
/// it's absent.
fn build_txt2img_workflow(
    prompt: &str,
    negative: &str,
    seed: u64,
    steps: u32,
) -> serde_json::Value {
    use serde_json::json;
    json!({
        "1": {
            "class_type": "UnetLoaderGGUF",
            "inputs": { "unet_name": "z-image-turbo-Q8_0.gguf" },
        },
        "2": {
            "class_type": "CLIPLoaderGGUF",
            "inputs": { "clip_name": "Qwen3-4B-UD-Q5_K_XL.gguf", "type": "stable_diffusion" },
        },
        "3": {
            "class_type": "VAELoader",
            "inputs": { "vae_name": "ae.safetensors" },
        },
        "4": {
            "class_type": "TextEncodeZImageOmni",
            "inputs": { "clip": ["2", 0], "prompt": prompt, "auto_resize_images": true },
        },
        "5": {
            "class_type": "TextEncodeZImageOmni",
            "inputs": { "clip": ["2", 0], "prompt": negative, "auto_resize_images": true },
        },
        "6": {
            "class_type": "EmptySD3LatentImage",
            "inputs": { "width": 1024, "height": 1024, "batch_size": 1 },
        },
        "7": {
            "class_type": "KSampler",
            "inputs": {
                "model": ["1", 0],
                "positive": ["4", 0],
                "negative": ["5", 0],
                "latent_image": ["6", 0],
                "seed": seed,
                "steps": steps,
                "cfg": 2.0,
                "sampler_name": "euler",
                "scheduler": "simple",
                "denoise": 1.0,
            },
        },
        "8": {
            "class_type": "VAEDecode",
            "inputs": { "samples": ["7", 0], "vae": ["3", 0] },
        },
        "9": {
            "class_type": "SaveImage",
            "inputs": { "images": ["8", 0], "filename_prefix": "chat_zimage" },
        },
    })
}

/// Run a Z-Image Turbo text-to-image job. No upload step (no reference
/// image); otherwise the lifecycle is shared with the Kontext/inpaint
/// paths via `run_workflow`. Random seed masked the same way as the
/// other generators. Caller is expected to have evicted competing
/// Ollama models and to call `free_memory()` afterward.
pub async fn generate_txt2img<F>(
    state: &AppState,
    prompt: &str,
    negative: &str,
    steps: u32,
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

    let seed: u64 = (Uuid::new_v4().as_u128() as u64) & 0x7fff_ffff_ffff_ffff;
    let workflow = build_txt2img_workflow(prompt, negative, seed, steps);
    run_workflow(
        state,
        &base,
        &client_id,
        workflow,
        TXT2IMG_SAVE_NODE_ID,
        cancel,
        on_progress,
    )
    .await
}

/// Run a Flux Fill masked-inpaint job. Shape mirrors `generate_kontext`:
/// upload reference + mask, queue the workflow, share the WS watcher /
/// history poll / view fetch via `run_workflow`. The caller is
/// expected to have already evicted competing Ollama models — mini's
/// 24 GB unified memory cannot hold the Fill diffusion model alongside
/// chat models, and free_memory() should follow on the handler side.
#[allow(clippy::too_many_arguments)]
pub async fn generate_inpaint<F>(
    state: &AppState,
    prompt: &str,
    negative: &str,
    base_image_b64: &str,
    mask_image_b64: &str,
    steps: u32,
    cancel: F,
    on_progress: Option<ProgressCallback>,
) -> Result<String, ChatStreamError>
where
    F: std::future::Future<Output = ()>,
{
    if base_image_b64.is_empty() || mask_image_b64.is_empty() {
        return Err(ChatStreamError::EmptyImage);
    }
    let base_url = state
        .settings
        .comfyui_url
        .as_deref()
        .ok_or(ChatStreamError::EmptyImage)?
        .trim_end_matches('/')
        .to_string();
    let client_id = Uuid::new_v4().to_string();

    let base_filename = upload_image(state, &base_url, base_image_b64, 0).await?;
    let mask_filename = upload_image(state, &base_url, mask_image_b64, 1).await?;

    let seed: u64 = (Uuid::new_v4().as_u128() as u64) & 0x7fff_ffff_ffff_ffff;
    let workflow = build_inpaint_workflow(
        prompt,
        negative,
        &base_filename,
        &mask_filename,
        seed,
        steps,
    );
    run_workflow(
        state,
        &base_url,
        &client_id,
        workflow,
        INPAINT_SAVE_NODE_ID,
        cancel,
        on_progress,
    )
    .await
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

/// Tell ComfyUI to unload all checkpoints + free its torch allocator.
/// Mirrors `ollama::evict` (per-request `keep_alive: 0`) so neither
/// upstream sits resident between requests on a 24 GB unified-memory
/// host. Next img2img job pays a ~10–15 s reload from disk; in exchange
/// the chat / refiner models can occupy the freed RAM during idle
/// stretches. Best-effort — failures only warn.
pub async fn free_memory(state: &AppState) {
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
        .post(format!("{base}/free"))
        .json(&serde_json::json!({
            "unload_models": true,
            "free_memory": true,
        }))
        .send()
        .await
    {
        tracing::warn!("comfyui /free failed: {e}");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inpaint_workflow_wires_mask_and_base() {
        // Sanity check: shape matches files/comfyui-workflows/flux-fill-inpaint.json.
        // Catches regressions to node ids or input names which would
        // otherwise only surface at runtime on the mini host.
        let wf = build_inpaint_workflow(
            "paint sky blue",
            "blurry, extra fingers",
            "chat/base.png",
            "chat/mask.png",
            42,
            20,
        );

        assert_eq!(
            wf["1"]["class_type"], "UnetLoaderGGUF",
            "node 1 must load the Flux Fill GGUF"
        );
        assert_eq!(wf["1"]["inputs"]["unet_name"], "flux1-fill-dev-Q6_K.gguf");

        assert_eq!(wf["4"]["class_type"], "LoadImage");
        assert_eq!(wf["4"]["inputs"]["image"], "chat/base.png");

        assert_eq!(wf["5"]["class_type"], "LoadImageMask");
        assert_eq!(wf["5"]["inputs"]["image"], "chat/mask.png");
        assert_eq!(wf["5"]["inputs"]["channel"], "red");

        // The user prompt rides on the positive CLIP encode (node 6),
        // negative on node 7. Flux Fill runs real CFG so node 7
        // actually influences sampling.
        assert_eq!(wf["6"]["inputs"]["text"], "paint sky blue");
        assert_eq!(wf["7"]["inputs"]["text"], "blurry, extra fingers");

        assert_eq!(wf["8"]["class_type"], "InpaintModelConditioning");
        assert_eq!(wf["8"]["inputs"]["noise_mask"], true);

        assert_eq!(wf["9"]["class_type"], "KSampler");
        assert_eq!(wf["9"]["inputs"]["seed"], 42);
        assert_eq!(wf["9"]["inputs"]["steps"], 20);

        // SaveImage id matches the constant the poll loop watches for.
        assert_eq!(wf[INPAINT_SAVE_NODE_ID]["class_type"], "SaveImage");
    }

    #[test]
    fn txt2img_workflow_wires_zimage() {
        // Sanity check: shape matches
        // files/comfyui-workflows/z-image-turbo-txt2img.json. Guards
        // against drift from the host JSON — node ids / class types /
        // model filenames that would only fail at runtime on the mini.
        let wf = build_txt2img_workflow("a red fox in snow", "blurry, low quality", 7, 8);

        assert_eq!(
            wf["1"]["class_type"], "UnetLoaderGGUF",
            "node 1 must load the Z-Image Turbo GGUF"
        );
        assert_eq!(wf["1"]["inputs"]["unet_name"], "z-image-turbo-Q8_0.gguf");

        assert_eq!(wf["2"]["class_type"], "CLIPLoaderGGUF");
        assert_eq!(wf["2"]["inputs"]["type"], "stable_diffusion");

        // Positive on node 4, negative on node 5; both carry the
        // schema-required auto_resize_images flag.
        assert_eq!(wf["4"]["class_type"], "TextEncodeZImageOmni");
        assert_eq!(wf["4"]["inputs"]["prompt"], "a red fox in snow");
        assert_eq!(wf["4"]["inputs"]["auto_resize_images"], true);
        assert_eq!(wf["5"]["inputs"]["prompt"], "blurry, low quality");
        assert_eq!(wf["5"]["inputs"]["auto_resize_images"], true);

        assert_eq!(wf["6"]["class_type"], "EmptySD3LatentImage");

        assert_eq!(wf["7"]["class_type"], "KSampler");
        assert_eq!(wf["7"]["inputs"]["seed"], 7);
        assert_eq!(wf["7"]["inputs"]["steps"], 8);

        // SaveImage id matches the constant the poll loop watches for.
        assert_eq!(wf[TXT2IMG_SAVE_NODE_ID]["class_type"], "SaveImage");
    }
}

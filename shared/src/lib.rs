//! Shared request/response types for the chat HTTP API.
//!
//! Both the actix backend (`chat-backend`) and the MCP bridge (`chat-mcp`)
//! depend on this crate so the wire format has a single source of truth.
//! Keep this crate dependency-light — adding heavy deps here forces both
//! consumers to recompile them.

use serde::{Deserialize, Serialize};

/// Default sampler steps for Flux Kontext img2img. Matches the value the
/// chat handler used before `steps` was made user-tunable. With the
/// Hyper-FLUX 8-steps LoRA active, 8 is balanced quality/latency.
pub const DEFAULT_KONTEXT_STEPS: u32 = 8;
/// Default sampler steps for Flux Fill inpaint. Matches Flux Fill's
/// upstream-recommended setting.
pub const DEFAULT_INPAINT_STEPS: u32 = 20;

/// Min/max bounds enforced server-side so a runaway `steps` value can't
/// burn unbounded GPU time.
pub const MIN_STEPS: u32 = 4;
pub const MAX_KONTEXT_STEPS: u32 = 20;
pub const MAX_INPAINT_STEPS: u32 = 40;

/// Body for `POST /api/v1/txt2img`. Pure text-to-image generation
/// via Ollama's `/v1/images/generations` endpoint. No reference
/// images, no mask, no sampler steps knob — Ollama's image surface
/// doesn't expose those. `model` is optional; the backend falls back
/// to `OLLAMA_MODEL` or the project default when unset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Txt2ImgRequest {
    pub prompt: String,
    #[serde(default)]
    pub model: Option<String>,
}

/// Single entry in the `/api/v1/models/image` listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageModelEntry {
    pub name: String,
    /// Ollama-reported family ids, e.g. `["gemma3"]` or
    /// `["qwen2.5vl"]`. Mostly informational — the agent can use this
    /// to pick a model that matches the user's stylistic intent.
    #[serde(default)]
    pub families: Vec<String>,
}

/// Body of `GET /api/v1/models/image`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageModelsResponse {
    pub models: Vec<ImageModelEntry>,
}

/// Body for `POST /api/v1/img2img`. Reference-image guided edit via
/// Flux Kontext. `images` carries one or more base64 PNG/JPEG blobs
/// without a `data:` prefix; the first image is the denoising target,
/// the rest chain as Kontext references.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Img2ImgRequest {
    pub prompt: String,
    pub images: Vec<String>,
    /// Sampler steps. Defaults to `DEFAULT_KONTEXT_STEPS`.
    /// Clamped to `MIN_STEPS..=MAX_KONTEXT_STEPS` server-side.
    #[serde(default)]
    pub steps: Option<u32>,
}

/// Body for `POST /api/v1/inpaint`. Masked region repaint via
/// Flux Fill. `mask` is the same size as `image`; in the mask's red
/// channel, white pixels mark regions to repaint and black pixels mark
/// regions to keep. Both payloads are base64-encoded with no `data:`
/// prefix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InpaintRequest {
    pub prompt: String,
    pub image: String,
    pub mask: String,
    #[serde(default)]
    pub negative_prompt: Option<String>,
    /// Sampler steps. Defaults to `DEFAULT_INPAINT_STEPS`.
    /// Clamped to `MIN_STEPS..=MAX_INPAINT_STEPS` server-side.
    #[serde(default)]
    pub steps: Option<u32>,
}

/// Final SSE `done` event payload — the rendered PNG as base64.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageResponse {
    pub image_b64: String,
}

/// SSE `progress` event payload. `value` reaches `max` at completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressPayload {
    pub value: u32,
    pub max: u32,
}

/// SSE `preview` event payload — in-progress latent decode. `mime` is
/// `image/jpeg` or `image/png`; `b64` is the raw image without prefix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewPayload {
    pub mime: String,
    pub b64: String,
}

/// SSE `error` event payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPayload {
    pub message: String,
}

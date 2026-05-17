//! MCP bridge for chat's image generation endpoints.
//!
//! Speaks MCP over stdio. Wraps `POST /api/v1/img2img` and
//! `POST /api/v1/inpaint` on the chat backend as MCP tools and
//! forwards SSE progress events as MCP `notifications/progress`.
//!
//! Configure via env:
//!
//! - `CHAT_BACKEND_URL` — e.g. `http://chat.local:8080`
//! - `CHAT_MCP_API_KEY` — matches the backend's `CHAT_MCP_API_KEY`
//! - `RUST_LOG` — defaults to `chat_mcp=info`
//!
//! Tools advertised:
//!
//! - `chat_img2img` — Flux Kontext reference-image edit
//! - `chat_inpaint` — Flux Fill masked repaint

mod backend;

use std::sync::Arc;

use anyhow::Context as _;
use chat_shared::{
    Img2ImgRequest, InpaintRequest, DEFAULT_INPAINT_STEPS, DEFAULT_KONTEXT_STEPS,
    MAX_INPAINT_STEPS, MAX_KONTEXT_STEPS, MIN_STEPS,
};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content, ErrorData, ProgressNotificationParam, ProgressToken},
    service::RequestContext,
    tool, tool_handler, tool_router,
    transport::stdio,
    RoleServer, ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

use crate::backend::{BackendConfig, BackendEvent};

/// Parameters for `chat_img2img`. Field docs flow into the JSON
/// Schema via schemars, which MCP clients show to the agent in tool
/// listings — keep the wording oriented at the agent, not the human
/// reader.
#[derive(Debug, Deserialize, JsonSchema)]
struct Img2ImgArgs {
    /// Natural-language edit instruction. Examples: "make the sky
    /// stormy", "add a wizard hat to the cat", "convert to oil
    /// painting style".
    prompt: String,
    /// One or more reference images as base64 PNG/JPEG (no `data:`
    /// prefix). The first image is the denoising target; any
    /// additional images chain as Kontext references that influence
    /// the result without becoming the output canvas.
    images: Vec<String>,
    /// Sampler steps. Higher = better quality, slower. When
    /// the user asks for "fast" / "preview" / "draft", pick the low
    /// end. When they ask for "final" / "high quality", pick the high
    /// end. Defaults to 8 (balanced).
    #[serde(default)]
    steps: Option<u32>,
}

/// Parameters for `chat_inpaint`. See `Img2ImgArgs` for schema notes.
#[derive(Debug, Deserialize, JsonSchema)]
struct InpaintArgs {
    /// What to paint into the masked region.
    prompt: String,
    /// Optional negative prompt — Flux Fill runs real CFG so this
    /// actually influences sampling. Example: "blurry, extra fingers,
    /// deformed hands".
    #[serde(default)]
    negative_prompt: Option<String>,
    /// Base image, base64 PNG/JPEG (no `data:` prefix).
    image: String,
    /// Mask aligned to `image`, base64 PNG. Red channel: white pixels
    /// mark the region to repaint, black pixels mark the region to
    /// keep unchanged.
    mask: String,
    /// Sampler steps. Higher = better quality, slower. Defaults to 20
    /// (balanced).
    #[serde(default)]
    steps: Option<u32>,
}

/// MCP tool handler. Holds the resolved backend config so each tool
/// call doesn't re-read env on every invocation.
#[derive(Clone)]
struct ChatImageTools {
    backend: Arc<BackendConfig>,
    tool_router: ToolRouter<ChatImageTools>,
}

#[tool_router]
impl ChatImageTools {
    pub fn new(backend: BackendConfig) -> Self {
        Self {
            backend: Arc::new(backend),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        name = "chat_img2img",
        description = "Edit one or more reference images using the Flux Kontext model and return a new PNG.

Provide a natural-language edit instruction in `prompt` and one or more base64-encoded reference images in `images`. The first image is the denoising target; additional images influence the result as Kontext references.

Speed / quality tradeoff via `steps`:
  - 4-6: fast preview (~30-60s warm), lower fidelity
  - 8: default, balanced (~60-90s warm)
  - 12-20: high quality (~2-4min warm)

When the user asks for \"quick\" / \"draft\" / \"preview\", pick a low value. When they ask for \"high quality\" / \"final\" / \"really good\", pick a high value. Cold-start adds ~15s.

Returns a single image content block (image/png, base64)."
    )]
    async fn chat_img2img(
        &self,
        Parameters(args): Parameters<Img2ImgArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let body = Img2ImgRequest {
            prompt: args.prompt,
            images: args.images,
            steps: Some(args.steps.unwrap_or(DEFAULT_KONTEXT_STEPS).clamp(MIN_STEPS, MAX_KONTEXT_STEPS)),
        };
        run_tool(self.backend.clone(), ctx, BackendJob::Img2Img(body)).await
    }

    #[tool(
        name = "chat_inpaint",
        description = "Repaint a masked region of an image using the Flux Fill model and return a new PNG.

`image` is the base image, `mask` is the inpaint mask (same size). In the mask's red channel, white pixels mark the region to repaint and black pixels mark the region to keep. Both fields are base64 PNG / JPEG without a `data:` prefix.

`prompt` describes what to paint into the masked region. `negative_prompt` is optional and actually influences sampling because Flux Fill uses real CFG.

Speed / quality tradeoff via `steps`:
  - 10: fast (~2min warm)
  - 20: default, balanced (~4min warm)
  - 30-40: high quality (~6-8min warm)

When the user asks for \"quick\" / \"draft\", pick the low end. When they ask for \"final\" / \"really good\", pick the high end. Cold-start adds ~15s.

Returns a single image content block (image/png, base64)."
    )]
    async fn chat_inpaint(
        &self,
        Parameters(args): Parameters<InpaintArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let body = InpaintRequest {
            prompt: args.prompt,
            image: args.image,
            mask: args.mask,
            negative_prompt: args.negative_prompt,
            steps: Some(args.steps.unwrap_or(DEFAULT_INPAINT_STEPS).clamp(MIN_STEPS, MAX_INPAINT_STEPS)),
        };
        run_tool(self.backend.clone(), ctx, BackendJob::Inpaint(body)).await
    }
}

#[tool_handler]
impl ServerHandler for ChatImageTools {}

/// One generation job to dispatch through `run_tool`. Owned values
/// only — `run_tool` spawns the SSE pump on a fresh task so the body
/// can't borrow from the caller.
enum BackendJob {
    Img2Img(Img2ImgRequest),
    Inpaint(InpaintRequest),
}

/// Shared body for both tool handlers: spawn the SSE pump, forward
/// progress events as MCP notifications, resolve with the final image
/// or surface the error as a tool error.
async fn run_tool(
    backend: Arc<BackendConfig>,
    ctx: RequestContext<RoleServer>,
    job: BackendJob,
) -> Result<CallToolResult, ErrorData> {
    let progress_token: Option<ProgressToken> = ctx.meta.get_progress_token();
    let peer = ctx.peer.clone();

    let (tx, mut rx) = mpsc::channel::<BackendEvent>(16);
    let backend_for_pump = backend.clone();
    let pump = tokio::spawn(async move {
        match job {
            BackendJob::Img2Img(req) => backend_for_pump.img2img(&req, tx).await,
            BackendJob::Inpaint(req) => backend_for_pump.inpaint(&req, tx).await,
        }
    });

    let mut final_image: Option<String> = None;
    let mut final_error: Option<String> = None;

    while let Some(evt) = rx.recv().await {
        match evt {
            BackendEvent::Queued => {
                notify_progress(&peer, &progress_token, 0.0, None, Some("queued")).await;
            }
            BackendEvent::Progress(p) => {
                if p.max > 0 {
                    notify_progress(
                        &peer,
                        &progress_token,
                        p.value as f64,
                        Some(p.max as f64),
                        None,
                    )
                    .await;
                }
            }
            BackendEvent::Preview(_) => {
                // MCP tools resolve to a single result; mid-call
                // previews have no native MCP carrier. Drop them —
                // the final image is what matters.
            }
            BackendEvent::Done(img) => {
                final_image = Some(img.image_b64);
            }
            BackendEvent::Error(e) => {
                final_error = Some(e.message);
            }
        }
    }

    // Drain the pump's outcome so a transport error (network, 401)
    // surfaces as a tool error rather than getting silently swallowed.
    let pump_result = pump.await.unwrap_or_else(|e| {
        Err(backend::BackendError::Sse(format!("join: {e}")))
    });

    match (final_image, final_error, pump_result) {
        (Some(b64), _, _) => Ok(CallToolResult::success(vec![Content::image(
            b64,
            "image/png",
        )])),
        (None, Some(msg), _) => Ok(CallToolResult::error(vec![Content::text(msg)])),
        (None, None, Err(e)) => Ok(CallToolResult::error(vec![Content::text(
            e.to_string(),
        )])),
        (None, None, Ok(())) => Ok(CallToolResult::error(vec![Content::text(
            "backend stream ended without a result",
        )])),
    }
}

async fn notify_progress(
    peer: &rmcp::service::Peer<RoleServer>,
    token: &Option<ProgressToken>,
    progress: f64,
    total: Option<f64>,
    message: Option<&str>,
) {
    let Some(token) = token else { return };
    let params = ProgressNotificationParam {
        progress_token: token.clone(),
        progress,
        total,
        message: message.map(String::from),
    };
    if let Err(e) = peer.notify_progress(params).await {
        // Failure to deliver progress is non-fatal — log and continue.
        tracing::debug!("notify_progress failed: {e}");
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // MCP servers write protocol JSON on stdout; logs MUST go to
    // stderr or they'd corrupt the framed messages. tracing's default
    // subscriber already targets stderr.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("chat_mcp=info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let backend = BackendConfig::from_env()
        .context("failed to load backend config from environment")?;
    tracing::info!("connecting to chat backend at {}", backend.base_url);

    let server = ChatImageTools::new(backend);
    let running = server
        .serve(stdio())
        .await
        .context("rmcp serve(stdio) failed")?;
    running.waiting().await.context("rmcp service ended")?;
    Ok(())
}

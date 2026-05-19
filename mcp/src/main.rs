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
mod transport_http;

use std::sync::Arc;

use anyhow::Context as _;
use chat_shared::{
    Img2ImgRequest, InpaintRequest, Txt2ImgRequest, DEFAULT_INPAINT_STEPS,
    DEFAULT_KONTEXT_STEPS, MAX_INPAINT_STEPS, MAX_KONTEXT_STEPS, MIN_STEPS,
};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, ErrorData, Implementation, ProgressNotificationParam,
        ProgressToken, ProtocolVersion, RawResource, ServerCapabilities, ServerInfo,
    },
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

/// Parameters for `chat_txt2img`. Pure text-to-image — no reference
/// image, no mask.
#[derive(Debug, Deserialize, JsonSchema)]
struct Txt2ImgArgs {
    /// Natural-language description of the image. Example: "A photo
    /// of a snowy mountain at golden hour, dramatic lighting".
    prompt: String,
    /// Ollama model name to use, e.g. `gemma3:4b-image`. Optional —
    /// the backend falls back to its configured default
    /// (`OLLAMA_MODEL` env) when unset. Call `chat_list_image_models`
    /// first to discover what's installed.
    #[serde(default)]
    model: Option<String>,
    /// See `Img2ImgArgs::inline`. Default false — return URL only.
    #[serde(default)]
    inline: bool,
}

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
    /// When `true`, the tool result includes the rendered PNG inline
    /// as a base64 image content block so the model can reason about
    /// it (for chained edits, visual critique, etc.). Default is
    /// `false`: only a fetch URL is returned, which keeps the bytes
    /// out of the LLM context. Set to `true` only when the agent
    /// needs to *see* the result; the user can save it from the URL
    /// regardless.
    #[serde(default)]
    inline: bool,
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
    /// See `Img2ImgArgs::inline`. Default false — return URL only.
    #[serde(default)]
    inline: bool,
}

/// MCP tool handler. Holds the resolved backend config so each tool
/// call doesn't re-read env on every invocation.
#[derive(Clone)]
pub struct ChatImageTools {
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
        name = "chat_list_image_models",
        description = "List the Ollama models currently installed on the chat backend that advertise the `image` capability — i.e. those usable with `chat_txt2img`. Call this first when the user asks for txt2img with a specific style or you need to verify a model is available. Result is a JSON array of {name, families}; pass an entry's `name` straight to `chat_txt2img`.

This does NOT list ComfyUI workflows. `chat_img2img` (Flux Kontext) and `chat_inpaint` (Flux Fill) have fixed model selections at the workflow level — no model picker for those.

Cheap to call repeatedly; the backend caches Ollama capability lookups for 5 minutes.",
        annotations(
            title = "List image models",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true,
        )
    )]
    async fn chat_list_image_models(&self) -> Result<CallToolResult, ErrorData> {
        match self.backend.list_image_models().await {
            Ok(resp) => Content::json(resp).map(|c| CallToolResult::success(vec![c])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(
        name = "chat_txt2img",
        description = "**IMPORTANT: If `model` is not provided, the backend uses a configured default. When no default is set, this call fails with an actionable error directing you to `chat_list_image_models`. When in doubt about which model to use, call `chat_list_image_models` first.**

Generate an image from a text prompt using Ollama's image-generation endpoint.

Use this when the user wants a fresh image and has not provided any reference image. If they DID provide a reference image they want edited, use `chat_img2img` instead. If they want a region of an existing image repainted under a mask, use `chat_inpaint`.

By default the result contains a fetch URL only — the PNG bytes are NOT in your context. Set `inline: true` when you need to see the result to reason about it (chained edits, visual critique).

No `steps` knob — Ollama's image API doesn't expose sampler controls. Typical render takes 15-60s warm, plus a model-load tax on cold start.",
        annotations(
            title = "Generate image from text",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true,
        )
    )]
    async fn chat_txt2img(
        &self,
        Parameters(args): Parameters<Txt2ImgArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let body = Txt2ImgRequest {
            prompt: args.prompt,
            model: args.model,
        };
        run_tool(self.backend.clone(), ctx, BackendJob::Txt2Img(body), args.inline).await
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

By default the result contains a fetch URL only — the PNG bytes are NOT in your context. Set `inline: true` when you need to see the result to reason about it (chained edits, visual critique).",
        annotations(
            title = "Edit image (Flux Kontext)",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true,
        )
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
        run_tool(self.backend.clone(), ctx, BackendJob::Img2Img(body), args.inline).await
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

By default the result contains a fetch URL only — the PNG bytes are NOT in your context. Set `inline: true` when you need to see the result to reason about it (chained edits, visual critique).",
        annotations(
            title = "Inpaint masked region (Flux Fill)",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true,
        )
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
        run_tool(self.backend.clone(), ctx, BackendJob::Inpaint(body), args.inline).await
    }
}

#[tool_handler]
impl ServerHandler for ChatImageTools {
    // Spec-strict MCP clients (Claude Code in particular) refuse to
    // call `tools/list` unless the server's `initialize` response
    // advertises `capabilities.tools`. The `#[tool_handler]` macro
    // injects `call_tool` and `list_tools` but NOT `get_info`, so
    // the default `ServerInfo` ships with empty capabilities and the
    // client sees zero tools. Override `get_info` to enable the
    // tools capability explicitly.
    //
    // `instructions` is a free-form string MCP clients show as
    // system-prompt-style guidance every time these tools are loaded.
    // Use it to teach the agent the non-obvious bits up front so it
    // doesn't have to discover them through failed calls.
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::default(),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation::from_build_env(),
            instructions: Some(
                "Image generation tools for the chat backend. Three generators \
                 (chat_txt2img, chat_img2img, chat_inpaint) and one discovery \
                 tool (chat_list_image_models).\n\n\
                 USAGE NOTES:\n\
                 - chat_txt2img is Ollama-backed. If you don't pass `model`, \
                   the backend uses CHAT_DEFAULT_IMAGE_MODEL; if that's unset \
                   it will reject the call with an actionable error pointing \
                   you at chat_list_image_models. When in doubt, call \
                   chat_list_image_models first.\n\
                 - chat_img2img and chat_inpaint are ComfyUI-backed with \
                   fixed workflow models (Flux Kontext / Flux Fill). No \
                   `model` parameter for those.\n\
                 - Results return a fetch URL by default; the PNG bytes are \
                   NOT in your context. URLs expire after ~30 min. Pass \
                   `inline: true` when you need to see the image yourself \
                   (chained edits, visual critique) — that's the only time \
                   the bytes enter your context window.\n\
                 - All three generators support a `steps` knob (except \
                   txt2img — Ollama doesn't expose sampler controls). Low \
                   steps = fast draft, high steps = better quality."
                    .into(),
            ),
        }
    }
}

/// One generation job to dispatch through `run_tool`. Owned values
/// only — `run_tool` spawns the SSE pump on a fresh task so the body
/// can't borrow from the caller.
enum BackendJob {
    Txt2Img(Txt2ImgRequest),
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
    inline: bool,
) -> Result<CallToolResult, ErrorData> {
    let progress_token: Option<ProgressToken> = ctx.meta.get_progress_token();
    let peer = ctx.peer.clone();

    let (tx, mut rx) = mpsc::channel::<BackendEvent>(16);
    let backend_for_pump = backend.clone();
    let pump = tokio::spawn(async move {
        match job {
            BackendJob::Txt2Img(req) => backend_for_pump.txt2img(&req, tx).await,
            BackendJob::Img2Img(req) => backend_for_pump.img2img(&req, tx).await,
            BackendJob::Inpaint(req) => backend_for_pump.inpaint(&req, tx).await,
        }
    });

    let mut final_b64: Option<String> = None;
    let mut final_uuid: Option<String> = None;
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
                final_uuid = if img.uuid.is_empty() {
                    None
                } else {
                    Some(img.uuid)
                };
                final_b64 = Some(img.image_b64);
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

    match (final_b64, final_error, pump_result) {
        (Some(b64), _, _) => Ok(success_result(&backend, &b64, final_uuid.as_deref(), inline)),
        (None, Some(msg), _) => Ok(CallToolResult::error(vec![Content::text(msg)])),
        (None, None, Err(e)) => Ok(CallToolResult::error(vec![Content::text(
            e.to_string(),
        )])),
        (None, None, Ok(())) => Ok(CallToolResult::error(vec![Content::text(
            "backend stream ended without a result",
        )])),
    }
}

/// Build the `CallToolResult` content vec for a successful render.
///
/// Default shape (`inline=false`):
///   - `Content::text(url)` — human-/curl-friendly. Always emitted
///     when the backend stored the image (i.e. `uuid` is non-empty).
///   - `Content::resource_link(uri=url)` — spec-native hint for MCP
///     clients that render resource links as actionable items
///     (Claude Desktop "Open" button etc.).
///   - The base64 image is *not* included, so the bytes never enter
///     the LLM's context window.
///
/// `inline=true` adds `Content::image(b64, "image/png")` on top so
/// the model can reason about the render directly.
///
/// Fallback: if the backend failed to store the blob (`uuid` empty),
/// we always include the inline image — otherwise the agent would
/// get nothing back.
fn success_result(
    backend: &BackendConfig,
    b64: &str,
    uuid: Option<&str>,
    inline: bool,
) -> CallToolResult {
    let mut content = Vec::with_capacity(3);
    let stored = uuid.is_some();
    if let Some(uuid) = uuid {
        let url = backend.image_url(uuid);
        content.push(Content::text(format!(
            "Image saved at {url} (expires in ~30 min)."
        )));
        content.push(Content::resource_link(RawResource {
            uri: url,
            name: format!("{uuid}.png"),
            title: None,
            description: Some("Rendered image".into()),
            mime_type: Some("image/png".into()),
            size: None,
            icons: None,
        }));
    }
    if inline || !stored {
        content.push(Content::image(b64.to_string(), "image/png"));
    }
    CallToolResult::success(content)
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

    let transport = std::env::var("CHAT_MCP_TRANSPORT").unwrap_or_else(|_| "stdio".into());
    let server = ChatImageTools::new(backend);

    match transport.as_str() {
        "stdio" => {
            let running = server
                .serve(stdio())
                .await
                .context("rmcp serve(stdio) failed")?;
            running.waiting().await.context("rmcp service ended")?;
        }
        "http" => {
            let cfg = transport_http::HttpConfig::from_env()
                .context("invalid http transport config")?;
            transport_http::serve(server, cfg)
                .await
                .context("http transport exited with error")?;
        }
        other => anyhow::bail!(
            "unknown CHAT_MCP_TRANSPORT={other:?}; expected `stdio` or `http`"
        ),
    }
    Ok(())
}

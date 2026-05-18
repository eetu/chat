//! HTTP client that drives the chat backend's `/api/v1/*` SSE endpoints.
//!
//! Each generation call POSTs JSON, parses an SSE stream, and pumps
//! `progress` / `preview` / `done` / `error` events back to the caller
//! via a `tokio::sync::mpsc::Sender`. The caller decides what to do
//! with each event — typically: turn `progress` into an MCP
//! `notifications/progress`, ignore `preview` for now (MCP tools are
//! single-shot results so previews have nowhere to land), and resolve
//! the tool result from the `done` event.

use chat_shared::{
    ErrorPayload, ImageModelsResponse, ImageResponse, Img2ImgRequest, InpaintRequest,
    PreviewPayload, ProgressPayload, Txt2ImgRequest,
};
use futures_util::StreamExt;
use reqwest::Client;
use reqwest_eventsource::{Event as SseEvent, EventSource};
use serde::Serialize;
use tokio::sync::mpsc;

/// Events forwarded out of the SSE pump. Order roughly matches the
/// SSE stream itself, with `Done` or `Error` always last.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `Preview` is parsed but currently dropped by the consumer.
pub enum BackendEvent {
    /// Emitted when the backend's image_sem was already at capacity.
    /// One-shot. Useful UX signal: "waiting for GPU".
    Queued,
    Progress(ProgressPayload),
    Preview(PreviewPayload),
    Done(ImageResponse),
    Error(ErrorPayload),
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("missing CHAT_BACKEND_URL env var")]
    NoUrl,
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("backend status {status}: {body}")]
    Status { status: u16, body: String },
    #[error("sse parse: {0}")]
    Sse(String),
    #[error("backend stream ended without a terminal event")]
    UnexpectedEof,
}

/// Configuration plucked once at server startup from env. Keeping it
/// in one struct makes the tool handlers trivially testable later.
#[derive(Debug, Clone)]
pub struct BackendConfig {
    pub base_url: String,
    /// mcp→backend Bearer. Optional — the backend's `/api/v1/*`
    /// extractor also treats an unset `CHAT_MCP_API_KEY` as
    /// auth-disabled, so the two hops are consistent. When set we
    /// attach `Authorization: Bearer <key>` to every request; when
    /// unset we skip the header entirely.
    pub api_key: Option<String>,
    pub client: Client,
}

impl BackendConfig {
    pub fn from_env() -> Result<Self, BackendError> {
        let base_url = std::env::var("CHAT_BACKEND_URL")
            .map_err(|_| BackendError::NoUrl)?
            .trim_end_matches('/')
            .to_string();
        // Unset or empty key = auth-off mode. Backend's ApiKey
        // extractor mirrors this, so the bearer header is harmless
        // when omitted on both ends.
        let api_key = std::env::var("CHAT_MCP_API_KEY").ok().filter(|s| !s.is_empty());
        // Long-running SSE: rely on reqwest's default (no timeout).
        // Don't call `.timeout(...)` here — a zero-duration value
        // would mean *immediate* timeout, not unbounded.
        let client = Client::builder().build()?;
        Ok(Self {
            base_url,
            api_key,
            client,
        })
    }

    /// POST `/api/v1/txt2img` and pump SSE events into `tx`.
    pub async fn txt2img(
        &self,
        body: &Txt2ImgRequest,
        tx: mpsc::Sender<BackendEvent>,
    ) -> Result<(), BackendError> {
        let url = format!("{}/api/v1/txt2img", self.base_url);
        self.run_sse(&url, body, tx).await
    }

    /// POST `/api/v1/img2img` and pump SSE events into `tx`.
    pub async fn img2img(
        &self,
        body: &Img2ImgRequest,
        tx: mpsc::Sender<BackendEvent>,
    ) -> Result<(), BackendError> {
        let url = format!("{}/api/v1/img2img", self.base_url);
        self.run_sse(&url, body, tx).await
    }

    /// GET `/api/v1/models/image` and return the parsed listing. This
    /// is a normal JSON call, not SSE — no streaming on the wire.
    pub async fn list_image_models(&self) -> Result<ImageModelsResponse, BackendError> {
        let url = format!("{}/api/v1/models/image", self.base_url);
        let mut req = self.client.get(url);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(BackendError::Status {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: ImageModelsResponse = resp.json().await?;
        Ok(parsed)
    }

    /// POST `/api/v1/inpaint` and pump SSE events into `tx`.
    pub async fn inpaint(
        &self,
        body: &InpaintRequest,
        tx: mpsc::Sender<BackendEvent>,
    ) -> Result<(), BackendError> {
        let url = format!("{}/api/v1/inpaint", self.base_url);
        self.run_sse(&url, body, tx).await
    }

    async fn run_sse<B: Serialize>(
        &self,
        url: &str,
        body: &B,
        tx: mpsc::Sender<BackendEvent>,
    ) -> Result<(), BackendError> {
        let mut req = self.client.post(url).json(body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let mut es = EventSource::new(req)
            .map_err(|e| BackendError::Sse(e.to_string()))?;

        while let Some(event) = es.next().await {
            match event {
                Ok(SseEvent::Open) => {}
                Ok(SseEvent::Message(msg)) => {
                    let kind = msg.event.as_str();
                    let parsed = decode_event(kind, &msg.data)?;
                    let is_terminal = matches!(
                        parsed,
                        BackendEvent::Done(_) | BackendEvent::Error(_)
                    );
                    if tx.send(parsed).await.is_err() {
                        // Consumer dropped — close the stream so the
                        // backend stops sampling. EventSource::close
                        // drops the underlying connection.
                        es.close();
                        return Ok(());
                    }
                    if is_terminal {
                        es.close();
                        return Ok(());
                    }
                }
                Err(reqwest_eventsource::Error::StreamEnded) => {
                    return Err(BackendError::UnexpectedEof);
                }
                Err(reqwest_eventsource::Error::InvalidStatusCode(status, resp)) => {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(BackendError::Status {
                        status: status.as_u16(),
                        body,
                    });
                }
                Err(e) => {
                    return Err(BackendError::Sse(e.to_string()));
                }
            }
        }
        Err(BackendError::UnexpectedEof)
    }
}

fn decode_event(kind: &str, data: &str) -> Result<BackendEvent, BackendError> {
    match kind {
        "queued" => Ok(BackendEvent::Queued),
        "progress" => {
            let p: ProgressPayload =
                serde_json::from_str(data).map_err(|e| BackendError::Sse(e.to_string()))?;
            Ok(BackendEvent::Progress(p))
        }
        "preview" => {
            let p: PreviewPayload =
                serde_json::from_str(data).map_err(|e| BackendError::Sse(e.to_string()))?;
            Ok(BackendEvent::Preview(p))
        }
        "done" => {
            let p: ImageResponse =
                serde_json::from_str(data).map_err(|e| BackendError::Sse(e.to_string()))?;
            Ok(BackendEvent::Done(p))
        }
        "error" => {
            let p: ErrorPayload =
                serde_json::from_str(data).map_err(|e| BackendError::Sse(e.to_string()))?;
            Ok(BackendEvent::Error(p))
        }
        other => Err(BackendError::Sse(format!("unknown event: {other}"))),
    }
}

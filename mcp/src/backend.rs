//! HTTP client that drives the chat backend's `/api/v1/*` SSE endpoints.
//!
//! Each generation call POSTs JSON, parses an SSE stream, and pumps
//! `progress` / `preview` / `done` / `error` events back to the caller
//! via a `tokio::sync::mpsc::Sender`. The caller decides what to do
//! with each event — typically: turn `progress` into an MCP
//! `notifications/progress`, ignore `preview` for now (MCP tools are
//! single-shot results so previews have nowhere to land), and resolve
//! the tool result from the `done` event.

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use chat_shared::{
    ErrorPayload, ImageResponse, Img2ImgRequest, InpaintRequest, PreviewPayload, ProgressPayload,
    Txt2ImgRequest,
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
    /// Internal URL used by chat-mcp itself when calling
    /// `/api/v1/*` — usually a container-network or LAN hostname.
    pub base_url: String,
    /// Externally-routable URL the *user* would hit to fetch a
    /// stored image (e.g. `https://chat.example.com`). Defaults to
    /// `base_url` when `CHAT_BACKEND_PUBLIC_URL` is unset, which is
    /// correct for local dev where the two are the same host.
    pub public_url: String,
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
        let public_url = std::env::var("CHAT_BACKEND_PUBLIC_URL")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|s| s.trim_end_matches('/').to_string())
            .unwrap_or_else(|| base_url.clone());
        // Unset or empty key = auth-off mode. Backend's ApiKey
        // extractor mirrors this, so the bearer header is harmless
        // when omitted on both ends.
        let api_key = std::env::var("CHAT_MCP_API_KEY")
            .ok()
            .filter(|s| !s.is_empty());
        // Long-running SSE: rely on reqwest's default (no timeout).
        // Don't call `.timeout(...)` here — a zero-duration value
        // would mean *immediate* timeout, not unbounded.
        let client = Client::builder().build()?;
        Ok(Self {
            base_url,
            public_url,
            api_key,
            client,
        })
    }

    /// Build the externally-routable URL for a stored image so the
    /// MCP tool can hand the user something they can `curl` or open
    /// in a browser. Bears no relation to `base_url` once
    /// `CHAT_BACKEND_PUBLIC_URL` diverges.
    pub fn image_url(&self, uuid: &str) -> String {
        format!("{}/api/v1/images/{}.png", self.public_url, uuid)
    }

    /// Re-hydrate an image *reference* into inline base64 (no `data:`
    /// prefix), so an agent can chain edits by passing back the short
    /// id/URL it got from a previous render instead of re-supplying the
    /// raw bytes (which it can't — a ~125 KB base64 blob won't fit in a
    /// tool-call argument).
    ///
    /// A `reference` is interpreted, in order:
    ///   1. `http(s)://…` — fetched as-is. Covers the public capability
    ///      URL from a prior render (`image_url`) and any other reachable
    ///      image.
    ///   2. a bare image id, `<uuid>` or `<uuid>.png` (the `uuid` from a
    ///      prior `done` event) — fetched from `/api/v1/images/<uuid>.png`
    ///      on the *internal* `base_url`.
    ///   3. anything else — assumed to already be inline base64 and
    ///      returned unchanged, so existing callers keep working.
    ///
    /// The `/api/v1/images/{uuid}.png` route is unauthenticated by design
    /// (capability URL), so no bearer is attached — and we deliberately
    /// never attach it to arbitrary external URLs either, to avoid
    /// leaking the MCP key off-host.
    pub async fn resolve_image(&self, reference: &str) -> Result<String, BackendError> {
        let url = if reference.starts_with("http://") || reference.starts_with("https://") {
            reference.to_string()
        } else if let Some(uuid) = as_image_id(reference) {
            format!("{}/api/v1/images/{}.png", self.base_url, uuid)
        } else {
            // Not a reference — already inline base64.
            return Ok(reference.to_string());
        };

        let resp = self.client.get(&url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(BackendError::Status {
                status: status.as_u16(),
                body,
            });
        }
        let bytes = resp.bytes().await?;
        Ok(STANDARD.encode(&bytes))
    }

    /// Resolve a batch of image references, preserving order. Used for
    /// `chat_img2img`'s `images` list.
    pub async fn resolve_images(&self, refs: &[String]) -> Result<Vec<String>, BackendError> {
        let mut out = Vec::with_capacity(refs.len());
        for r in refs {
            out.push(self.resolve_image(r).await?);
        }
        Ok(out)
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
        let mut es = EventSource::new(req).map_err(|e| BackendError::Sse(e.to_string()))?;

        while let Some(event) = es.next().await {
            match event {
                Ok(SseEvent::Open) => {}
                Ok(SseEvent::Message(msg)) => {
                    let kind = msg.event.as_str();
                    let parsed = decode_event(kind, &msg.data)?;
                    let is_terminal =
                        matches!(parsed, BackendEvent::Done(_) | BackendEvent::Error(_));
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

/// Return the bare uuid when `s` is an image id — `<uuid>` or
/// `<uuid>.png` in the canonical 8-4-4-4-12 hyphenated form. A base64
/// blob never matches (wrong length, non-hex bytes, no hyphens at the
/// fixed offsets), so reference-detection can't misfire on real image
/// data.
fn as_image_id(s: &str) -> Option<&str> {
    let id = s.strip_suffix(".png").unwrap_or(s);
    is_canonical_uuid(id).then_some(id)
}

fn is_canonical_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    b.iter().enumerate().all(|(i, c)| match i {
        8 | 13 | 18 | 23 => *c == b'-',
        _ => c.is_ascii_hexdigit(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_bare_and_suffixed_uuids() {
        let uuid = "f47ac10b-58cc-4372-a567-0e02b2c3d479";
        assert_eq!(as_image_id(uuid), Some(uuid));
        assert_eq!(as_image_id(&format!("{uuid}.png")), Some(uuid));
    }

    #[test]
    fn rejects_base64_and_other_non_ids() {
        // A real base64 PNG blob must never be mistaken for an id, or
        // resolve_image would try to fetch garbage instead of passing
        // the bytes through.
        let b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        assert_eq!(as_image_id(b64), None);
        assert_eq!(as_image_id(""), None);
        assert_eq!(as_image_id("not-a-uuid"), None);
        // Right length, wrong hyphen placement.
        assert_eq!(as_image_id("f47ac10b58cc-4372-a567-0e02b2c3d4790"), None);
        // Right length & hyphens, non-hex byte.
        assert_eq!(as_image_id("g47ac10b-58cc-4372-a567-0e02b2c3d479"), None);
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

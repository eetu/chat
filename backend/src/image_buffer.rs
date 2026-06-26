//! Short-lived in-memory cache for rendered images.
//!
//! Used by the `/api/v1/*` generation handlers to hand callers a
//! pointer (`uuid`) they can fetch via `GET /api/v1/images/{uuid}.png`
//! instead of (or in addition to) inlining the bytes in the SSE
//! `done` event. The MCP bridge swaps the inline-bytes path for the
//! pointer-fetch path: tool results return a URL the user can curl,
//! and the bytes never enter the LLM's context.
//!
//! Eviction policy: oldest-entry-out when `image_buffer_limit` is
//! reached at insert time; periodic sweep drops anything older than
//! `image_buffer_ttl_secs`. Both knobs are env-configurable.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::STANDARD, Engine};
use bytes::Bytes;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::AppState;

/// One cached render. `inserted_at` is captured on insert and used
/// both for TTL expiry and eviction-by-age.
#[derive(Debug, Clone)]
pub struct ImageBlob {
    pub bytes: Bytes,
    pub mime: &'static str,
    pub inserted_at: Instant,
}

/// Thread-safe blob store. `Arc<RwLock<...>>` is overkill for the
/// access pattern (mostly inserts, occasional reads from the
/// fetch handler) but keeps the API simple and is what the rest of
/// `AppState` already uses.
#[derive(Default)]
pub struct ImageBuffer {
    inner: RwLock<HashMap<Uuid, ImageBlob>>,
}

impl ImageBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a base64 PNG and return its `uuid`. Decodes once so
    /// `GET /api/v1/images/{uuid}.png` can stream raw bytes without
    /// re-decoding per request.
    ///
    /// When the buffer is at `limit`, the oldest entry is evicted
    /// before the new one is inserted — bounded memory regardless of
    /// upstream call rate.
    pub async fn insert(&self, b64: &str, limit: usize) -> Result<Uuid, base64::DecodeError> {
        let raw = STANDARD.decode(b64)?;
        let bytes = Bytes::from(raw);
        let id = Uuid::new_v4();
        let blob = ImageBlob {
            bytes,
            mime: "image/png",
            inserted_at: Instant::now(),
        };
        let mut g = self.inner.write().await;
        if g.len() >= limit {
            if let Some(oldest_id) = g.iter().min_by_key(|(_, b)| b.inserted_at).map(|(k, _)| *k) {
                g.remove(&oldest_id);
            }
        }
        g.insert(id, blob);
        Ok(id)
    }

    /// Look up a stored blob. `None` for unknown or expired ids; the
    /// caller (`get_image`) turns that into a 404.
    pub async fn get(&self, id: Uuid, ttl: Duration) -> Option<ImageBlob> {
        let g = self.inner.read().await;
        g.get(&id)
            .cloned()
            .filter(|b| b.inserted_at.elapsed() < ttl)
    }

    /// Drop everything older than `ttl`. Returns the count removed
    /// so the sweep task can log it.
    pub async fn sweep(&self, ttl: Duration) -> usize {
        let mut g = self.inner.write().await;
        let before = g.len();
        g.retain(|_, b| b.inserted_at.elapsed() < ttl);
        before - g.len()
    }
}

/// Spawn the periodic sweep task. Runs every 60s — shorter would
/// chew CPU on idle hosts, longer would briefly overshoot the TTL by
/// up to a minute, which is fine for a 30-min default.
pub fn start_sweep_loop(state: Arc<AppState>) {
    tokio::spawn(async move {
        let ttl = Duration::from_secs(state.settings.image_buffer_ttl_secs);
        let interval = Duration::from_secs(60);
        loop {
            tokio::time::sleep(interval).await;
            let dropped = state.image_buffer.sweep(ttl).await;
            if dropped > 0 {
                tracing::debug!("image buffer sweep: dropped {dropped} expired entries");
            }
        }
    });
}

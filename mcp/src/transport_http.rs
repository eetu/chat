//! Streamable-HTTP transport for `chat-mcp`.
//!
//! Wraps `rmcp::transport::streamable_http_server::tower::StreamableHttpService`
//! in an axum router, gates `/mcp` behind a `Authorization: Bearer ...`
//! check against `CHAT_MCP_SERVER_KEY`, and exposes an unauthenticated
//! `GET /health` for container probes / reverse-proxy health checks.
//!
//! The session manager is the stock in-memory `LocalSessionManager`. A
//! session ID is created on the client's first `initialize` request,
//! returned in the `Mcp-Session-Id` response header, and required on
//! every subsequent request. This is what allows in-flight progress
//! notifications to be delivered against an ongoing tool call — they
//! ride the session's SSE stream.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::{HeaderValue, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::{any_service, get},
    Router,
};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager,
    tower::{StreamableHttpServerConfig, StreamableHttpService},
};
use subtle::ConstantTimeEq;
use tower::ServiceBuilder;

use crate::ChatImageTools;

/// HTTP listener config. Pulled from env at startup so the rest of
/// the module is deterministic.
#[derive(Debug, Clone)]
pub struct HttpConfig {
    pub bind: SocketAddr,
    /// Optional client-facing Bearer token. `None` disables the auth
    /// middleware entirely — the `/mcp` mount is reachable by anyone
    /// who can hit the bind address. Intended for trusted LAN
    /// deployments behind another auth layer (or `127.0.0.1` binds);
    /// a startup warning is logged so accidental public exposure
    /// surfaces in the logs.
    pub server_key: Option<String>,
    /// Path the MCP service is mounted at. `/mcp` is the
    /// streamable-HTTP convention.
    pub mount_path: String,
}

impl HttpConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let port: u16 = std::env::var("PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8090);
        let bind_host = std::env::var("CHAT_MCP_BIND").unwrap_or_else(|_| "0.0.0.0".into());
        let bind: SocketAddr = format!("{bind_host}:{port}")
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid bind {bind_host}:{port}: {e}"))?;
        // Auth is opt-in via env presence: unset or empty
        // `CHAT_MCP_SERVER_KEY` disables the Bearer middleware. An
        // empty string is treated the same as missing so deployment
        // tooling doesn't accidentally enable auth with a blank value.
        let server_key = std::env::var("CHAT_MCP_SERVER_KEY")
            .ok()
            .filter(|s| !s.is_empty());
        let mount_path = std::env::var("CHAT_MCP_MOUNT_PATH").unwrap_or_else(|_| "/mcp".into());
        Ok(Self {
            bind,
            server_key,
            mount_path,
        })
    }
}

#[derive(Clone)]
struct AuthState {
    expected: Arc<String>,
}

/// Reject anything that doesn't carry the right Bearer token. Constant
/// time compare so the key length can't be probed by timing. Returns
/// 401 with `WWW-Authenticate` so the client knows what scheme to use.
async fn require_bearer(
    State(state): State<AuthState>,
    req: Request,
    next: Next,
) -> Result<Response, Response> {
    let presented = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(|s| s.trim());
    let ok = match presented {
        Some(token) => token.as_bytes().ct_eq(state.expected.as_bytes()).into(),
        None => false,
    };
    if ok {
        Ok(next.run(req).await)
    } else {
        let mut resp = Response::new(axum::body::Body::from(
            r#"{"error":"missing or invalid bearer token"}"#,
        ));
        *resp.status_mut() = StatusCode::UNAUTHORIZED;
        resp.headers_mut().insert(
            axum::http::header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"chat-mcp\""),
        );
        resp.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        Err(resp)
    }
}

/// Unauthenticated liveness probe. Returns 200 with a one-line body so
/// `curl --fail` and reverse-proxy probes (`HealthCmd=wget --spider`)
/// can tell the listener is up without holding the server key.
async fn health() -> &'static str {
    "ok\n"
}

/// Stand up the axum service and serve until the process is killed.
/// `tools` is cloned on every session so each MCP connection gets a
/// fresh router; the inner `Arc<BackendConfig>` is shared.
pub async fn serve(tools: ChatImageTools, cfg: HttpConfig) -> anyhow::Result<()> {
    // `StreamableHttpService` calls the factory once per new session.
    // Cloning a `ChatImageTools` is cheap — it's an `Arc<BackendConfig>`
    // plus a router descriptor.
    let factory_tools = tools.clone();
    let mcp_service = StreamableHttpService::new(
        move || Ok(factory_tools.clone()),
        Arc::new(LocalSessionManager::default()),
        // Default: stateful_mode=true, which is what we want — session
        // IDs let progress notifications find the right SSE stream.
        StreamableHttpServerConfig::default(),
    );

    let mut mcp_router = Router::new().route_service(&cfg.mount_path, any_service(mcp_service));
    let auth_enabled = if let Some(key) = cfg.server_key.clone() {
        let auth_state = AuthState {
            expected: Arc::new(key),
        };
        mcp_router = mcp_router.layer(
            ServiceBuilder::new().layer(middleware::from_fn_with_state(auth_state, require_bearer)),
        );
        true
    } else {
        tracing::warn!(
            "CHAT_MCP_SERVER_KEY is unset — http transport will accept unauthenticated MCP requests"
        );
        false
    };

    let app = Router::new()
        .route("/health", get(health))
        .merge(mcp_router);

    tracing::info!(
        "chat-mcp http transport listening on {} (mount={}, auth={})",
        cfg.bind,
        cfg.mount_path,
        if auth_enabled { "bearer" } else { "off" },
    );
    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

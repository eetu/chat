use std::future::{ready, Ready};
use std::sync::Arc;

use actix_session::Session;
use actix_web::{web, FromRequest, HttpRequest, HttpResponse, ResponseError};
use openidconnect::{Nonce, PkceCodeVerifier};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::AppState;

const SESSION_KEY_USER: &str = "user";
const SESSION_KEY_CSRF: &str = "oidc.csrf";
const SESSION_KEY_NONCE: &str = "oidc.nonce";
const SESSION_KEY_PKCE: &str = "oidc.pkce";
const SESSION_KEY_NEXT: &str = "oidc.next";

/// Accept only same-origin absolute paths. Rejects protocol-relative
/// (`//evil.com`), schemes, backslashes, and empty strings. Returns the
/// canonical path or `"/"` as default.
fn sanitize_next(raw: Option<&str>) -> String {
    match raw {
        Some(s) if s.starts_with('/') && !s.starts_with("//") && !s.contains('\\') => {
            s.to_string()
        }
        _ => "/".to_string(),
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AuthUser {
    pub sub: String,
    pub username: String,
}

#[derive(Error, Debug)]
pub enum AuthError {
    #[error("unauthenticated")]
    Unauthenticated,
    #[error("session error")]
    Session,
}

impl ResponseError for AuthError {
    fn status_code(&self) -> actix_web::http::StatusCode {
        actix_web::http::StatusCode::UNAUTHORIZED
    }
    fn error_response(&self) -> HttpResponse {
        HttpResponse::Unauthorized().json(serde_json::json!({"error": self.to_string()}))
    }
}

impl FromRequest for AuthUser {
    type Error = AuthError;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, payload: &mut actix_web::dev::Payload) -> Self::Future {
        let session = match Session::from_request(req, payload).into_inner() {
            Ok(s) => s,
            Err(_) => return ready(Err(AuthError::Session)),
        };
        match session.get::<AuthUser>(SESSION_KEY_USER) {
            Ok(Some(user)) => ready(Ok(user)),
            _ => ready(Err(AuthError::Unauthenticated)),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct LoginQuery {
    /// Used in dev mode only.
    pub username: Option<String>,
    /// Same-origin path to redirect to after successful auth.
    pub next: Option<String>,
}

/// GET /auth/login
///
/// Priority:
/// 1. OIDC configured + provider discovered → 302 to `authorize_url`
/// 2. `DEV_AUTH=1` → write a session for `?username=foo` and 302 to `/`
/// 3. Otherwise → 503
pub async fn login(
    state: web::Data<Arc<AppState>>,
    session: Session,
    query: web::Query<LoginQuery>,
) -> HttpResponse {
    let next = sanitize_next(query.next.as_deref());

    if let Some(oidc) = &state.oidc {
        let auth = oidc.authorize();
        if let Err(e) = session.insert(SESSION_KEY_CSRF, auth.csrf.secret()) {
            tracing::error!("session csrf insert: {e}");
            return HttpResponse::InternalServerError().finish();
        }
        if let Err(e) = session.insert(SESSION_KEY_NONCE, auth.nonce.secret()) {
            tracing::error!("session nonce insert: {e}");
            return HttpResponse::InternalServerError().finish();
        }
        if let Err(e) = session.insert(SESSION_KEY_PKCE, auth.pkce_verifier.secret()) {
            tracing::error!("session pkce insert: {e}");
            return HttpResponse::InternalServerError().finish();
        }
        if let Err(e) = session.insert(SESSION_KEY_NEXT, &next) {
            tracing::error!("session next insert: {e}");
            return HttpResponse::InternalServerError().finish();
        }
        return HttpResponse::Found()
            .append_header(("Location", auth.url.to_string()))
            .finish();
    }

    if state.settings.dev_auth {
        let username = query.username.clone().unwrap_or_else(|| "dev".into());
        let sub = format!("dev:{username}");
        let user = AuthUser { sub: sub.clone(), username: username.clone() };
        if let Err(e) = state.storage.upsert_user(&sub, &username) {
            tracing::error!("upsert_user failed: {e}");
        }
        if let Err(e) = session.insert(SESSION_KEY_USER, &user) {
            tracing::error!("session insert failed: {e}");
        }
        return HttpResponse::Found().append_header(("Location", next)).finish();
    }

    HttpResponse::ServiceUnavailable().json(serde_json::json!({
        "error": "auth not configured. set DEV_AUTH=1 or OIDC_* env vars"
    }))
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

/// GET /auth/callback
pub async fn callback(
    state: web::Data<Arc<AppState>>,
    session: Session,
    query: web::Query<CallbackQuery>,
) -> HttpResponse {
    let oidc = match &state.oidc {
        Some(o) => o,
        None => {
            return HttpResponse::ServiceUnavailable()
                .json(serde_json::json!({"error": "oidc not configured"}));
        }
    };

    if let Some(err) = &query.error {
        tracing::warn!(
            "oidc provider returned error: {err} ({:?})",
            query.error_description
        );
        return HttpResponse::BadRequest()
            .json(serde_json::json!({"error": err, "description": query.error_description}));
    }

    let code = match &query.code {
        Some(c) => c.clone(),
        None => return HttpResponse::BadRequest().body("missing code"),
    };

    let returned_state = match &query.state {
        Some(s) => s.clone(),
        None => return HttpResponse::BadRequest().body("missing state"),
    };

    let stored_csrf = session.get::<String>(SESSION_KEY_CSRF).ok().flatten();
    let stored_nonce = session.get::<String>(SESSION_KEY_NONCE).ok().flatten();
    let stored_pkce = session.get::<String>(SESSION_KEY_PKCE).ok().flatten();
    let stored_next = session.get::<String>(SESSION_KEY_NEXT).ok().flatten();

    // Always clear the OIDC handshake values once we've read them.
    session.remove(SESSION_KEY_CSRF);
    session.remove(SESSION_KEY_NONCE);
    session.remove(SESSION_KEY_PKCE);
    session.remove(SESSION_KEY_NEXT);

    let (csrf, nonce, pkce) = match (stored_csrf, stored_nonce, stored_pkce) {
        (Some(c), Some(n), Some(p)) => (c, n, p),
        _ => return HttpResponse::BadRequest().body("session missing handshake values"),
    };

    if csrf != returned_state {
        tracing::warn!("oidc state mismatch (csrf) — possible csrf attempt");
        return HttpResponse::BadRequest().body("state mismatch");
    }

    let claims = match oidc
        .exchange(&code, PkceCodeVerifier::new(pkce), Nonce::new(nonce))
        .await
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("oidc exchange failed: {e}");
            return HttpResponse::BadGateway()
                .json(serde_json::json!({"error": e.to_string()}));
        }
    };

    if let Err(e) = state.storage.upsert_user(&claims.sub, &claims.username) {
        tracing::error!("upsert_user failed: {e}");
    }

    let user = AuthUser { sub: claims.sub, username: claims.username };
    if let Err(e) = session.insert(SESSION_KEY_USER, &user) {
        tracing::error!("session user insert: {e}");
        return HttpResponse::InternalServerError().finish();
    }

    let dest = sanitize_next(stored_next.as_deref());
    HttpResponse::Found().append_header(("Location", dest)).finish()
}

/// POST /auth/logout
pub async fn logout(session: Session) -> HttpResponse {
    session.purge();
    HttpResponse::NoContent().finish()
}

/// GET /api/me
pub async fn me(user: AuthUser) -> HttpResponse {
    HttpResponse::Ok().json(user)
}

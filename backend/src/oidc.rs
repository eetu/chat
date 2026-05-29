//! OIDC authorization-code-with-PKCE flow against an external issuer
//! (kanidm in production). Provider metadata is discovered once at startup
//! and cached; the `CoreClient` is rebuilt per-request from the cached
//! metadata, since its concrete type is heavy on type-state generics and
//! re-creating it is cheap (everything is `Arc`-backed inside).
//!
//! The browser never sees access or ID tokens — they live for the duration
//! of the callback exchange only. After validation we drop them and persist
//! just `{ sub, username }` in the cookie session.

use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, Scope, TokenResponse,
};
use thiserror::Error;
use tokio::sync::RwLock;
use url::Url;

use crate::settings::OidcSettings;

#[derive(Error, Debug)]
pub enum OidcError {
    #[error("oidc init failed: {0}")]
    Init(String),
    #[error("oidc discovery failed: {0}")]
    Discover(String),
    #[error("oidc exchange failed: {0}")]
    Exchange(String),
    #[error("oidc id_token missing or invalid: {0}")]
    Token(String),
    #[error("oidc state mismatch")]
    StateMismatch,
    #[error("http client build failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("invalid url: {0}")]
    Url(#[from] url::ParseError),
}

#[derive(Clone)]
pub struct OidcContext {
    provider: CoreProviderMetadata,
    client_id: ClientId,
    client_secret: ClientSecret,
    redirect_url: RedirectUrl,
    http: reqwest::Client,
}

impl OidcContext {
    pub async fn discover(s: &OidcSettings) -> Result<Self, OidcError> {
        // Disable redirects on the OIDC HTTP client — required SSRF
        // protection per openidconnect-rs guidance, since metadata + token
        // endpoints must not follow arbitrary redirects.
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        let issuer = IssuerUrl::new(s.issuer.clone())
            .map_err(|e| OidcError::Init(format!("issuer url: {e}")))?;

        let provider = CoreProviderMetadata::discover_async(issuer, &http)
            .await
            .map_err(|e| OidcError::Discover(e.to_string()))?;

        Ok(Self {
            provider,
            client_id: ClientId::new(s.client_id.clone()),
            client_secret: ClientSecret::new(s.client_secret.clone()),
            redirect_url: RedirectUrl::new(s.redirect_url.clone())?,
            http,
        })
    }

    fn client(
        &self,
    ) -> CoreClient<
        openidconnect::EndpointSet,
        openidconnect::EndpointNotSet,
        openidconnect::EndpointNotSet,
        openidconnect::EndpointNotSet,
        openidconnect::EndpointMaybeSet,
        openidconnect::EndpointMaybeSet,
    > {
        CoreClient::from_provider_metadata(
            self.provider.clone(),
            self.client_id.clone(),
            Some(self.client_secret.clone()),
        )
        .set_redirect_uri(self.redirect_url.clone())
    }

    /// Build the authorize URL and the values that need to round-trip via
    /// the user's session cookie until the callback fires.
    pub fn authorize(&self) -> Authorize {
        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
        let (auth_url, csrf, nonce) = self
            .client()
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .add_scope(Scope::new("openid".into()))
            .add_scope(Scope::new("profile".into()))
            .set_pkce_challenge(pkce_challenge)
            .url();

        Authorize { url: auth_url, csrf, nonce, pkce_verifier }
    }

    /// Exchange the authorization code for tokens and validate the ID token.
    /// Returns the verified subject and a best-effort username.
    pub async fn exchange(
        &self,
        code: &str,
        pkce_verifier: PkceCodeVerifier,
        nonce: Nonce,
    ) -> Result<VerifiedClaims, OidcError> {
        let client = self.client();
        let token_response = client
            .exchange_code(AuthorizationCode::new(code.to_string()))
            .map_err(|e| OidcError::Exchange(e.to_string()))?
            .set_pkce_verifier(pkce_verifier)
            .request_async(&self.http)
            .await
            .map_err(|e| OidcError::Exchange(e.to_string()))?;

        let id_token = token_response
            .id_token()
            .ok_or_else(|| OidcError::Token("server did not return id_token".into()))?;

        let claims = id_token
            .claims(&client.id_token_verifier(), &nonce)
            .map_err(|e| OidcError::Token(e.to_string()))?;

        let sub = claims.subject().as_str().to_string();
        let username = claims
            .preferred_username()
            .map(|n| n.as_str().to_string())
            .or_else(|| {
                claims
                    .name()
                    .and_then(|n| n.get(None))
                    .map(|n| n.as_str().to_string())
            })
            .unwrap_or_else(|| sub.clone());

        // Token response is dropped here — we never store access/refresh
        // tokens. The session cookie holds only `{ sub, username }`.
        drop(token_response);

        Ok(VerifiedClaims { sub, username })
    }
}

/// Lazily-discovered OIDC provider with on-demand retry.
///
/// Discovery is NOT done at boot. The issuer (kanidm) may boot
/// concurrently with this process; a one-shot boot discovery that failed
/// would leave the app serving "auth not configured" forever, since the
/// IaC only restarts chat on a config change — and `/status` would still
/// return 200, so no orchestrator would restart it either.
///
/// Instead every auth-needing caller — and the `/status` poll — routes
/// through [`OidcLazy::ctx`]. The first call (or the next after the issuer
/// comes up) discovers and caches; the regular `/status` poll is therefore
/// the self-heal driver, no restart required. The write lock single-flights
/// concurrent retries so a burst of polls makes at most one discovery call.
///
/// Kept in lockstep with scribe's `OidcLazy` — both sibling apps self-heal
/// OIDC the same way; port changes across when one evolves.
pub struct OidcLazy {
    settings: Option<OidcSettings>,
    cached: RwLock<Option<OidcContext>>,
}

impl OidcLazy {
    pub fn new(settings: Option<OidcSettings>) -> Self {
        Self { settings, cached: RwLock::new(None) }
    }

    /// Whether OIDC is configured at all (env vars present). `false` =
    /// DEV_AUTH-only (or no auth) deploy; there's nothing to discover.
    pub fn is_configured(&self) -> bool {
        self.settings.is_some()
    }

    /// Whether a provider context is currently cached. Read-only — does NOT
    /// trigger discovery. For honest health reporting without side effects.
    pub async fn is_ready(&self) -> bool {
        self.cached.read().await.is_some()
    }

    /// Resolve the OIDC context, discovering (or re-discovering) on demand.
    /// Returns `None` when OIDC isn't configured, or when discovery is still
    /// failing (issuer down) — the caller should surface a retryable 503.
    pub async fn ctx(&self) -> Option<OidcContext> {
        // Fast path: already discovered.
        if let Some(ctx) = self.cached.read().await.as_ref() {
            return Some(ctx.clone());
        }
        // Nothing to retry if OIDC isn't configured.
        let settings = self.settings.as_ref()?;
        let mut guard = self.cached.write().await;
        // Double-check: another task may have discovered while we waited.
        if let Some(ctx) = guard.as_ref() {
            return Some(ctx.clone());
        }
        match OidcContext::discover(settings).await {
            Ok(ctx) => {
                tracing::info!("oidc provider discovered: {}", settings.issuer);
                *guard = Some(ctx.clone());
                Some(ctx)
            }
            Err(e) => {
                tracing::warn!(
                    "oidc discovery failed for {}: {e}; will retry on next auth/status call",
                    settings.issuer
                );
                None
            }
        }
    }
}

pub struct Authorize {
    pub url: Url,
    pub csrf: CsrfToken,
    pub nonce: Nonce,
    pub pkce_verifier: PkceCodeVerifier,
}

pub struct VerifiedClaims {
    pub sub: String,
    pub username: String,
}

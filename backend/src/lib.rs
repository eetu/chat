pub mod auth;
pub mod comfyui;
pub mod handlers;
pub mod handlers_api;
pub mod image_buffer;
pub mod image_kind;
pub mod oidc;
pub mod ollama;
pub mod personas;
pub mod rag;
pub mod ratelimit;
pub mod settings;
pub mod storage;

use std::sync::Arc;

use actix_cors::Cors;
use actix_files::Files;
use actix_session::{config::CookieContentSecurity, storage::CookieSessionStore, SessionMiddleware};
use actix_web::{cookie::Key, middleware::DefaultHeaders, web, App, HttpServer};
use tracing_actix_web::TracingLogger;

/// Content-Security-Policy applied to every response. `style-src` allows
/// inline because Emotion injects styles at runtime; the google fonts +
/// material icons origins are the only third parties the SPA reaches.
/// `connect-src 'self'` keeps fetch/SSE same-origin; the vite dev server
/// proxies upstream targets so no wildcard is needed for development.
const CSP: &str = concat!(
    "default-src 'self'; ",
    "script-src 'self'; ",
    "style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; ",
    "font-src 'self' data: https://fonts.gstatic.com; ",
    "img-src 'self' data: blob:; ",
    "connect-src 'self'; ",
    "frame-ancestors 'none'; ",
    "base-uri 'self'; ",
    "object-src 'none'; ",
    "form-action 'self'",
);

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use oidc::OidcLazy;
use ollama::ModelCapabilities;
use ratelimit::RateLimiter;
use settings::Settings;
use storage::Storage;
use tokio::sync::Semaphore;

const CAPS_TTL: Duration = Duration::from_secs(300);

pub struct CapsCache {
    inner: Mutex<HashMap<String, (Instant, ModelCapabilities)>>,
}

impl CapsCache {
    pub fn new() -> Self {
        Self { inner: Mutex::new(HashMap::new()) }
    }

    pub async fn get(&self, key: &str) -> Option<ModelCapabilities> {
        let map = self.inner.lock().await;
        map.get(key).and_then(|(at, caps)| {
            if at.elapsed() < CAPS_TTL { Some(caps.clone()) } else { None }
        })
    }

    pub async fn set(&self, key: String, caps: ModelCapabilities) {
        self.inner.lock().await.insert(key, (Instant::now(), caps));
    }
}

impl Default for CapsCache {
    fn default() -> Self {
        Self::new()
    }
}

pub struct AppState {
    pub settings: Settings,
    pub http_client: reqwest::Client,
    pub storage: Storage,
    /// Lazily-discovered OIDC provider with on-demand retry. Discovery runs
    /// on first auth use and on the `/status` poll, so a kanidm that was
    /// down at boot self-heals without a restart. See [`oidc::OidcLazy`].
    pub oidc: OidcLazy,
    pub caps_cache: CapsCache,
    pub chat_limit: RateLimiter,
    pub auth_limit: RateLimiter,
    /// Bounded permit pool guarding image generation. Holding a permit
    /// gates concurrent ComfyUI / Ollama image jobs so VRAM-bound hosts
    /// don't OOM when multiple users send at once.
    pub image_sem: Arc<Semaphore>,
    /// Short-lived cache of rendered PNGs served via
    /// `GET /api/v1/images/{uuid}.png`. Bypasses the LLM context for
    /// MCP-driven flows — see `image_buffer.rs`.
    pub image_buffer: image_buffer::ImageBuffer,
    /// Coarse cache of "does Ollama have at least one embedding-capable
    /// model?" so `/status` doesn't walk every model on every poll.
    pub embed_models_available: Mutex<Option<(Instant, bool)>>,
    /// ComfyUI prompt IDs this process has submitted and not yet finished.
    /// ComfyUI's `/interrupt` and `/free` are global — when several chat
    /// backends share one ComfyUI host (e.g. localhost + raspi), a cancel
    /// or memory-free from one instance must NOT abort another instance's
    /// in-flight render. We gate every interrupt/free on this set so we
    /// only ever touch a job we own. `std::sync::Mutex` — locked only for
    /// quick insert/remove/snapshot, never held across `.await`.
    pub active_prompts: std::sync::Mutex<HashSet<String>>,
}

pub fn create_app(
    state: Arc<AppState>,
    static_dir: &str,
) -> App<

    impl actix_web::dev::ServiceFactory<
        actix_web::dev::ServiceRequest,
        Config = (),
        Response = actix_web::dev::ServiceResponse<impl actix_web::body::MessageBody>,
        Error = actix_web::Error,
        InitError = (),
    >,
> {
    let static_dir = static_dir.to_string();
    let session_key = derive_session_key(&state.settings.session_key_hex);
    let document_limit = state.settings.max_document_bytes;

    App::new()
        .app_data(web::Data::new(state))
        // TracingLogger opens a per-request span (with a request_id field)
        // that all `tracing::*` calls in handlers inherit. Replaces the
        // built-in access logger — request lifecycle is logged inside the
        // span instead.
        .wrap(TracingLogger::default())
        .wrap(
            DefaultHeaders::new()
                .add(("Content-Security-Policy", CSP))
                .add(("X-Content-Type-Options", "nosniff"))
                .add(("Referrer-Policy", "same-origin"))
                .add(("X-Frame-Options", "DENY")),
        )
        .wrap(Cors::permissive())
        .wrap(
            SessionMiddleware::builder(CookieSessionStore::default(), session_key)
                .cookie_name("chat_session".into())
                .cookie_secure(false)
                .cookie_http_only(true)
                .cookie_content_security(CookieContentSecurity::Private)
                .build(),
        )
        .route("/status", web::get().to(handlers::status))
        .service(
            web::scope("/auth")
                .route("/login", web::get().to(auth::login))
                .route("/callback", web::get().to(auth::callback))
                .route("/logout", web::post().to(auth::logout)),
        )
        .service(
            // /api/v1 — stateless MCP-facing image endpoints. Same
            // JSON body cap as /api/chat (base64 image inflate factor
            // ~33% on the wire). The generation handlers are Bearer-auth
            // gated by the ApiKey extractor; GET /images/{id} is public
            // (a capability URL — unguessable UUID + short TTL).
            //
            // MUST be registered BEFORE the /api scope below. actix-web
            // matches scopes by exclusive prefix: the first scope whose
            // prefix matches the request path consumes it. If /api came
            // first, /api/v1/* would hit /api, find no child route, and
            // 404 without falling through to /api/v1.
            web::scope("/api/v1")
                .app_data(web::JsonConfig::default().limit(12 * 1024 * 1024))
                .route("/txt2img", web::post().to(handlers_api::txt2img))
                .route("/img2img", web::post().to(handlers_api::img2img))
                .route("/inpaint", web::post().to(handlers_api::inpaint))
                .route("/images/{id}", web::get().to(handlers_api::get_image)),
        )
        .service(
            web::scope("/api")
                .route("/me", web::get().to(auth::me))
                .route("/me", web::delete().to(auth::delete_me))
                .route("/models", web::get().to(handlers::list_models))
                .route("/models/caps", web::get().to(handlers::model_caps))
                .route("/personas", web::get().to(handlers::list_personas))
                .route("/voices", web::get().to(handlers::list_voices))
                .route("/search", web::get().to(handlers::search))
                .service(
                    // Document bodies are larger than the default JSON
                    // cap. Match the per-upload byte limit + a small
                    // header overhead so the JSON layer doesn't reject
                    // payloads the handler is willing to accept.
                    web::resource("/documents")
                        .app_data(
                            // base64-encoded binaries inflate ~33% on
                            // the wire, so the JSON cap allows for the
                            // raw byte limit plus that overhead plus a
                            // small header margin.
                            web::JsonConfig::default().limit(
                                document_limit * 4 / 3 + 64 * 1024,
                            ),
                        )
                        .route(web::get().to(handlers::list_documents))
                        .route(web::post().to(handlers::upload_document)),
                )
                .route(
                    "/documents/{id}",
                    web::delete().to(handlers::delete_document),
                )
                .route(
                    "/embedding-models",
                    web::get().to(handlers::list_embedding_models),
                )
                .service(
                    web::scope("/conversations")
                        .route("", web::get().to(handlers::list_conversations))
                        .route("", web::post().to(handlers::create_conversation))
                        .route("/{id}", web::delete().to(handlers::delete_conversation))
                        .route("/{id}", web::patch().to(handlers::update_conversation))
                        .route("/{id}/messages", web::get().to(handlers::get_messages))
                        .route(
                            "/{conv_id}/messages/{msg_id}",
                            web::delete().to(handlers::delete_message_from),
                        )
                        .route(
                            "/{conv_id}/messages/{msg_id}/cancel",
                            web::post().to(handlers::cancel_pending_message),
                        )
                        .route(
                            "/{conv_id}/messages/{msg_id}/image/{idx}",
                            web::get().to(handlers::get_message_image),
                        )
                        .route(
                            "/{conv_id}/messages/{msg_id}/mask",
                            web::get().to(handlers::get_message_mask),
                        ),
                )
                .service(
                    web::resource("/chat")
                        .app_data(web::JsonConfig::default().limit(3 * 1024 * 1024))
                        .route(web::post().to(handlers::chat)),
                )
                .service(
                    // Audio uploads can exceed the default 256kB body cap
                    // — a one-minute opus clip lands around 200–600 kB,
                    // longer clips need more headroom. 16 MB matches the
                    // browser's typical MediaRecorder budget for a few
                    // minutes of dictation.
                    web::resource("/transcribe")
                        .app_data(web::PayloadConfig::new(16 * 1024 * 1024))
                        .route(web::post().to(handlers::transcribe)),
                )
                .route("/tts", web::post().to(handlers::tts)),
        )
        .service({
            let index_path = format!("{static_dir}/index.html");
            Files::new("/", &static_dir)
                .index_file("index.html")
                .default_handler(actix_web::dev::fn_service(
                    move |req: actix_web::dev::ServiceRequest| {
                        let index_path = index_path.clone();
                        async move {
                            let (req, _) = req.into_parts();
                            let file = actix_files::NamedFile::open_async(&index_path).await?;
                            let res = file.into_response(&req);
                            Ok(actix_web::dev::ServiceResponse::new(req, res))
                        }
                    },
                ))
        })
}

fn derive_session_key(hex_str: &str) -> Key {
    match hex::decode(hex_str) {
        Ok(bytes) if bytes.len() >= 64 => Key::from(&bytes),
        _ => {
            tracing::warn!("SESSION_KEY missing or too short; using ephemeral key");
            Key::generate()
        }
    }
}

pub async fn run_server() -> std::io::Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let settings = Settings::from_env();
    let port = settings.port;
    let static_dir = settings.static_dir.clone();
    let db_path = settings.db_path.clone();

    let storage = Storage::open(std::path::Path::new(&db_path))
        .expect("failed to open SQLite database");

    // OIDC discovery is lazy (not at boot): kanidm may boot concurrently
    // with us, and a failed one-shot boot discovery would wedge auth until a
    // manual restart. Instead the first auth call and the `/status` poll
    // drive discovery + retry, so it self-heals once kanidm is up.
    let oidc = OidcLazy::new(settings.oidc.clone());

    let chat_rate = settings.chat_rate_per_min;
    let auth_rate = settings.auth_rate_per_min;
    let image_concurrency = settings.image_gen_concurrency;
    if settings.mcp_api_key.is_none() {
        tracing::warn!(
            "CHAT_MCP_API_KEY is unset — /api/v1/* endpoints will accept unauthenticated requests"
        );
    }
    let state = Arc::new(AppState {
        settings,
        http_client: reqwest::Client::new(),
        storage,
        oidc,
        caps_cache: CapsCache::new(),
        chat_limit: RateLimiter::per_minute(chat_rate),
        auth_limit: RateLimiter::per_minute(auth_rate),
        image_sem: Arc::new(Semaphore::new(image_concurrency)),
        image_buffer: image_buffer::ImageBuffer::new(),
        embed_models_available: Mutex::new(None),
        active_prompts: std::sync::Mutex::new(HashSet::new()),
    });

    // Image generation can take >1 minute. Anything older than 5 still
    // marked pending must have been left behind by a backend restart, so
    // mark it errored so the UI can stop spinning.
    match state.storage.fail_stale_pending(300) {
        Ok(0) => {}
        Ok(n) => tracing::info!("startup: marked {n} stale pending message(s) as errored"),
        Err(e) => tracing::error!("startup pending sweep failed: {e}"),
    }

    storage::start_ttl_loop(state.clone());
    image_buffer::start_sweep_loop(state.clone());

    tracing::info!("starting chat server on port {port}");

    HttpServer::new(move || create_app(state.clone(), &static_dir))
        .bind(("0.0.0.0", port))?
        .run()
        .await
}

pub fn create_test_state() -> Arc<AppState> {
    let storage = Storage::open(std::path::Path::new(":memory:"))
        .expect("in-memory db");
    Arc::new(AppState {
        settings: Settings::test_defaults(),
        http_client: reqwest::Client::new(),
        storage,
        oidc: OidcLazy::new(None),
        caps_cache: CapsCache::new(),
        chat_limit: RateLimiter::per_minute(0),
        auth_limit: RateLimiter::per_minute(0),
        image_sem: Arc::new(Semaphore::new(1)),
        image_buffer: image_buffer::ImageBuffer::new(),
        embed_models_available: Mutex::new(None),
        active_prompts: std::sync::Mutex::new(HashSet::new()),
    })
}

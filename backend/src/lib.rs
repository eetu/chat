pub mod auth;
pub mod handlers;
pub mod oidc;
pub mod ollama;
pub mod settings;
pub mod storage;

use std::sync::Arc;

use actix_cors::Cors;
use actix_files::Files;
use actix_session::{config::CookieContentSecurity, storage::CookieSessionStore, SessionMiddleware};
use actix_web::{cookie::Key, middleware, web, App, HttpServer};

use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use oidc::OidcContext;
use ollama::ModelCapabilities;
use settings::Settings;
use storage::Storage;

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
    pub oidc: Option<OidcContext>,
    pub caps_cache: CapsCache,
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

    App::new()
        .app_data(web::Data::new(state))
        .wrap(middleware::Logger::default())
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
            web::scope("/api")
                .route("/me", web::get().to(auth::me))
                .route("/models", web::get().to(handlers::list_models))
                .route("/models/caps", web::get().to(handlers::model_caps))
                .service(
                    web::scope("/conversations")
                        .route("", web::get().to(handlers::list_conversations))
                        .route("", web::post().to(handlers::create_conversation))
                        .route("/{id}", web::delete().to(handlers::delete_conversation))
                        .route("/{id}/messages", web::get().to(handlers::get_messages)),
                )
                .service(
                    web::resource("/chat")
                        .app_data(web::JsonConfig::default().limit(3 * 1024 * 1024))
                        .route(web::post().to(handlers::chat)),
                ),
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

    let oidc = match &settings.oidc {
        Some(s) => match OidcContext::discover(s).await {
            Ok(ctx) => {
                tracing::info!("oidc provider discovered: {}", s.issuer);
                Some(ctx)
            }
            Err(e) => {
                tracing::error!(
                    "oidc discovery failed for {}: {e}; falling back to dev_auth = {}",
                    s.issuer,
                    settings.dev_auth
                );
                None
            }
        },
        None => None,
    };

    let state = Arc::new(AppState {
        settings,
        http_client: reqwest::Client::new(),
        storage,
        oidc,
        caps_cache: CapsCache::new(),
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
        oidc: None,
        caps_cache: CapsCache::new(),
    })
}

use std::env;

#[derive(Debug, Clone)]
pub struct Settings {
    pub port: u16,
    pub static_dir: String,
    pub db_path: String,
    pub ollama_url: String,
    pub ollama_model_lock: Option<String>,
    /// Chat-capable model used to expand a user's image-gen prompt into a
    /// detailed prompt before it's sent to the image generator. Disabled
    /// when unset.
    pub prompt_refiner_model: Option<String>,
    /// ComfyUI base URL — the sole backend for all image generation
    /// (Z-Image txt2img, Flux Kontext img2img, Flux Fill inpaint). When
    /// unset, image mode is unavailable: the handler fails the request
    /// with an SSE error rather than falling back anywhere.
    pub comfyui_url: Option<String>,
    /// whisper.cpp HTTP server base URL (`/inference` endpoint). Drives
    /// the voice-input mic in the composer. When unset, the UI hides
    /// the affordance.
    pub whisper_url: Option<String>,
    /// piper-tts HTTP server base URL (`POST /`). Drives the "read
    /// aloud" affordance on assistant messages.
    pub piper_url: Option<String>,
    /// Ollama model used to embed uploaded RAG documents and the
    /// user's prompt before retrieval. RAG is disabled when unset.
    pub embedding_model: Option<String>,
    /// How many top-scoring chunks to inject as system context per
    /// chat turn. Cosine-ranked.
    pub rag_top_k: usize,
    /// Max accepted document size in bytes for the RAG upload path.
    /// Text content only today — when PDF / docx extraction lands the
    /// cap will apply post-extraction so a fat image-heavy file isn't
    /// rejected at the door.
    pub max_document_bytes: usize,
    pub chat_ttl_days: u32,
    pub session_key_hex: String,
    pub oidc: Option<OidcSettings>,
    pub dev_auth: bool,
    /// Per-user token-bucket rate for `/api/chat`. 0 disables the limit.
    pub chat_rate_per_min: u32,
    /// Per-IP token-bucket rate for `/auth/login` + `/auth/callback`. 0
    /// disables the limit.
    pub auth_rate_per_min: u32,
    /// Max concurrent image-generation jobs. VRAM-bound on Apple Silicon;
    /// default 1.
    pub image_gen_concurrency: usize,
    /// How long a generated image stays in the in-memory buffer
    /// served by `GET /api/v1/images/{uuid}.png`. Tuned for "agent
    /// renders, user reviews, user saves" workflows; 30 min is long
    /// enough for a slow review without pinning megabytes forever.
    pub image_buffer_ttl_secs: u64,
    /// Cap on how many images live in the buffer at once. When full,
    /// the oldest entry is evicted on insert. Each entry is roughly
    /// 0.5–2 MB, so default 64 caps memory around 128 MB worst case.
    pub image_buffer_limit: usize,
    /// Shared secret for the MCP bridge's `/api/v1/*` endpoints. When
    /// set, every request must carry a matching `Authorization:
    /// Bearer ...`. When unset the routes accept every request —
    /// intended for trusted-LAN deployments behind another auth layer
    /// (Wireguard, Tailscale, mTLS); `run_server` logs a startup
    /// warning so the open state shows up in logs.
    pub mcp_api_key: Option<String>,
    /// Master switch for web search. Off by default: searching runs
    /// in-process and reaches the internet from this host's own IP, so
    /// it's a deliberate choice rather than something a deploy inherits.
    /// When false the composer hides the affordance and the chat handler
    /// ignores the flag even if a client sends it.
    pub web_search_enabled: bool,
    /// Allow the HTML-scraping backends (Google, Bing, DuckDuckGo
    /// SERPs). Off by default — scraping those breaks their terms of
    /// service and collects CAPTCHAs; the RSS and public-API backends
    /// cover general queries without either problem.
    pub web_search_allow_scrapers: bool,
    /// Search backends to skip, by daedra's own names. Every backend is
    /// awaited before the aggregate returns, so one that hangs is paid
    /// for by every search — which is what the default is about; see
    /// `websearch::DEFAULT_EXCLUDED`.
    pub web_search_exclude_backends: Vec<String>,
    /// How many search results to inject as context per turn.
    pub web_search_max_results: usize,
    /// How many of those results get their page fetched and reduced to
    /// article text. The rest contribute their search snippet only.
    /// Raising it costs little wall-clock — fetches run concurrently
    /// under a fixed deadline — but does raise peak memory.
    pub web_search_fetch_count: usize,
    /// Hard cap on a single fetched page's body. Enforced while
    /// streaming, so a response that omits or lies about
    /// `content-length` can't blow the memory ceiling.
    pub web_search_max_page_bytes: usize,
    /// Chat model used to rewrite the user's turn into a standalone
    /// search query, so follow-ups ("what about the second one?")
    /// still search sensibly. When unset the raw turn is the query.
    pub web_search_query_model: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OidcSettings {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_url: String,
}

impl Settings {
    pub fn from_env() -> Self {
        let port = env::var("PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(8080);
        let static_dir = env::var("STATIC_DIR").unwrap_or_else(|_| "../frontend/dist".into());
        let db_path = env::var("CHAT_DB_PATH").unwrap_or_else(|_| "chat.db".into());
        let ollama_url = env::var("OLLAMA_URL").unwrap_or_else(|_| "http://localhost:11434".into());
        let ollama_model_lock = env::var("OLLAMA_MODEL").ok().filter(|s| !s.is_empty());
        let prompt_refiner_model = env::var("PROMPT_REFINER_MODEL")
            .ok()
            .filter(|s| !s.is_empty());
        let comfyui_url = env::var("COMFYUI_URL").ok().filter(|s| !s.is_empty());
        let whisper_url = env::var("WHISPER_URL").ok().filter(|s| !s.is_empty());
        let piper_url = env::var("PIPER_URL").ok().filter(|s| !s.is_empty());
        let embedding_model = env::var("EMBEDDING_MODEL").ok().filter(|s| !s.is_empty());
        let rag_top_k = env::var("RAG_TOP_K")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n: &usize| *n > 0)
            .unwrap_or(4);
        let max_document_bytes = env::var("MAX_DOCUMENT_MB")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(10)
            * 1024
            * 1024;
        let chat_ttl_days = env::var("CHAT_TTL_DAYS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30);
        let session_key_hex = env::var("SESSION_KEY").unwrap_or_else(|_| "0".repeat(128));
        let dev_auth = env::var("DEV_AUTH")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false);
        let chat_rate_per_min = env::var("CHAT_RATE_PER_MIN")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60);
        let auth_rate_per_min = env::var("AUTH_RATE_PER_MIN")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20);
        let image_gen_concurrency = env::var("IMAGE_GEN_CONCURRENCY")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n: &usize| *n > 0)
            .unwrap_or(1);
        let mcp_api_key = env::var("CHAT_MCP_API_KEY").ok().filter(|s| !s.is_empty());
        let image_buffer_ttl_secs = env::var("CHAT_IMAGE_BUFFER_TTL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n: &u64| *n > 0)
            .unwrap_or(1800);
        let image_buffer_limit = env::var("CHAT_IMAGE_BUFFER_LIMIT")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n: &usize| *n > 0)
            .unwrap_or(64);
        let web_search_enabled = env::var("WEB_SEARCH_ENABLED")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false);
        let web_search_allow_scrapers = env::var("WEB_SEARCH_ALLOW_SCRAPERS")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false);
        // Set to an empty string to exclude nothing.
        let web_search_exclude_backends = match env::var("WEB_SEARCH_EXCLUDE_BACKENDS") {
            Ok(raw) => raw
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            Err(_) => crate::websearch::DEFAULT_EXCLUDED
                .iter()
                .map(|s| s.to_string())
                .collect(),
        };
        // The ceiling protects the model's context, not the provider:
        // each result contributes up to `RESULT_CHAR_BUDGET` characters,
        // so 25 is already ~30k characters of system message before the
        // conversation gets a word in.
        let web_search_max_results = env::var("WEB_SEARCH_MAX_RESULTS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n: &usize| *n > 0)
            .unwrap_or(8)
            .min(25);
        let web_search_fetch_count = env::var("WEB_SEARCH_FETCH_COUNT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3)
            .min(web_search_max_results);
        // 512 KB of HTML is a generous article. The DOM built from it
        // costs several times that, and the container's MemoryMax is
        // the real constraint, so the default stays modest.
        let web_search_max_page_bytes = env::var("WEB_SEARCH_MAX_PAGE_KB")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(512)
            * 1024;
        let web_search_query_model = env::var("WEB_SEARCH_QUERY_MODEL")
            .ok()
            .filter(|s| !s.is_empty());

        let oidc = match (
            env::var("OIDC_ISSUER"),
            env::var("OIDC_CLIENT_ID"),
            env::var("OIDC_CLIENT_SECRET"),
            env::var("OIDC_REDIRECT_URL"),
        ) {
            (Ok(issuer), Ok(client_id), Ok(client_secret), Ok(redirect_url))
                if !issuer.is_empty() =>
            {
                Some(OidcSettings {
                    issuer,
                    client_id,
                    client_secret,
                    redirect_url,
                })
            }
            _ => None,
        };

        Self {
            port,
            static_dir,
            db_path,
            ollama_url,
            ollama_model_lock,
            prompt_refiner_model,
            comfyui_url,
            whisper_url,
            piper_url,
            embedding_model,
            rag_top_k,
            max_document_bytes,
            chat_ttl_days,
            session_key_hex,
            oidc,
            dev_auth,
            chat_rate_per_min,
            auth_rate_per_min,
            image_gen_concurrency,
            mcp_api_key,
            image_buffer_ttl_secs,
            image_buffer_limit,
            web_search_enabled,
            web_search_allow_scrapers,
            web_search_exclude_backends,
            web_search_max_results,
            web_search_fetch_count,
            web_search_max_page_bytes,
            web_search_query_model,
        }
    }

    pub fn test_defaults() -> Self {
        Self {
            port: 0,
            static_dir: ".".into(),
            db_path: ":memory:".into(),
            ollama_url: "http://localhost:11434".into(),
            ollama_model_lock: None,
            prompt_refiner_model: None,
            comfyui_url: None,
            whisper_url: None,
            piper_url: None,
            embedding_model: None,
            rag_top_k: 4,
            max_document_bytes: 10 * 1024 * 1024,
            chat_ttl_days: 30,
            session_key_hex: "0".repeat(128),
            oidc: None,
            dev_auth: true,
            chat_rate_per_min: 0,
            auth_rate_per_min: 0,
            image_gen_concurrency: 1,
            mcp_api_key: None,
            image_buffer_ttl_secs: 1800,
            image_buffer_limit: 64,
            web_search_enabled: false,
            web_search_allow_scrapers: false,
            web_search_exclude_backends: Vec::new(),
            web_search_max_results: 5,
            web_search_fetch_count: 3,
            web_search_max_page_bytes: 512 * 1024,
            web_search_query_model: None,
        }
    }
}

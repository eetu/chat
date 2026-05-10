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
    /// ComfyUI base URL used for img2img (Flux Kontext). When unset, image
    /// mode falls back to Ollama's `/v1/images/generations` even if the
    /// user attached an input image.
    pub comfyui_url: Option<String>,
    pub chat_ttl_days: u32,
    pub session_key_hex: String,
    pub oidc: Option<OidcSettings>,
    pub dev_auth: bool,
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
        let port = env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8080);
        let static_dir = env::var("STATIC_DIR").unwrap_or_else(|_| "../frontend/dist".into());
        let db_path = env::var("CHAT_DB_PATH").unwrap_or_else(|_| "chat.db".into());
        let ollama_url =
            env::var("OLLAMA_URL").unwrap_or_else(|_| "http://localhost:11434".into());
        let ollama_model_lock = env::var("OLLAMA_MODEL").ok().filter(|s| !s.is_empty());
        let prompt_refiner_model =
            env::var("PROMPT_REFINER_MODEL").ok().filter(|s| !s.is_empty());
        let comfyui_url = env::var("COMFYUI_URL").ok().filter(|s| !s.is_empty());
        let chat_ttl_days = env::var("CHAT_TTL_DAYS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30);
        let session_key_hex = env::var("SESSION_KEY")
            .unwrap_or_else(|_| "0".repeat(128));
        let dev_auth = env::var("DEV_AUTH").map(|v| v == "1" || v == "true").unwrap_or(false);

        let oidc = match (
            env::var("OIDC_ISSUER"),
            env::var("OIDC_CLIENT_ID"),
            env::var("OIDC_CLIENT_SECRET"),
            env::var("OIDC_REDIRECT_URL"),
        ) {
            (Ok(issuer), Ok(client_id), Ok(client_secret), Ok(redirect_url))
                if !issuer.is_empty() =>
            {
                Some(OidcSettings { issuer, client_id, client_secret, redirect_url })
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
            chat_ttl_days,
            session_key_hex,
            oidc,
            dev_auth,
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
            chat_ttl_days: 30,
            session_key_hex: "0".repeat(128),
            oidc: None,
            dev_auth: true,
        }
    }
}

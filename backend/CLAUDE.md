# Backend

Rust crate `chat-backend`. Single binary, self-contained — bundles SQLite via
`rusqlite/bundled`.

## Modules

| File | Role |
|---|---|
| `main.rs` | Entrypoint — calls `chat_backend::run_server()` |
| `lib.rs` | App wiring: `AppState`, `create_app`, `run_server`, session middleware, route mounting, SPA fallback |
| `settings.rs` | Env parsing — `Settings::from_env()` |
| `storage.rs` | SQLite schema + per-user CRUD + TTL sweep loop |
| `ollama.rs` | Upstream client — `list_models`, NDJSON `stream_chat`, SSE helpers |
| `handlers.rs` | HTTP handlers for `/api/*` |
| `auth.rs` | `AuthUser` extractor, `/auth/login`, `/auth/callback`, `/auth/logout`, `/api/me` |
| `oidc.rs` | `OidcContext` — provider discovery + authorize/exchange against any OIDC issuer (kanidm in production) |

## Validation / iteration

- `cargo check` — fast feedback (use `bacon` for watch loop)
- `cargo run` — boots on `:8080` (or `PORT`)
- `cargo test` — currently only the schema bootstrap; expand before adding
  non-trivial storage logic. `wiremock` is already in dev-deps for upstream
  Ollama mocking.

## Patterns

- **`AppState` is `Arc<AppState>`**, registered as `web::Data<Arc<AppState>>`.
  Inside handlers extract a fresh `Arc` with `state.get_ref().clone()` before
  spawning a task — `state.into_inner()` returns `Arc<Arc<AppState>>` and is
  the wrong type.
- **SSE.** Use `actix_web_lab::sse`. Return type
  `Result<sse::Sse<ReceiverStream<Result<sse::Event, Infallible>>>, actix_web::Error>`.
  Spawn a task that pushes events to an `mpsc::channel`, receiver wraps into
  the SSE response. See `handlers::chat`.
- **Auth gate.** Any handler that takes `AuthUser` as an extractor returns
  401 automatically when there's no session. Don't read the cookie manually.
- **Storage scoping.** Every storage method that touches conversations or
  messages takes `user_sub: &str` and returns `Forbidden` for cross-user
  access. Never bypass this — never use a method that doesn't take user_sub.
- **Auto-rename.** After persisting the assistant's response on a chat that
  now has exactly two messages (first user + first assistant), `handlers::chat`
  spawns a separate task that calls `ollama::summarize_title` (non-streaming
  /api/chat with a "give a 3-6 word lowercase title" system prompt). On
  success the conversation row's `title` is overwritten via
  `storage::set_conversation_title`. Failures are logged and ignored — the
  earlier eager truncate (first 60 chars of the user message) stays in
  place. Do not run auto-rename on subsequent turns.
- **Vision / image attachments.** `messages.attachments` is a nullable
  TEXT column holding a JSON array of base64 strings (no `data:` prefix).
  `Storage::append_message` takes an `images: &[String]` slice; pass
  `&[]` for assistant turns. `ChatBody.images` flows in via the JSON
  body and is forwarded straight to Ollama's `/api/chat` per-message
  `images` field. When the resolved model isn't vision-capable, Ollama
  ignores the field — no client-side filtering needed. `GET
  /api/models/caps?model=…` proxies `/api/show` and normalises the
  response into `{ vision, tools, capabilities, families }`, with a 5
  min in-memory cache (`CapsCache`).

## Ollama upstream

- `POST /api/chat` with `stream: true` returns NDJSON. `ollama::stream_chat`
  parses line-by-line and forwards content deltas via a callback. Final chunk
  has `done: true`.
- `GET /api/tags` lists installed models. Cached at the client level; no
  caching layer in this app yet.
- If `OLLAMA_MODEL` is set, all chats use that model; the client `model` field
  is ignored (`ollama::resolve_model`).

## OIDC (kanidm)

Wired in `oidc.rs` and `auth.rs`. Behavior:

- At startup, if all four `OIDC_*` env vars are set, the server calls
  `CoreProviderMetadata::discover_async` against the issuer and caches the
  resulting `OidcContext` on `AppState`. Discovery failure is logged and
  the app falls back to whatever `DEV_AUTH` says.
- `/auth/login`:
  1. If `state.oidc.is_some()`: build PKCE challenge + verifier, CSRF,
     and nonce via `OidcContext::authorize`. Store the verifier, CSRF, and
     nonce in the cookie session under `oidc.pkce` / `oidc.csrf` /
     `oidc.nonce`. Redirect 302 to the provider's authorize URL.
  2. Else if `DEV_AUTH=1`: mint a session for `?username=foo`.
  3. Else: 503.
- `/auth/callback`:
  1. Read `code` + `state` from query; reject if either missing or if the
     provider returned an `error` param.
  2. Read `oidc.csrf` / `oidc.nonce` / `oidc.pkce` from the session, then
     **immediately remove them** so they can't be replayed.
  3. Compare returned `state` to `oidc.csrf` — mismatch is a 400 (CSRF).
  4. Call `OidcContext::exchange`, which posts to the token endpoint with
     the PKCE verifier, parses the ID token, and validates issuer +
     audience + nonce + signature via `client.id_token_verifier()`.
  5. `storage.upsert_user(sub, preferred_username || name || sub)` and
     write `AuthUser { sub, username }` into the session. Redirect 302 to
     `/`. The token response is dropped — only `{ sub, username }` ever
     gets persisted client-side, in the encrypted cookie.

Scopes requested: `openid profile`. Add `email` only when the UI needs to
show or use email.

The OIDC HTTP client has redirects disabled (`redirect::Policy::none`),
per openidconnect-rs SSRF guidance — token / metadata endpoints must not
follow arbitrary redirects.

Keep the dev path working alongside — it's invaluable for local
development and CI. Set `DEV_AUTH=1` and leave `OIDC_*` unset.

## Don'ts

- Don't add admin or impersonation routes.
- Don't commit `chat.db*`. The `.gitignore` already excludes them.
- Don't log message content. The `tracing` calls in `ollama.rs` and
  `handlers.rs` already avoid this; keep it that way.

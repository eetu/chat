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
| `websearch.rs` | In-process web search (daedra) + context builder for the chat path |
| `safefetch.rs` | The only outbound page fetcher — SSRF guard + readability extraction |

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
- **Image generation.** All image gen runs on ComfyUI (`COMFYUI_URL`) —
  there is no Ollama image path. It is a server-level capability,
  independent of the selected chat model. When the client sends
  `mode: "image"` the chat handler inserts a `status='pending'`
  assistant row; if no ComfyUI host is configured it immediately fails
  that row and emits an SSE `error` (no fallback). Otherwise it
  optionally calls `ollama::refine_image_prompt` (skipped if
  `PROMPT_REFINER_MODEL` is unset) to expand the prompt, evicts the chat
  model, then dispatches in `comfyui.rs`:
  - no attachment → `generate_txt2img` (Z-Image Turbo, 8 steps, cfg 2.0);
  - reference image(s) → `generate_kontext` (Flux Kontext img2img);
  - base + mask → `generate_inpaint` (Flux Fill).
  The refiner's negative prompt feeds the real-CFG paths (txt2img +
  inpaint); Kontext runs cfg=1 and ignores it. On success the row is
  updated to `status='done'` with the b64 PNG in `attachments` and the
  refined prompt as `content` (rendered as a caption). On failure the
  row is set to `status='error'`. Stale pending rows (>5 min) are swept
  to `error` at startup. `comfyui::free_memory` is called after every
  job (success/error/cancel) so the diffusion stack doesn't sit resident.
- **Web search.** Deterministic pre-retrieval, not function calling —
  nothing sends a `tools` array to Ollama. `ChatBody.web_search` is a
  modifier on the text path only; `websearch::is_configured` gates it,
  so the flag is ignored when `WEB_SEARCH_ENABLED` is off. A URL in the
  user's turn routes to `websearch::fetch`, anything else to
  `websearch::search` followed by `websearch::hydrate` (which swaps the
  top few snippets for real article text); with `WEB_SEARCH_QUERY_MODEL`
  set, `websearch::refine_query` rewrites the turn into a standalone
  query first. Results join RAG's chunks in one `system` message and one
  `context` SSE event — never emit a second `context` event or a second
  `messages.insert(0, …)`.
- **The search provider is `daedra`, in-process.** No key, no upstream
  service. Use `SearchProvider::auto()` via the module's `PROVIDER`
  `OnceLock`, never `daedra::tools::search::perform_search` — the latter
  is the plain DuckDuckGo path, ignores `exclude_backends`, and walks
  straight into an anti-bot page. The provider is cached because it owns
  the per-backend circuit breakers; rebuilding it per turn would reset
  them and re-hammer a backend that just served a CAPTCHA.
- **A stalled backend is paid for by every search, and you cannot
  auto-detect which one it was.** The chain awaits all of them and
  returns a single merged list, so there is no per-backend status in the
  return value. Attributing health from `data[].metadata.source` was
  tried and is *wrong*: the merge truncates to `num_results`, so a
  backend that answered well can be absent purely by ranking — measured,
  bing-rss returned 10 results and did not appear, and a health tracker
  built on that promptly rested one of the best backends. daedra's own
  breaker (3 failures, 30 s) does not help either: chat searches are
  minutes apart, so the cooldown has always expired and the dead backend
  is re-probed every turn. The working answer is the explicit
  `WEB_SEARCH_EXCLUDE_BACKENDS` list plus `SEARCH_BUDGET`. To find the
  culprit, run with `RUST_LOG=daedra=debug` and compare each backend's
  completion timestamp. Real auto-detection would mean calling each
  backend separately and owning the merge.
- **All fetching goes through `safefetch`.** It is the only module that
  dereferences a URL, because those URLs come from chat input and the
  backend sits on a LAN full of admin surfaces. It vets scheme, port and
  credentials, rejects every non-public resolved address, pins the
  connection to the address that passed (DNS rebinding), re-validates
  each of at most 3 redirect hops, and streams the body under a byte
  cap. Don't route a fetch around it, and don't reach for `daedra`'s own
  fetcher — the guard is the point. Page-size caps matter on the Pi:
  the container has a `MemoryMax` and a DOM costs several times its
  page, which is what bounds `FETCH_CONCURRENCY`.
- **Retrieval latency is the user staring at an empty bubble.** Nothing
  streams until the chat handler's retrieval block returns, so
  `hydrate`'s worst case *is* the perceived delay. A dead host costs the
  full connect-plus-read budget; fetching serially made the delay the
  sum of them (four measured at 64 s). It is bounded twice now —
  `FETCH_CONCURRENCY` in flight, and the whole phase cut off at
  `HYDRATE_BUDGET` via `take_until`, which keeps the pages that already
  landed. Don't swap that for `tokio::time::timeout` around the stream:
  that discards partial results. Keep both bounds if you touch it.
- **Retrieved sources.** `messages.sources` is a nullable TEXT column
  holding a JSON array of `{kind, name, url?, score?}`, written by
  `Storage::set_message_sources` after the assistant row lands (it
  returns the new id). `kind` is `doc` for RAG, `web` for search
  results. `message_to_dto` parses it and drops it silently if it no
  longer deserialises. Both retrieval kinds share the column — don't
  add a web-only one.
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

# royale with chat

<p>
  <img src="documentation/screenshots/main.png" alt="desktop" />
</p>

Minimal, self-hosted chat UI for an Ollama endpoint on the LAN. Sibling project
to [halo](../hcc) — same design tokens, different glyph, same warm orange dot.

- Streaming token-by-token responses (SSE) with end-of-turn token / TPS stats
- Per-user conversation history in SQLite (kanidm OIDC; dev fallback for local work)
- Configurable retention (default 30 days)
- Image generation entirely via ComfyUI — text-to-image (Z-Image Turbo),
  reference-guided img2img (Flux Kontext, multi-image), and masked inpaint
  (Flux Fill, in-browser mask editor), all with live progress + preview over
  WebSocket. One server-level capability, independent of the selected chat model
- Voice input via whisper.cpp with browser-side Web Audio VAD — utterances
  segment on silence and transcribe one at a time so dictation stays stable
- Read-aloud via piper-tts (opus over chunked ogg) streamed straight into a
  MediaSource for instant playback; falls back to WAV when the browser
  can't decode opus (looking at you, Safari)
- RAG over uploaded text / markdown / PDF documents — per-document embedding
  model (picked from whatever Ollama installs surface the `embedding`
  capability), cosine retrieval at chat time, source chip under each
  assistant reply naming the docs it drew from
- Full-text search across every message with FTS5; ⌘K sidebar palette
  highlights matches and scrolls the chat to the hit
- Optional prompt refiner with persona-flavoured rewrites for image generation
- Manual rename + edit-and-resend + regenerate-from-user + remix-this-image flows
- Code blocks render with language label, streaming-fence balancing, copy button
- Mermaid diagrams render lazily from ```mermaid fences
- Light / dark / system theme override + STT language picker in settings
- Per-user rate limits, image-gen semaphore, CSP + magic-byte MIME sniffing on
  uploaded images
- No navbars, no chrome — just the conversation

## Stack

| Layer | Tech |
|---|---|
| Backend | Rust, actix-web 4, actix-web-lab SSE, reqwest stream → Ollama NDJSON, ComfyUI WS for progress, tokio semaphore for GPU coordination, rusqlite (bundled with FTS5), `pdf-extract` for PDF text, actix-session cookie store, tracing-actix-web spans |
| Frontend | Vite + React 19, Emotion theme (copied from halo), TanStack Router (file-based), SWR, MediaRecorder + Web Audio analyser for VAD, MediaSource for streaming TTS, mermaid.js (lazy), highlight.js for code |
| Auth | OIDC (kanidm) — code + PKCE flow with cookie session. Dev fallback via `DEV_AUTH=1`. |
| Persistence | SQLite (`chat.db` by default), schema bootstrapped on startup; FTS5 mirror + triggers rebuilt on drift |

## Layout

```
chat/
├── backend/         actix-web server, SSE proxy, SQLite, auth, RAG
├── frontend/        React SPA (Vite)
├── .claude/skills/  chat-design — design system + brand guidelines
└── README.md
```

## Quick start

Two terminals.

**Backend**

```sh
cd backend
cp .env.example .env             # edit OLLAMA_URL etc.
cargo run
```

**Frontend**

```sh
cd frontend
yarn install
yarn dev
```

Then open http://localhost:5173. Vite proxies `/api`, `/auth`, `/status` to
the backend on `:8080`.

**Git hooks** (one-time, per clone):

```sh
./install-hooks.sh
```

This points `core.hooksPath` at `.githooks/`. The pre-commit hook runs
`yarn lint` + `yarn format` for staged frontend changes and `cargo clippy
-- -D warnings` for staged backend changes.

In dev mode (`DEV_AUTH=1`), hitting "sign in" calls `/auth/login` which writes
a cookie session for user `dev`. Override with `?username=foo` for multiple
users on one box.

## Companion services

These features expect dedicated LAN endpoints, deployed via the sibling
[`mini`](../mini) IaC repo. All three sit behind Caddy on the Mac mini:

| Service | Endpoint | Provides |
|---|---|---|
| whisper.cpp HTTP server | `WHISPER_URL` (`/inference`) | Voice input transcription |
| piper-tts | `PIPER_URL` (`POST /` returns chunked ogg/opus, `GET /voices` lists) | Read-aloud |
| ComfyUI | `COMFYUI_URL` | All image generation: Z-Image Turbo txt2img, Flux Kontext img2img, Flux Fill inpaint — with WS progress |

The chat features auto-detect availability via the `/status` payload — set
each env var only when its endpoint is reachable; the matching UI hides
otherwise.

## Configuration

See `backend/.env.example` for the full list. Key values:

| Var | Default | Purpose |
|---|---|---|
| `OLLAMA_URL` | `http://localhost:11434` | Upstream Ollama HTTP endpoint |
| `OLLAMA_MODEL` | unset | If set, locks all chats to this model and ignores client selection |
| `COMFYUI_URL` | unset | Enables all image generation (Z-Image txt2img, Flux Kontext img2img, Flux Fill inpaint). Image mode is unavailable when unset — no fallback. |
| `PROMPT_REFINER_MODEL` | unset | Chat model that rewrites image-gen prompts before they hit the image runner |
| `WHISPER_URL` | unset | whisper.cpp HTTP server (`/inference`) for voice input |
| `PIPER_URL` | unset | piper-tts HTTP server (`POST /`) for read-aloud |
| `EMBEDDING_MODEL` | unset | Default Ollama embedding model for RAG ingest + retrieval; users override per-upload via the settings dropdown |
| `RAG_TOP_K` | `4` | How many retrieved chunks to inject as system context per turn |
| `MAX_DOCUMENT_MB` | `10` | Max raw upload size for RAG documents (text or PDF) |
| `WEB_SEARCH_ENABLED` | `0` | Master switch for web search. Searching reaches the internet from this host's IP, so it's opt-in. |
| `WEB_SEARCH_MAX_RESULTS` | `5` | Results injected as context per searched turn |
| `WEB_SEARCH_FETCH_COUNT` | `3` | How many of those get their page fetched + extracted. The rest contribute a snippet. Concurrent under a fixed deadline, so this costs memory rather than wall-clock. |
| `WEB_SEARCH_MAX_PAGE_KB` | `512` | Hard cap on one fetched page, enforced while streaming |
| `WEB_SEARCH_QUERY_MODEL` | unset | Chat model that rewrites a turn into a standalone search query. Raw turn is used when unset. |
| `WEB_SEARCH_ALLOW_SCRAPERS` | `0` | Allow the Google/Bing/DuckDuckGo HTML-scraping backends. Off by default — ToS + CAPTCHAs. |
| `WEB_SEARCH_EXCLUDE_BACKENDS` | `marginalia` | Comma-separated backends to skip. One stalled backend delays every search, since results aggregate only after all of them answer. |
| `CHAT_TTL_DAYS` | `30` | Conversations older than this are purged hourly |
| `CHAT_DB_PATH` | `chat.db` | SQLite file (relative to backend cwd) |
| `CHAT_RATE_PER_MIN` | `60` | Per-user token-bucket cap on `/api/chat`. `0` disables. |
| `AUTH_RATE_PER_MIN` | `20` | Per-IP cap on `/auth/login` + `/auth/callback`. `0` disables. |
| `IMAGE_GEN_CONCURRENCY` | `1` | Max concurrent image-gen jobs. VRAM-bound on Apple Silicon. |
| `SESSION_KEY` | _ephemeral_ | 64-byte hex; generate with `openssl rand -hex 64`. Sessions invalidate on restart if unset. |
| `DEV_AUTH` | `0` | When `1`, `/auth/login` writes a session without OIDC |
| `OIDC_ISSUER` | unset | Kanidm OIDC issuer URL, e.g. `https://kanidm.lan/oauth2/openid/chat` |
| `OIDC_CLIENT_ID` | unset | OIDC client id registered in kanidm |
| `OIDC_CLIENT_SECRET` | unset | OIDC client secret |
| `OIDC_REDIRECT_URL` | unset | Public callback URL, e.g. `https://chat.lan/auth/callback` |

## API

```
GET    /status                         { upstream, model_locked, auth,
                                         refiner_available, img2img_available,
                                         voice_in_available, voice_out_available,
                                         rag_available, web_search_available }
GET    /auth/login                     start auth (dev: writes session; oidc: redirect)
GET    /auth/callback                  oidc callback (validates state + nonce + id_token)
POST   /auth/logout                    clear session

GET    /api/me                         { sub, username } or 401
DELETE /api/me                         account self-delete (cascades all data)
GET    /api/models                     installed Ollama models, filtered to those with chat caps
GET    /api/models/caps?model=…        capability snapshot { vision, tools, chat, capabilities, families }
GET    /api/personas                   list refiner-persona presets
GET    /api/voices                     list piper voices (proxied)
GET    /api/embedding-models           installed Ollama models with the `embedding` capability
GET    /api/search?q=…                 FTS5 full-text search across the user's messages

GET    /api/conversations              list user's conversations
POST   /api/conversations              { title?, model? } → Conversation
PATCH  /api/conversations/{id}         { title } — manual rename
DELETE /api/conversations/{id}
GET    /api/conversations/{id}/messages
DELETE /api/conversations/{id}/messages/{msg_id}            delete from row onward
POST   /api/conversations/{id}/messages/{msg_id}/cancel     interrupt pending image job
GET    /api/conversations/{id}/messages/{msg_id}/image/{idx}  blob (ETag + immutable cache)
GET    /api/conversations/{id}/messages/{msg_id}/mask          inpaint mask blob

GET    /api/documents                  list uploaded RAG documents
POST   /api/documents                  { name, content_b64, mime?, model? } → Document
                                       Accepts text, markdown, or PDF (server-side text
                                       extract). Chunked + embedded once at upload.
DELETE /api/documents/{id}             cascade-delete document + chunks

POST   /api/chat                       { conv_id, content, model?, images?, mode?,
                                         refine?, persona?, sub_mode?, mask?,
                                         negative?, retry_assistant_id?,
                                         regenerate_from_user? } → SSE stream.
                                         sub_mode ∈ txt2img|img2img|inpaint (omit to
                                         infer from images/mask). Events:
                                         delta (raw text), done ({conv_id}),
                                         error ({message}), stats ({tokens,
                                         prompt_tokens, tokens_per_sec}),
                                         context ({sources: [{name, score}]}),
                                         progress ({value, max}),
                                         preview ({mime, b64}), queued
POST   /api/transcribe?lang=…          raw audio body → { text }  (whisper.cpp)
POST   /api/tts                        { text, voice?, format? } → audio/ogg|wav (piper)
```

### `/api/v1` — stateless image API (MCP bridge)

Bearer-auth (`CHAT_MCP_API_KEY`), no session, nothing persisted. Each
generator returns an SSE stream (`progress` / `preview` / `done` /
`error`); the render is stashed in a short-lived buffer fetched by uuid.
See [`mcp/README.md`](mcp/README.md).

```
POST   /api/v1/txt2img                 { prompt, negative_prompt?, steps? } (Z-Image Turbo)
POST   /api/v1/img2img                 { prompt, images[], steps? }         (Flux Kontext)
POST   /api/v1/inpaint                 { prompt, image, mask, negative_prompt?, steps? } (Flux Fill)
GET    /api/v1/images/{uuid}.png       fetch a buffered render (TTL ~30 min)
```

## Voice dictation

The mic button records utterance-by-utterance. The browser:

1. Opens a single mic stream + spins up a Web Audio analyser
2. Walks the RMS energy every animation frame. Speech is confirmed after
   ≥250 ms above an RMS threshold; an utterance ends after 700 ms of silence
3. Each utterance gets its own MediaRecorder. On VAD silence the recorder is
   stopped → the resulting WebM blob is appended to a serial transcribe
   queue → a fresh recorder starts for the next utterance
4. Each transcribe pass uploads as 16 kHz mono WAV (whisper.cpp's dr_wav
   decoder rejects opus / webm). Transcripts append in speech order

This keeps already-committed text immutable (whisper isn't deterministic on
re-runs) and discards trailing segments that didn't actually contain speech,
so the stop click itself doesn't show up as a phantom utterance. Language
hint (`?lang=`) defaults to the browser locale; settings can pin to en / fi
/ sv.

## RAG

Upload a text, markdown, or PDF file in **settings → documents**. The
backend:

1. Detects the format from magic bytes (`%PDF-` or UTF-8 text)
2. Extracts plain text — `pdf_extract` for PDFs, raw decode for text/md
3. Slides over the text with overlapping ~800-character windows
4. Embeds each chunk via Ollama using the model the user picked at upload
   time (stored per-document — the embedding model can be switched freely
   for new uploads without invalidating older docs)
5. Stores chunks + f32 vectors as BLOB columns

On every chat turn the user's prompt is embedded once per distinct model in
their corpus, cosine-ranked against every chunk, and the top `RAG_TOP_K`
chunks (score > 0.3) prepend as a system message with `[doc-name]` tags.
The UI surfaces a small "from: [docA] [docB]" chip under the assistant
reply via a streaming `context` SSE event so the user can see what was
consulted. Failures stay non-fatal — the assistant just sees the prompt
without retrieved context.

In-memory cosine works fine for a few hundred chunks; sqlite-vec or a
proper ANN index is the upgrade path when the corpus grows.

## Web search

Nothing to deploy and no API key: search runs **in-process** via the
[`daedra`](https://crates.io/crates/daedra) crate, which fans a query
across unkeyed backends (mwmbl, Marginalia, Bing RSS, Google News,
Hacker News, Wikipedia, StackExchange, GitHub, Wiby, DDG Instant) and
falls down the chain when one is blocked, with a circuit breaker per
backend. Set `WEB_SEARCH_ENABLED=1` and the composer grows a
`travel_explore` toggle on chat turns.

The HTML-scraping backends (Google, Bing, DuckDuckGo SERPs) are
excluded by default — scraping those breaks their terms of service and
earns CAPTCHAs. `WEB_SEARCH_ALLOW_SCRAPERS=1` re-enables them.

**One slow backend is everyone's problem.** The chain awaits all of
them before aggregating, so a backend that hangs is added to every
search — `marginalia` is excluded by default for exactly that (its
public endpoint stopped answering the URL shape the crate builds, which
cost a measured 60 s per search). If searches get slow, that is the
first thing to look at: run with `RUST_LOG=daedra=debug` and compare
each backend's completion timestamp.

When the toggle is on, the backend retrieves before it streams:

- A message containing an `http(s)` URL fetches that page —
  "summarise this page".
- Anything else searches, then fetches the top `WEB_SEARCH_FETCH_COUNT`
  hits and reduces them to article text (search backends return only
  short snippets, which is thin ground for an answer). A page that
  refuses us — Stack Overflow 403s bots — keeps its snippet rather than
  dropping the source, as does one that misses the fetch deadline.
  Nothing streams while this runs, so the phase is bounded on both
  sides: a few pages in flight at once, and a hard cut-off that keeps
  whatever landed. With `WEB_SEARCH_QUERY_MODEL` set, the turn is
  first rewritten into a standalone query so follow-ups ("what about
  the second one?") don't get searched verbatim.

Results fold into the same system message and the same `context` SSE
event as RAG, so a turn can cite documents and live pages together.
Citations are persisted on the assistant row and survive a reload; web
entries render as links in the sources chip.

This is deterministic pre-retrieval, not function calling — the model
never decides to search, so it works on models with no `tools`
capability.

### Fetching safely

The backend shares a LAN with kanidm, vaultwarden and every other
service, and web search makes it fetch URLs influenced by chat input.
`backend/src/safefetch.rs` is the only thing that fetches: http(s) only,
no credentials in the URL, default ports only; every resolved address is
checked against the non-public ranges (RFC1918, loopback, link-local
incl. `169.254.169.254`, CGNAT, v4-in-v6, 6to4) *before* connecting, and
the connection is pinned to the address that passed so a second DNS
answer can't swap in a private one; redirects are followed by hand, max
3 hops, each re-validated; the body must be HTML or text and is streamed
under a hard byte cap. `daedra`'s own fetcher is deliberately not used
for this path.

## Design system

The `chat-design` skill in `.claude/skills/chat-design/` is the source of
truth for visual language. Run `/chat-design` from inside Claude Code to load
it, or read `.claude/skills/chat-design/SKILL.md` directly.

The chat app reuses the halo theme verbatim (`frontend/src/themes.ts`) so
both apps render identical colors, fonts, and shadow tokens. Only the
wordmark glyph differs: chat bubble + accent dot vs. ring + accent dot.

## CI

GitHub Actions in `.github/workflows/`:

- `ci.yaml` — frontend (lint, format, typecheck, build) + backend (clippy
  with `-D warnings`, test, build) on push/PR to `main`.
- `automerge.yaml` — auto-merges Dependabot PRs that pass CI.
- `dockerimage.yaml` — builds + publishes the multi-arch GHCR image on
  push to `main` (release branch).
- `dependabot.yaml` — weekly updates for npm, cargo, and github-actions,
  with React, TanStack, and markdown deps grouped to keep PR noise low.

## Roadmap / TODO

- [ ] Persist token / TPS stats so the caption survives reloads
- [ ] Per-conversation RAG opt-out toggle
- [ ] Conversation export / import (JSON dump with image blobs)
- [ ] sqlite-vec for RAG retrieval when the corpus outgrows in-memory cosine
- [ ] OIDC RP-initiated logout (currently relies on provider session TTL)

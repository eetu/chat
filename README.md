# royale with chat

<p>
  <img src="documentation/screenshots/main.png" alt="desktop" />
</p>

Minimal, self-hosted chat UI for an Ollama endpoint on the LAN. Sibling project
to [halo](../hcc) — same design tokens, different glyph, same warm orange dot.

- Streaming token-by-token responses (SSE) with end-of-turn token / TPS stats
- Per-user conversation history in SQLite (kanidm OIDC; dev fallback for local work)
- Configurable retention (default 30 days)
- Image generation via Ollama's `/v1/images/generations`, plus img2img through
  ComfyUI Flux Kontext (multi-image references, live progress + preview)
- Voice input via whisper.cpp (`/inference`) and read-aloud via piper-tts
  (`/`) — both LAN endpoints; client transcodes mic audio to 16 kHz mono WAV
  before upload
- Optional prompt refiner with persona-flavoured rewrites for image generation
- Manual rename + edit-and-resend + remix-this-image flows
- Code blocks render with language label, streaming-fence balancing, copy button
- Per-user rate limits, image-gen semaphore, CSP + magic-byte MIME sniffing on
  uploaded images
- No navbars, no chrome — just the conversation

## Stack

| Layer | Tech |
|---|---|
| Backend | Rust, actix-web 4, actix-web-lab SSE, reqwest stream → Ollama NDJSON, ComfyUI WS for progress, tokio semaphore for GPU coordination, rusqlite (bundled), actix-session cookie store, tracing-actix-web spans |
| Frontend | Vite + React 19, Emotion theme (copied from halo), TanStack Router (file-based), SWR, MediaRecorder + Web Audio API for voice in |
| Auth | OIDC (kanidm) — code + PKCE flow with cookie session. Dev fallback via `DEV_AUTH=1`. |
| Persistence | SQLite (`chat.db` by default), schema bootstrapped on startup |

## Layout

```
chat/
├── backend/         actix-web server, SSE proxy, SQLite, auth
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

## Configuration

See `backend/.env.example` for the full list. Key values:

| Var | Default | Purpose |
|---|---|---|
| `OLLAMA_URL` | `http://localhost:11434` | Upstream Ollama HTTP endpoint |
| `OLLAMA_MODEL` | unset | If set, locks all chats to this model and ignores client selection |
| `COMFYUI_URL` | unset | Enables img2img via Flux Kontext when set |
| `PROMPT_REFINER_MODEL` | unset | Chat model that rewrites image-gen prompts before they hit the image runner |
| `WHISPER_URL` | unset | whisper.cpp HTTP server (`/inference`) for voice input |
| `PIPER_URL` | unset | piper-tts HTTP server (`POST /`) for read-aloud |
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
                                         voice_in_available, voice_out_available }
GET    /auth/login                     start auth (dev: writes session; oidc: redirect)
GET    /auth/callback                  oidc callback (validates state + nonce + id_token)
POST   /auth/logout                    clear session

GET    /api/me                         { sub, username } or 401
DELETE /api/me                         account self-delete (cascades all data)
GET    /api/models                     proxy of /api/tags (or { locked: true })
GET    /api/models/caps?model=…        capability snapshot { vision, tools, chat, image_gen }
GET    /api/personas                   list refiner-persona presets
GET    /api/voices                     list piper voices (proxied)

GET    /api/conversations              list user's conversations
POST   /api/conversations              { title?, model? } → Conversation
PATCH  /api/conversations/{id}         { title } — manual rename
DELETE /api/conversations/{id}
GET    /api/conversations/{id}/messages
DELETE /api/conversations/{id}/messages/{msg_id}            delete from row onward
POST   /api/conversations/{id}/messages/{msg_id}/cancel     interrupt pending image job
GET    /api/conversations/{id}/messages/{msg_id}/image/{idx}  blob (ETag + immutable cache)

POST   /api/chat                       { conv_id, content, model?, images?, mode?,
                                         refine?, persona?, retry_assistant_id? }
                                       → SSE stream. Events:
                                         delta (raw text), done ({conv_id}),
                                         error ({message}), stats ({tokens,
                                         prompt_tokens, tokens_per_sec}),
                                         progress ({value, max}),
                                         preview ({mime, b64}), queued
POST   /api/transcribe                 raw audio body → { text }  (whisper.cpp)
POST   /api/tts                        { text, voice? } → audio/wav  (piper)
```

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

- [ ] Mermaid diagram rendering
- [ ] File ingestion / RAG for PDFs and docs — see
  `.claude/skills/chat-design/README.md` → "Future renderer extensions",
  two-tier plan (naive injection vs. embed+retrieve via `sqlite-vec`)
- [ ] Conversation search (FTS5 + ⌘K palette)
- [ ] Long-press for the conversation action menu on touch devices

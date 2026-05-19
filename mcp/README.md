# chat-mcp

MCP bridge that exposes the chat backend's image generation endpoints
(`/api/v1/img2img`, `/api/v1/inpaint`) as MCP tools. Forwards ComfyUI
sampler progress as `notifications/progress` so well-behaved MCP
clients don't time out during multi-minute renders.

Two transports are supported in the same binary:

| Transport | Pick when | How clients connect |
|---|---|---|
| `stdio` *(default for `cargo run`)* | Local dev; MCP client and binary live on the same machine | Client spawns `chat-mcp` (or `docker run -i --rm ...`) as a subprocess |
| `http` *(default in the container)* | Deployed server, multiple devices or web clients | Client opens `https://mcp.example.com/mcp` with a Bearer header |

## Tools

| Name | Backend | Description |
|---|---|---|
| `chat_list_image_models` | Ollama | Discover which models the backend exposes for `chat_txt2img`. |
| `chat_txt2img` | Ollama | Generate a fresh image from a text prompt. Pick a model from the list tool. |
| `chat_img2img` | Flux Kontext (ComfyUI) | Edit one or more reference images using a natural-language prompt. |
| `chat_inpaint` | Flux Fill (ComfyUI) | Repaint a masked region of an image; uses real CFG so negative prompts matter. |

`chat_img2img` and `chat_inpaint` both expose a quality/speed knob
(`steps`) in their tool description — the agent can pick a value
when the user asks for "fast preview" vs "really good quality".
`chat_txt2img` has no steps knob; Ollama's image surface doesn't
expose sampler controls.

### Tool output shape

By default the rendered image is **not** returned inline as base64.
Each render is stashed in the backend's 30-minute in-memory image
buffer; the tool result contains:

1. `Content::text("Image saved at <url> (expires in ~30 min).")`
2. `Content::resource_link(uri=<url>, mimeType=image/png)`

The user can `curl -O <url>` to save the PNG, or click in MCP clients
that render `resource_link` as an actionable item (Claude Desktop).
Since the bytes never enter the LLM's context, a 1024² PNG costs
~80 tokens instead of ~700K.

Pass `inline: true` on any of the three generation tools to also
include the base64 image content block when the agent needs to
*see* the result (chained edits, visual critique). The fetch URL
is still emitted alongside.

Backend knobs (set on the *backend* container, not chat-mcp):

| Env | Default | Purpose |
|---|---|---|
| `CHAT_DEFAULT_IMAGE_MODEL` | unset | Ollama model used by `chat_txt2img` when the caller doesn't pass `model`. Without this, calls fail until the agent supplies one explicitly. Recommended: `x/flux2-klein:4b` or similar. |
| `CHAT_IMAGE_BUFFER_TTL_SECS` | `1800` | How long renders live before sweep drops them. |
| `CHAT_IMAGE_BUFFER_LIMIT` | `64` | Max entries before oldest is evicted on insert. |

## Configuration

| Env var | Required | Default | Purpose |
|---|---|---|---|
| `CHAT_BACKEND_URL` | yes | — | Chat backend base URL used by chat-mcp itself, e.g. `http://chat-backend:8080`. Points at the backend, *not* ComfyUI. Typically a container/LAN hostname. |
| `CHAT_BACKEND_PUBLIC_URL` | no | falls back to `CHAT_BACKEND_URL` | Externally-routable URL the *user* would hit to fetch a rendered image, e.g. `https://chat.example.com`. The mcp tools embed this in their results so the user can `curl` / open it in a browser. Only relevant when the public URL differs from the internal one. |
| `CHAT_MCP_API_KEY` | no | — | mcp→backend Bearer. Must match `CHAT_MCP_API_KEY` on the backend. **Unset or empty omits the header entirely**, which works only if the backend is also running with the key unset (auth-off mode). |
| `CHAT_MCP_TRANSPORT` | no | `stdio` (binary) / `http` (container) | `stdio` or `http`. |
| `CHAT_MCP_SERVER_KEY` | no | — | client→mcp Bearer in HTTP mode. **Unset or empty disables the auth middleware** — the listener accepts every request. Set when exposing beyond a trusted LAN. |
| `PORT` | no | `8090` | HTTP listen port. |
| `CHAT_MCP_BIND` | no | `0.0.0.0` | HTTP listen address. |
| `CHAT_MCP_MOUNT_PATH` | no | `/mcp` | HTTP mount path for the MCP service. |
| `RUST_LOG` | no | `chat_mcp=info` | Logs go to stderr (stdout is the MCP wire in stdio mode). |

Two keys, two trust boundaries: `CHAT_MCP_SERVER_KEY` gates *clients*
hitting this server; `CHAT_MCP_API_KEY` gates *this server* hitting
the chat backend. They can be the same value if you don't care about
the isolation, but separating them costs nothing.

Leaving `CHAT_MCP_SERVER_KEY` unset turns off the client-side auth
middleware entirely — useful for LAN-only deployments behind another
auth layer (Tailscale, mTLS at the reverse proxy, `127.0.0.1` bind).
The server logs a startup warning when it boots without a key, and
the `auth=off` marker shows up in the listening-on log line.

## Build

### Container (recommended)

CI publishes `ghcr.io/eetu/chat-mcp:latest` alongside the backend image on every push to `main`.

Local build:

```sh
docker build --target mcp-runner -t chat-mcp:dev .
```

### Native binary

```sh
cargo build -p chat-mcp --release
```

Binary lands in `target/release/chat-mcp`.

## Usage — HTTP transport (deployed server)

### Run the server

Container:

```sh
docker run --rm -p 8090:8090 \
  -e CHAT_BACKEND_URL=https://chat.example.com \
  -e CHAT_MCP_API_KEY=$BACKEND_KEY \
  -e CHAT_MCP_SERVER_KEY=$SERVER_KEY \
  ghcr.io/eetu/chat-mcp:latest
```

Native:

```sh
CHAT_MCP_TRANSPORT=http \
CHAT_BACKEND_URL=https://chat.example.com \
CHAT_MCP_API_KEY=$BACKEND_KEY \
CHAT_MCP_SERVER_KEY=$SERVER_KEY \
PORT=8090 \
./target/release/chat-mcp
```

Smoke test:

```sh
curl -i http://localhost:8090/health
# → 200 OK, body: ok

curl -i http://localhost:8090/mcp
# → 401 Unauthorized, WWW-Authenticate: Bearer realm="chat-mcp"

curl -i -X POST http://localhost:8090/mcp \
  -H "Authorization: Bearer $SERVER_KEY" \
  -H "Accept: application/json, text/event-stream" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize",
       "params":{"protocolVersion":"2025-06-18",
                 "capabilities":{},
                 "clientInfo":{"name":"curl","version":"0"}}}'
# → 200, Mcp-Session-Id: ...
```

### Reverse proxy

Put the server behind TLS at e.g. `https://mcp.example.com`. The MCP
service is mounted at `/mcp` by default; `/health` is unauthenticated
and suitable for liveness probes. Stream MCP responses without
buffering — they're SSE.

Traefik labels (illustrative):

```ini
traefik.http.routers.chat-mcp.rule=Host(`mcp.example.com`)
traefik.http.routers.chat-mcp.entrypoints=websecure
traefik.http.routers.chat-mcp.tls.certresolver=letsencrypt
traefik.http.services.chat-mcp.loadbalancer.server.port=8090
```

### Install in Claude Code (HTTP)

```sh
claude mcp add chat-image \
  --transport http \
  --url https://mcp.example.com/mcp \
  --header "Authorization: Bearer $SERVER_KEY"
```

### Install in Claude Desktop (HTTP)

```json
{
  "mcpServers": {
    "chat-image": {
      "url": "https://mcp.example.com/mcp",
      "headers": {
        "Authorization": "Bearer ${CHAT_MCP_SERVER_KEY}"
      }
    }
  }
}
```

## Usage — stdio transport (local subprocess)

The MCP client spawns the binary as a child process and talks over
stdin/stdout. No HTTP listener; no server key needed.

### Install in Claude Code (stdio, native binary)

```sh
claude mcp add chat-image \
  --env CHAT_BACKEND_URL=http://chat.lan:8080 \
  --env CHAT_MCP_API_KEY=$(pass chat/mcp-key) \
  -- /path/to/target/release/chat-mcp
```

### Install in Claude Code (stdio, container)

```sh
claude mcp add chat-image -- \
  docker run -i --rm \
    -e CHAT_MCP_TRANSPORT=stdio \
    -e CHAT_BACKEND_URL=http://chat.lan:8080 \
    -e CHAT_MCP_API_KEY=$(pass chat/mcp-key) \
    ghcr.io/eetu/chat-mcp:latest
```

`-i` is required — the MCP client owns the container's stdin/stdout.
`-e CHAT_MCP_TRANSPORT=stdio` overrides the container's HTTP default.

### Install in Claude Desktop (stdio, container)

`~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "chat-image": {
      "command": "docker",
      "args": [
        "run", "-i", "--rm",
        "-e", "CHAT_MCP_TRANSPORT=stdio",
        "-e", "CHAT_BACKEND_URL",
        "-e", "CHAT_MCP_API_KEY",
        "ghcr.io/eetu/chat-mcp:latest"
      ],
      "env": {
        "CHAT_BACKEND_URL": "http://chat.lan:8080",
        "CHAT_MCP_API_KEY": "..."
      }
    }
  }
}
```

## How progress flows

```
ComfyUI WS  →  backend SSE  →  reqwest-eventsource  →  MCP progress notifications
   │              │                  │                       │
   │              └─ progress, preview, done, error
   │
   └─ /ws progress + binary preview frames
```

`preview` events are parsed but currently dropped — MCP tools resolve
to a single result and the spec has no carrier for mid-call previews.
The final `done` event becomes the tool's image content block.

## Cancellation

- **stdio** — closing the MCP client tears down the subprocess.
- **HTTP** — closing the MCP session (HTTP `DELETE /mcp`) or dropping
  the SSE consumer terminates the in-flight tool call.

In both cases `reqwest-eventsource` drops the backend connection, the
chat backend watches `mpsc::Sender::closed()`, and ComfyUI gets
`POST /interrupt` so the ongoing sampler stops rather than burning
GPU cycles on a render nobody will see.

## Security notes

- Bearer tokens travel in headers; always front HTTP mode with TLS.
- `CHAT_MCP_SERVER_KEY` must be long and random (≥ 32 bytes). The
  server uses constant-time compare to prevent length-leak timing
  attacks but a short key is still trivial to brute-force.
- Leaving `CHAT_MCP_SERVER_KEY` unset disables client auth and prints
  a `WARN` line at startup. Safe for `127.0.0.1`-bind or trusted-LAN
  deployments; do not combine with a public bind.
- The container runs as UID 1000; SQLite / file mounts on the
  backend container have no analogue here — chat-mcp is stateless.
- The MCP server does not currently rate-limit. Concurrency is bounded
  by the backend's `image_sem` (default 1) so the worst case is queue,
  not load. If you expose the server to the public internet, add a
  reverse-proxy rate limit.
- CORS is not enabled — the server is not reachable from `claude.ai`
  in a browser yet. Add a `tower_http::cors::CorsLayer` if you want
  web client access.

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
| `chat_img2img` | Flux Kontext | Edit one or more reference images using a natural-language prompt. |
| `chat_inpaint` | Flux Fill | Repaint a masked region of an image; uses real CFG so negative prompts matter. |

Both tools document a quality/speed knob (`steps`) in their tool
description — the agent can pick a value when the user asks for
"fast preview" vs "really good quality".

## Configuration

| Env var | Required | Default | Purpose |
|---|---|---|---|
| `CHAT_BACKEND_URL` | yes | — | Chat backend base URL, e.g. `https://chat.example.com`. Points at the backend, *not* ComfyUI. |
| `CHAT_MCP_API_KEY` | yes | — | mcp→backend Bearer. Must match `CHAT_MCP_API_KEY` set on the backend. |
| `CHAT_MCP_TRANSPORT` | no | `stdio` (binary) / `http` (container) | `stdio` or `http`. |
| `CHAT_MCP_SERVER_KEY` | when `http` | — | client→mcp Bearer. Required only in HTTP mode. |
| `PORT` | no | `8090` | HTTP listen port. |
| `CHAT_MCP_BIND` | no | `0.0.0.0` | HTTP listen address. |
| `CHAT_MCP_MOUNT_PATH` | no | `/mcp` | HTTP mount path for the MCP service. |
| `RUST_LOG` | no | `chat_mcp=info` | Logs go to stderr (stdout is the MCP wire in stdio mode). |

Two keys, two trust boundaries: `CHAT_MCP_SERVER_KEY` gates *clients*
hitting this server; `CHAT_MCP_API_KEY` gates *this server* hitting
the chat backend. They can be the same value if you don't care about
the isolation, but separating them costs nothing.

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
- The container runs as UID 1000; SQLite / file mounts on the
  backend container have no analogue here — chat-mcp is stateless.
- The MCP server does not currently rate-limit. Concurrency is bounded
  by the backend's `image_sem` (default 1) so the worst case is queue,
  not load. If you expose the server to the public internet, add a
  reverse-proxy rate limit.
- CORS is not enabled — the server is not reachable from `claude.ai`
  in a browser yet. Add a `tower_http::cors::CorsLayer` if you want
  web client access.

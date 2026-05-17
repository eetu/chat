# chat-mcp

MCP bridge that exposes the chat backend's image generation endpoints
(`/api/v1/img2img`, `/api/v1/inpaint`) as MCP tools. Speaks MCP over
stdio. Forwards ComfyUI sampler progress as `notifications/progress`
so well-behaved MCP clients don't time out during multi-minute
renders.

## Tools

| Name | Backend | Description |
|---|---|---|
| `chat_img2img` | Flux Kontext | Edit one or more reference images using a natural-language prompt. |
| `chat_inpaint` | Flux Fill | Repaint a masked region of an image; uses real CFG so negative prompts matter. |

Both tools document a quality/speed knob (`steps`) in their tool
description — the agent can pick a value when the user asks for
"fast preview" vs "really good quality".

## Configuration

| Env var | Required | Purpose |
|---|---|---|
| `CHAT_BACKEND_URL` | yes | e.g. `http://chat.lan:8080` — points at the chat backend, *not* ComfyUI. |
| `CHAT_MCP_API_KEY` | yes | Bearer token; must match `CHAT_MCP_API_KEY` on the backend. |
| `RUST_LOG` | no | Defaults to `chat_mcp=info`. Logs go to stderr (stdout is the MCP wire). |

## Build

### Container (recommended)

The repo's `Dockerfile` has a dedicated `mcp-runner` final stage. CI
publishes `ghcr.io/<owner>/chat-mcp:latest` alongside the backend
image on every push to `main`.

Local build:

```sh
docker build --target mcp-runner -t chat-mcp:dev .
```

### Native binary

```sh
cargo build -p chat-mcp --release
```

Binary lands in `target/release/chat-mcp`.

## Install in Claude Code

Using the published image:

```sh
claude mcp add chat-image -- \
  docker run -i --rm \
    -e CHAT_BACKEND_URL=http://chat.lan:8080 \
    -e CHAT_MCP_API_KEY=$(pass chat/mcp-key) \
    ghcr.io/eetu/chat-mcp:latest
```

`-i` is required — the MCP client owns the container's stdin/stdout.
Omit `--rm` if you want to keep the container around for log
inspection between calls.

Using a local binary instead:

```sh
claude mcp add chat-image \
  --env CHAT_BACKEND_URL=http://chat.lan:8080 \
  --env CHAT_MCP_API_KEY=$(pass chat/mcp-key) \
  -- /path/to/target/release/chat-mcp
```

## Install in Claude Desktop

Add to `~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "chat-image": {
      "command": "docker",
      "args": [
        "run", "-i", "--rm",
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

Closing the MCP client (or the agent issuing a tool cancel) drops the
SSE consumer. `reqwest-eventsource` tears down the TCP connection;
the backend's chat handler watches `mpsc::Sender::closed()` and posts
`/interrupt` to ComfyUI so the ongoing sampler stops.

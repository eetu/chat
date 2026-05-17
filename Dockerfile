# syntax=docker/dockerfile:1

# --- Cross-compilation helper ---
FROM --platform=$BUILDPLATFORM tonistiigi/xx AS xx

# --- Stage 1: Build frontend (native, output is platform-independent) ---
FROM --platform=$BUILDPLATFORM node:24-alpine AS frontend-build
ARG CHAT_IMAGE_TAG
ENV VITE_CHAT_IMAGE_TAG=$CHAT_IMAGE_TAG
WORKDIR /app
COPY frontend/package.json frontend/yarn.lock frontend/.yarnrc.yml* ./
RUN corepack enable && yarn install --immutable --network-timeout 1000000
COPY frontend/ .
RUN yarn build

# --- Stage 2: Build workspace dependencies (native, cross-compiled) ---
#
# Caches every dependency in the workspace (backend + mcp + shared) by
# compiling a stub source tree. Both `backend-build` and `mcp-build`
# extend this stage so they share the dep-compile cache, which on cold
# builds dominates wall time. Only Cargo manifests are copied here; the
# real source lands in the per-binary build stage so a source edit
# doesn't bust the dep cache.
FROM --platform=$BUILDPLATFORM rust:1-alpine AS workspace-deps
COPY --from=xx / /
RUN apk add --no-cache clang lld musl-dev curl
ARG TARGETPLATFORM
RUN xx-apk add --no-cache musl-dev gcc
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY backend/Cargo.toml backend/Cargo.toml
COPY shared/Cargo.toml shared/Cargo.toml
COPY mcp/Cargo.toml mcp/Cargo.toml
RUN mkdir -p backend/src shared/src mcp/src \
    && printf 'fn main() {}\n' > backend/src/main.rs \
    && : > shared/src/lib.rs \
    && printf 'fn main() {}\n' > mcp/src/main.rs \
    && xx-cargo build --release --workspace \
    && rm -rf backend/src shared/src mcp/src

# --- Stage 3a: Build chat-backend ---
FROM workspace-deps AS backend-build
ARG TARGETPLATFORM
COPY shared/src ./shared/src
COPY backend/src ./backend/src
# `touch` so cargo notices the stub→real source swap. Workspace shares
# a target dir so only the changed package rebuilds.
RUN touch shared/src/lib.rs backend/src/main.rs \
    && xx-cargo build --release -p chat-backend

# --- Stage 3b: Build chat-mcp ---
FROM workspace-deps AS mcp-build
ARG TARGETPLATFORM
COPY shared/src ./shared/src
COPY mcp/src ./mcp/src
RUN touch shared/src/lib.rs mcp/src/main.rs \
    && xx-cargo build --release -p chat-mcp

# --- Stage 4a: Backend runtime ---
FROM scratch AS runner
WORKDIR /app
LABEL org.opencontainers.image.description="royale with chat — self-hosted LAN chat for an Ollama endpoint"
LABEL org.opencontainers.image.source="https://github.com/eetu/chat"

COPY --from=backend-build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
COPY --from=backend-build /app/target/*/release/chat-backend ./chat-backend
COPY --from=frontend-build /app/dist ./dist

# Sensible runtime defaults — override via -e at run time.
ENV STATIC_DIR=./dist
ENV CHAT_DB_PATH=/data/chat.db
ENV PORT=8080

USER 1000

EXPOSE 8080

CMD ["./chat-backend"]

# --- Stage 4b: chat-mcp runtime ---
#
# Default transport is HTTP — the container model fits a long-running
# remote MCP server better than per-call subprocesses. Override to
# `stdio` (and run with `docker run -i --rm ...`) when the MCP client
# wants to spawn the container as a subprocess instead.
#
# Required at run time:
#   - CHAT_BACKEND_URL (e.g. https://chat.example.com)
#   - CHAT_MCP_API_KEY (mcp→backend auth, matches the backend's value)
#   - CHAT_MCP_SERVER_KEY (client→mcp Bearer, only used when transport=http)
FROM scratch AS mcp-runner
WORKDIR /app
LABEL org.opencontainers.image.description="chat-mcp — MCP bridge for chat's img2img / inpaint endpoints"
LABEL org.opencontainers.image.source="https://github.com/eetu/chat"

COPY --from=mcp-build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
COPY --from=mcp-build /app/target/*/release/chat-mcp ./chat-mcp

ENV CHAT_MCP_TRANSPORT=http
ENV PORT=8090

USER 1000

EXPOSE 8090

ENTRYPOINT ["./chat-mcp"]

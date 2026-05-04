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

# --- Stage 2: Build backend dependencies (native, cross-compiled) ---
FROM --platform=$BUILDPLATFORM rust:1-alpine AS backend-deps
COPY --from=xx / /
RUN apk add --no-cache clang lld musl-dev curl
ARG TARGETPLATFORM
RUN xx-apk add --no-cache musl-dev gcc
WORKDIR /app
COPY backend/Cargo.toml backend/Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs \
    && xx-cargo build --release \
    && rm -rf src

# --- Stage 3: Build backend ---
FROM backend-deps AS backend-build
ARG TARGETPLATFORM
COPY backend/src ./src
RUN touch src/main.rs && xx-cargo build --release

# --- Stage 4: Runtime ---
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

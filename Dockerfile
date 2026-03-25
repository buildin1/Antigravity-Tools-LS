# --- Frontend Stage (Pre-built on Host) ---
FROM alpine:latest AS frontend-builder
WORKDIR /app
# 复制宿主机已编译好的静态资源（架构无关）
COPY apps/web-dashboard/dist ./dist/

# --- Backend Build Stage ---
FROM rust:1-slim-bookworm AS backend-builder
RUN apt-get update && apt-get install -y \
    pkg-config libssl-dev build-essential protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# 复制工作空间配置并动态清理无关成员
COPY Cargo.toml ./
RUN sed -i '/"apps\/desktop\/src-tauri"/d' Cargo.toml

# 复制核心后端源码
COPY apps/cli-server ./apps/cli-server
COPY transcoder-core ./transcoder-core
COPY ls-orchestrator ./ls-orchestrator
COPY ls-accounts ./ls-accounts

# 编译后端二进制 (由 buildx 自动决定目标架构)
RUN cargo build --release --bin cli-server

# --- Final Runtime Stage ---
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y \
    libssl3 ca-certificates curl binutils xz-utils \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# 复制二进制和前端静态资源
COPY --from=backend-builder /app/target/release/cli-server ./antigravity-server
COPY --from=frontend-builder /app/dist ./dist

# 运行时配置
ENV ABV_DIST_PATH=/app/dist
ENV PORT=5173
ENV RUST_LOG=info

EXPOSE 5173

# 运行指令
ENTRYPOINT ["/app/antigravity-server"]

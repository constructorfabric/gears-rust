# Multi-stage build for cf-server with mini-chat + k8s features
# Stage 1: Builder
FROM rust:1.95.0-bookworm@sha256:6bb82db0878825e157664188b319c875de4f1fff5d70f5917b3a3f1974b472e4 AS builder

# Build arguments
ARG CARGO_FEATURES=mini-chat,static-authn,static-authz,single-tenant,static-credstore,k8s
ARG BUILD_PROFILE=dev

# Install protobuf-compiler for prost-build
RUN apt-get update && \
    apt-get install -y --no-install-recommends cmake protobuf-compiler libprotobuf-dev && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy workspace files
COPY Cargo.toml Cargo.lock ./
COPY rust-toolchain.toml ./

# Copy all workspace members
COPY apps/cf-server ./apps/cf-server
COPY apps/gts-docs-validator ./apps/gts-docs-validator
COPY libs ./libs
COPY modules ./modules
COPY examples ./examples
COPY config ./config
COPY proto ./proto

# Build the cf-server binary.
# BUILD_PROFILE: "dev" (default, fast compile) or "release" (optimized).
# BuildKit cache mounts persist cargo registry + target dir across builds.
# On linux hosts (same triple as the container), this reuses compiled deps.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/build/target,sharing=locked \
    RELEASE_FLAG="" && \
    OUTPUT_DIR="debug" && \
    if [ "$BUILD_PROFILE" = "release" ]; then \
        RELEASE_FLAG="--release"; \
        OUTPUT_DIR="release"; \
    fi && \
    if [ -n "$CARGO_FEATURES" ]; then \
        cargo build $RELEASE_FLAG --bin cf-server --package=cf-server --features "$CARGO_FEATURES"; \
    else \
        cargo build $RELEASE_FLAG --bin cf-server --package=cf-server; \
    fi && \
    cp /build/target/$OUTPUT_DIR/cf-server /tmp/cf-server

# Stage 2: Runtime
FROM debian:13.3-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the built binary from builder stage (via /tmp because target/ is a cache mount)
COPY --from=builder /tmp/cf-server /app/cf-server
# Copy config
COPY --from=builder /build/config /app/config

# Expose mini-chat API port
EXPOSE 8087

RUN useradd -U -u 1000 appuser && \
    chown -R 1000:1000 /app
USER 1000
CMD ["/app/cf-server", "--config", "/app/config/mini-chat.yaml", "run"]

# Multi-stage build for cf-gears-example-server with mini-chat + k8s features
# Stage 1: Builder
# Matches rust-toolchain.toml (1.97.0); a stale pin here just makes rustup
# download a second toolchain on every build.
FROM rust:1.97.0-bookworm@sha256:8fa55b2f3ddf97471ab6a767bfa3f37e6bad0986ba823e75fea57e2a2a5c3073 AS builder

# Build arguments
ARG CARGO_FEATURES=mini-chat,static-authn,static-authz,single-tenant,static-credstore,k8s
ARG BUILD_PROFILE=dev

# Install protobuf-compiler for prost-build
RUN apt-get update && \
    apt-get install -y --no-install-recommends cmake protobuf-compiler libprotobuf-dev && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy workspace files
COPY Cargo.toml Cargo.lock Gears.toml ./
COPY rust-toolchain.toml ./
COPY .cargo ./.cargo
COPY tools ./tools

# Copy all workspace members
COPY apps/cf-gears-example-server ./apps/cf-gears-example-server
COPY libs ./libs
COPY gears ./gears
COPY examples ./examples
COPY config ./config
COPY proto ./proto

# Build the cf-gears-example-server binary.
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
        cargo build $RELEASE_FLAG --bin cf-gears-example-server --package=cf-gears-example-server --features "$CARGO_FEATURES"; \
    else \
        cargo build $RELEASE_FLAG --bin cf-gears-example-server --package=cf-gears-example-server; \
    fi && \
    cp /build/target/$OUTPUT_DIR/cf-gears-example-server /tmp/cf-gears-example-server

# Stage 2: Runtime
FROM debian:13.3-slim@sha256:1d3c811171a08a5adaa4a163fbafd96b61b87aa871bbc7aa15431ac275d3d430

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the built binary from builder stage (via /tmp because target/ is a cache mount)
COPY --from=builder /tmp/cf-gears-example-server /app/cf-gears-example-server
# Copy config
COPY --from=builder /build/config /app/config

# Expose mini-chat API port
EXPOSE 8087

RUN useradd -U -u 1000 appuser && \
    chown -R 1000:1000 /app

# The shipped configs set `server.home_dir: "~/.cf-gears"`. A numeric USER does
# not update HOME, so it stays /root — which uid 1000 cannot write, and the
# server aborts with "Failed to create home_dir: Permission denied". Point HOME
# at /app, which is already owned by uid 1000.
ENV HOME=/app
USER 1000
CMD ["/app/cf-gears-example-server", "--config", "/app/config/mini-chat.yaml", "run"]

# Multi-stage build for cf-gears-example-server with mini-chat + k8s features
# Stage 1: Builder
# Should match rust-toolchain.toml; a stale pin here just makes rustup
# download a second toolchain on every build.
FROM rust:1.98.0-bookworm@sha256:82150a52ec202c1b14d7817e14516c392bb7f5cfebd88f1ed531cb37ebd39922 AS builder

# Build arguments
ARG CARGO_FEATURES=mini-chat,static-authn,static-authz,single-tenant,static-credstore,k8s
ARG BUILD_PROFILE=dev

# Install protobuf-compiler for prost-build
RUN apt-get update && \
    apt-get install -y --no-install-recommends cmake protobuf-compiler libprotobuf-dev && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy the whole workspace rather than enumerating members.
#
# Enumerating is exactly what rotted this file in #4798: the hand-picked list
# silently drifted out of sync and dropped tools/, .cargo/config.toml and
# Gears.toml, breaking the build without anything noticing.
#
# The repo-root .dockerignore already keeps target/, .git and other build
# noise out of the context.
COPY . .

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
FROM debian:13.7-slim@sha256:a99cfc517144bc59b1978475ec53b46ecabec7e43635402ee5b77cc54cd1b20a

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

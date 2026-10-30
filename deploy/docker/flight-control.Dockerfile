# Multi-stage build for the CF/Gears Flight Control image.
#
# The release pipeline (.github/workflows/publish-artifacts.yml) builds this from
# the `flight-control` entry in deploy/components.yaml; the production build args
# live there, not here.
#
# Build by hand:
#   docker build -f deploy/docker/flight-control.Dockerfile \
#     -t ghcr.io/constructorfabric/flight-control:dev .
#
# Build (production OIDC authn):
#   docker build -f deploy/docker/flight-control.Dockerfile \
#     --build-arg CARGO_NO_DEFAULT_FEATURES=1 \
#     --build-arg CARGO_FEATURES="oidc-authn k8s otel" \
#     -t ghcr.io/constructorfabric/flight-control:prod .

# ---------------------------------------------------------------------------
# Stage 1: Builder
# ---------------------------------------------------------------------------
FROM rust:1.99.1-bookworm@sha256:e12566d5abc17a0da741628782cdb504b68b8961f5bfcf677936bf3e8b32de29 AS builder

# BUILD_PROFILE: "release" (default, optimized) or "dev" (fast compile).
ARG BUILD_PROFILE=release
# Kubernetes platform-plane auth is required by the chart's TokenReview config.
ARG CARGO_FEATURES="k8s otel"
# Set to a non-empty value (e.g. "1") to pass --no-default-features.
ARG CARGO_NO_DEFAULT_FEATURES=""

# protobuf-compiler is required by prost-build (gRPC / directory protos).
RUN apt-get update && \
    apt-get install -y --no-install-recommends cmake protobuf-compiler libprotobuf-dev && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy the full workspace context (.dockerignore trims target/, .git/, logs/).
COPY . .

# Build the flight-control binary. BuildKit cache mounts persist the cargo
# registry + target dir across builds; copy the binary out of the cache mount.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/build/target,sharing=locked \
    set -eux; \
    RELEASE_FLAG=""; OUTPUT_DIR="debug"; \
    if [ "$BUILD_PROFILE" = "release" ]; then RELEASE_FLAG="--release"; OUTPUT_DIR="release"; fi; \
    NO_DEFAULT_FLAG=""; \
    if [ -n "$CARGO_NO_DEFAULT_FEATURES" ]; then NO_DEFAULT_FLAG="--no-default-features"; fi; \
        if [ -n "$CARGO_FEATURES" ]; then \
            cargo build $RELEASE_FLAG $NO_DEFAULT_FLAG --features "$CARGO_FEATURES" \
                --bin flight-control --package cf-gears-flight-control; \
        else \
            cargo build $RELEASE_FLAG $NO_DEFAULT_FLAG \
                --bin flight-control --package cf-gears-flight-control; \
        fi; \
    cp "/build/target/$OUTPUT_DIR/flight-control" /tmp/flight-control

# ---------------------------------------------------------------------------
# Stage 2: Runtime
# ---------------------------------------------------------------------------
FROM debian:13.7-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /tmp/flight-control /app/flight-control
COPY --from=builder /build/config/flight-control.yaml /app/config/flight-control.yaml

# HTTP edge / probes and gRPC DirectoryService.
EXPOSE 8087 50051

ENV APP__SERVER__HOME_DIR=/app/data

RUN useradd -U -u 1000 appuser && \
    mkdir -p /app/data && \
    chown -R 1000:1000 /app
USER 1000

CMD ["/app/flight-control", "--config", "/app/config/flight-control.yaml", "run"]
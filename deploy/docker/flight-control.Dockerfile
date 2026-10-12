# Multi-stage build for the CF/Gears Flight Control image.
#
# Build (static authn, Kubernetes platform-plane auth, and OTel):
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
FROM rust:1.98.1-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS builder

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
FROM debian:13.7-slim@sha256:a99cfc517144bc59b1978475ec53b46ecabec7e43635402ee5b77cc54cd1b20a

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
# Image for the composed cf-gears example server, used by the E2E docker lane.
#
# Built by `make e2e-docker` (tools/scripts/ci.py) and by
# testing/docker/docker-compose.yml. The build context is the repository root.
#
# Stage 1: Builder
FROM rust:1.97.0-bookworm@sha256:8fa55b2f3ddf97471ab6a767bfa3f37e6bad0986ba823e75fea57e2a2a5c3073 AS builder

# Cargo features for the composed binary. ci.py forwards --features here;
# empty means `default = []`, i.e. a server with no optional gears.
ARG CARGO_FEATURES
# "release" (default) or "dev" — dev trades runtime speed for much faster builds.
ARG BUILD_PROFILE=release

# protobuf-compiler: prost-build. cmake: libz-ng-sys (and anything else
# vendoring a CMake project). Both are hard build prerequisites — see README.
RUN apt-get update && \
    apt-get install -y --no-install-recommends cmake protobuf-compiler libprotobuf-dev && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy the whole workspace rather than enumerating members.
#
# Enumerating is what rotted the previous version of this file: it silently
# lost `tools/` (whose gts-analyze and xtask are declared workspace members, so
# Cargo could not load the workspace at all), `.cargo/config.toml` (which pins
# PDFIUM_VERSION — load-bearing, an upstream PDFIUM release has broken
# golden-markdown tests before) and `Gears.toml`.
#
# .dockerignore already keeps target/, .git, .github, .venv, data and logs out.
COPY . .

RUN set -eux; \
    if [ "$BUILD_PROFILE" = "release" ]; then RELEASE_FLAG="--release"; OUTPUT_DIR="release"; \
    else RELEASE_FLAG=""; OUTPUT_DIR="debug"; fi; \
    if [ -n "$CARGO_FEATURES" ]; then \
        cargo build $RELEASE_FLAG --bin cf-gears-example-server --package=cf-gears-example-server --features "$CARGO_FEATURES"; \
    else \
        cargo build $RELEASE_FLAG --bin cf-gears-example-server --package=cf-gears-example-server; \
    fi; \
    cp "/build/target/$OUTPUT_DIR/cf-gears-example-server" /tmp/cf-gears-example-server; \
    rm -rf /build/target
# `rm` is part of the same RUN on purpose. A separate layer would only mask the
# directory, leaving it in the diff; deleted inside the layer that created it,
# it never enters the image at all. The builder stage is discarded anyway, but
# its layers are still written to disk during the build - ~9.5 GB of them, on
# runners where this repo already needs jlumbroso/free-disk-space elsewhere.

# Stage 2: Runtime — must match the builder's base OS.
FROM debian:13.3-slim@sha256:1d3c811171a08a5adaa4a163fbafd96b61b87aa871bbc7aa15431ac275d3d430

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

# e2e-local config uses file-parser.allowed_local_base_dir: data
RUN mkdir -p /app/data

# --chown on the COPY itself: a later `chown -R` would rewrite every byte of the
# 580 MB binary into a second layer, doubling the runtime image for nothing.
COPY --from=builder --chown=1000:1000 /tmp/cf-gears-example-server /app/cf-gears-example-server
COPY --from=builder --chown=1000:1000 /build/config /app/config

# Port that config/e2e-local.yaml binds (gears.api-gateway.config.bind_addr).
EXPOSE 8086

RUN useradd -U -u 1000 appuser && \
    chown 1000:1000 /app /app/data

# The shipped configs set `server.home_dir: "~/.cf-gears"`. A numeric USER does
# not update HOME, so it stays /root — which uid 1000 cannot write, and the
# server aborts with "Failed to create home_dir: Permission denied". Point HOME
# at /app, which is already owned by uid 1000.
ENV HOME=/app
USER 1000

# `run` is required: the CLI takes a subcommand (run | check | migrate).
CMD ["/app/cf-gears-example-server", "--config", "/app/config/e2e-local.yaml", "run"]

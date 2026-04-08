# Multi-stage build for hyperspot-server API backend
# Stage 1: Builder
FROM rust:1.92-bookworm@sha256:e90e846de4124376164ddfbaab4b0774c7bdeef5e738866295e5a90a34a307a2 AS builder

# Build arguments for cargo features
ARG CARGO_FEATURES

# Install protobuf-compiler for prost-build
RUN apt-get update && \
    apt-get install -y --no-install-recommends protobuf-compiler libprotobuf-dev && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy workspace files
COPY Cargo.toml Cargo.lock ./
COPY rust-toolchain.toml ./

# Copy all workspace members
COPY apps/hyperspot-server ./apps/hyperspot-server
COPY apps/gts-docs-validator ./apps/gts-docs-validator
COPY libs ./libs
COPY modules ./modules
COPY examples ./examples
COPY config ./config
COPY proto ./proto

# Build the hyperspot-server binary in release mode
# Using --bin to build only the specific binary
# Features can be customized via CARGO_FEATURES build arg
RUN if [ -n "$CARGO_FEATURES" ]; then \
        cargo build --release --bin hyperspot-server --package=hyperspot-server --features "$CARGO_FEATURES"; \
    else \
        cargo build --release --bin hyperspot-server --package=hyperspot-server; \
    fi

# Stage 2: Runtime - must match builder's base OS
FROM debian:13.3-slim

WORKDIR /app

# e2e-local config uses file-parser.allowed_local_base_dir: data
# Ensure it exists in container runtime working directory.
RUN mkdir -p /app/data

# Copy the built binary from builder stage
COPY --from=builder /build/target/release/hyperspot-server /app/hyperspot-server
# Copy config used in CMD
COPY --from=builder /build/config /app/config

# Expose the HTTP port for E2E tests
EXPOSE 8086

# Run with shared e2e-local config (same config path as local E2E).
RUN useradd -U -u 1000 appuser && \
    chown -R 1000:1000 /app
USER 1000
CMD ["/app/hyperspot-server", "--config", "/app/config/e2e-local.yaml"]

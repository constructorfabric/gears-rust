# Packaging-only Dockerfile — expects a pre-built binary at build context.
# Used by `make mini-chat-docker` on linux when the host cargo target is reusable.
ARG BINARY_PATH=target/debug/cf-server

FROM debian:13.3-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

ARG BINARY_PATH
COPY ${BINARY_PATH} /app/cf-server
COPY config /app/config

EXPOSE 8087

RUN useradd -U -u 1000 appuser && \
    chown -R 1000:1000 /app
USER 1000
CMD ["/app/cf-server", "--config", "/app/config/mini-chat.yaml", "run"]

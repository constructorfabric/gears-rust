# Packaging-only Dockerfile — expects a pre-built binary at build context.
# Used by `make mini-chat-docker` on linux when the host cargo target is reusable.
ARG BINARY_PATH=target/debug/cf-gears-example-server

FROM debian:13.3-slim@sha256:1d3c811171a08a5adaa4a163fbafd96b61b87aa871bbc7aa15431ac275d3d430

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

ARG BINARY_PATH
COPY ${BINARY_PATH} /app/cf-gears-example-server
COPY config /app/config

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

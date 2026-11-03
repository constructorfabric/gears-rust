# Packaging-only Dockerfile — expects a pre-built binary at build context.
# Used by `make mini-chat-docker` on linux when the host cargo target is reusable.
ARG BINARY_PATH=target/debug/cf-gears-example-server

FROM debian:13.7-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad

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

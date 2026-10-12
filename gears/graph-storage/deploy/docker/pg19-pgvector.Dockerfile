# PostgreSQL 19 with pgvector, for this gear's own test lane.
#
# The gear's baseline is "PostgreSQL 16 or later with pgvector" (ADR-0001) and
# its SQL/PGQ traversal needs 19. No published image carries both: pgvector
# gained PostgreSQL 19 support upstream in 2026-07 and `test-containers` pins a
# stock `19beta3-alpine`, on which `CREATE EXTENSION vector` fails and the
# schema migration cannot run at all. So the lane's image is built here, from
# the official base — the operator-supplied images that do carry pgvector are
# CloudNativePG operands with no `docker-entrypoint.sh`, which `test-containers`
# cannot start because it passes server arguments the operand has nothing to
# read them with.
#
# This belongs in `test-containers` once it publishes such an image; until
# then every consumer of this lane builds it, which is one `docker build` and
# no per-developer instructions beyond it:
#
#   docker build -f gears/graph-storage/docker/pg19-pgvector.Dockerfile \
#     -t pg19-pgvector:latest gears/graph-storage/docker
#   GEARS_TEST_PG_GRAPH_IMAGE=pg19-pgvector:latest make test-graph-storage-pg
#
# The extension, the base **and the server headers** are pinned. The tag is a
# beta label Docker Hub can move at any time, and a base that changes between
# the build stage and the runtime stage — or between two CI runs — gives a
# `vector.so` compiled against one set of server headers and loaded into
# another. A lane whose server moves underneath it is a lane whose failures
# nobody can date.
#
# The headers are the half that was missing, and it broke exactly as this
# paragraph describes. `postgresql-server-dev-19` came from apt unversioned
# while the base was pinned by digest, so the day pgdg published beta4 the
# extension compiled against beta4 headers and was loaded into the beta3
# server this image runs: `CREATE INDEX` then failed with "index access
# method handler function ... did not return an IndexAmRoutine struct", and
# the schema migration could not apply. Nothing in the repository had
# changed. The dev package is now pinned to the version of the server
# already installed in this very image, read out of it rather than written
# down, so the two cannot drift apart and refreshing the base needs no second
# edit here.
# Refresh with: docker buildx imagetools inspect postgres:19beta3
ARG PG_BASE=postgres:19beta3@sha256:a48b19841e04b35b72a25e9a94314ac80546d32b5e2e3cd9279390cbd8a99572
FROM ${PG_BASE} AS build
ARG PGVECTOR_REF=5219575
RUN set -eux; \
    apt-get update; \
    PG_SERVER_VERSION="$(dpkg-query -W -f='${Version}' postgresql-19)"; \
    apt-get install -y --no-install-recommends \
        build-essential ca-certificates git \
        "postgresql-server-dev-19=${PG_SERVER_VERSION}"; \
    git clone https://github.com/pgvector/pgvector.git /tmp/pgvector; \
    cd /tmp/pgvector; \
    git checkout "${PGVECTOR_REF}"; \
    make OPTFLAGS=""; \
    make install

# The runtime image is the official one plus the built extension: the
# entrypoint, the initdb behaviour and the server arguments are all the stock
# ones, which is what `test-containers` drives.
FROM ${PG_BASE}
COPY --from=build /usr/share/postgresql/19/extension/vector* /usr/share/postgresql/19/extension/
COPY --from=build /usr/lib/postgresql/19/lib/vector.so /usr/lib/postgresql/19/lib/

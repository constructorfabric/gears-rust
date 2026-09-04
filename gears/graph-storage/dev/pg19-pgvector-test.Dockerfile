# PostgreSQL 19 beta 3 + pgvector on the OFFICIAL image (docker-entrypoint kept),
# for the gear's test-containers lane. The studio image is a CNPG operand and
# cannot serve that lane (D-003).
FROM postgres:19beta3 AS build
ARG PGVECTOR_REF=5219575
RUN set -eux; apt-get update; apt-get install -y --no-install-recommends build-essential ca-certificates git postgresql-server-dev-19; \
    git clone https://github.com/pgvector/pgvector.git /tmp/pgvector; cd /tmp/pgvector; git checkout "${PGVECTOR_REF}"; \
    make OPTFLAGS=""; make install
FROM postgres:19beta3
COPY --from=build /usr/share/postgresql/19/extension/vector* /usr/share/postgresql/19/extension/
COPY --from=build /usr/lib/postgresql/19/lib/vector.so /usr/lib/postgresql/19/lib/

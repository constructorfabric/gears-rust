-- The plugin's own tables (docs/DESIGN.md §2.1). No IF NOT EXISTS: a schema
-- that drifted from the recorded version fails startup loudly.

CREATE TABLE evbk__log (                             -- one row per provisioned topic
    topic_uuid     UUID          PRIMARY KEY,         -- GtsId::to_uuid(topic)
    topic_id       VARCHAR(1024) NOT NULL,
    log_id         UUID          NOT NULL,            -- feature 0006
    partitions     INTEGER       NOT NULL,            -- at most 9999 (table naming)
    max_age        INTERVAL,                          -- NULL = unbounded
    max_bytes      BIGINT,                            -- per partition; NULL = unbounded
    provisioned_at TIMESTAMPTZ   NOT NULL
);

CREATE TABLE evbk__partition (                       -- one row per (topic, partition); append writes it
    topic_uuid     UUID    NOT NULL REFERENCES evbk__log,
    partition      INTEGER NOT NULL,
    last_sequence  BIGINT  NOT NULL DEFAULT 0,        -- highest ever assigned; 0 = nothing yet
    appended_bytes BIGINT  NOT NULL DEFAULT 0,        -- cumulative, never lowered
    PRIMARY KEY (topic_uuid, partition)
);

CREATE TABLE evbk__retention (                       -- one row per (topic, partition); maintenance writes it
    topic_uuid     UUID    NOT NULL,
    partition      INTEGER NOT NULL,
    floor          BIGINT  NOT NULL DEFAULT 0,        -- highest removed sequence; stored = (floor, last_sequence]
    removed_bytes  BIGINT  NOT NULL DEFAULT 0,        -- cumulative, never lowered
    PRIMARY KEY (topic_uuid, partition),
    FOREIGN KEY (topic_uuid, partition) REFERENCES evbk__partition
);

CREATE TABLE evbk__fence (                           -- deduplication, one row per fence key
    fence_key      UUID    PRIMARY KEY,               -- FenceKey's 16 bytes, opaque
    last_seq       BIGINT  NOT NULL                   -- last applied FenceSeq
);

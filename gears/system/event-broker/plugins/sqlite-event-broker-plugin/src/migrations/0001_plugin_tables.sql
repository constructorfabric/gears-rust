-- The plugin's tables (docs/DESIGN.md §2). No IF NOT EXISTS: a file whose
-- schema drifted from the recorded version fails startup loudly.
--
-- SQLite does not enforce a VARCHAR length, so each bounded string carries a
-- CHECK of its own; ids are 16-byte BLOBs, times INTEGER microseconds since
-- the Unix epoch, UTC.

-- Queries:
--   provision: INSERT ... ON CONFLICT (topic_uuid) DO NOTHING; SELECT log_id, partitions;
--     UPDATE log_id, max_age_ms, max_bytes - one write transaction
--   topic lookup, once per topic per instance (cached): SELECT partitions WHERE topic_uuid = ?
--   sweep, every pass: joined from evbk__partition for the bounds
CREATE TABLE evbk__log (                                   -- one row per provisioned topic
    topic_uuid     BLOB          NOT NULL PRIMARY KEY CHECK (length(topic_uuid) = 16),  -- GtsId::to_uuid(topic)
    topic_id       VARCHAR(1024) NOT NULL CHECK (length(topic_id) <= 1024),          -- gts_id::GTS_ID_MAX_LENGTH
    log_id         BLOB          NOT NULL CHECK (length(log_id) = 16),               -- feature 0006
    partitions     INTEGER       NOT NULL CHECK (partitions > 0),
    max_age_ms     INTEGER,                                -- NULL = unbounded
    max_bytes      INTEGER,                                -- per partition, event bodies; NULL = unbounded
    provisioned_at INTEGER       NOT NULL
) WITHOUT ROWID;

-- Queries:
--   append, once per partition it touches, under the write lock:
--     UPDATE ... SET hwm = hwm + ?, bytes = bytes + ?, head_time = MAX(head_time, ?) RETURNING hwm, head_time
--   resolve: SELECT hwm, floor; watch: SELECT hwm; query: SELECT floor, hwm
--   sweep: SELECT over every bounded partition with floor < hwm; UPDATE floor, bytes
CREATE TABLE evbk__partition (                             -- one row per (topic, partition)
    topic_uuid BLOB    NOT NULL REFERENCES evbk__log (topic_uuid),
    partition  INTEGER NOT NULL,
    hwm        INTEGER NOT NULL DEFAULT 0,                 -- highest sequence ever assigned; 0 = none
    floor      INTEGER NOT NULL DEFAULT 0,                 -- highest sequence removed; 0 = none
    bytes      INTEGER NOT NULL DEFAULT 0,                 -- body bytes of the events in (floor, hwm]
    head_time  INTEGER NOT NULL DEFAULT 0,                 -- sequence_time of the event at hwm
    PRIMARY KEY (topic_uuid, partition),
    CHECK (0 <= floor AND floor <= hwm)
) WITHOUT ROWID;

-- Queries:
--   append, first in its transaction:
--     INSERT ... ON CONFLICT (fence_key) DO UPDATE SET last_seq = excluded.last_seq WHERE last_seq < excluded.last_seq
CREATE TABLE evbk__fence (                                 -- one row per fence key
    fence_key BLOB    NOT NULL PRIMARY KEY CHECK (length(fence_key) = 16),
    last_seq  INTEGER NOT NULL
) WITHOUT ROWID;

-- A rowid table, not WITHOUT ROWID: bodies are often larger than the twentieth
-- of a page a WITHOUT ROWID row should stay under, and the key is kept in the
-- primary key's own index instead.
--
-- Queries:
--   append: INSERT one row per event, multi-row
--   read: WHERE topic_uuid = ? AND partition = ? AND sequence > ? ORDER BY sequence LIMIT ?   (primary key)
--   resolve(At), sweep by age: WHERE ... AND sequence_time >= / < ? ORDER BY sequence_time ... LIMIT 1   (evbk__event_time)
--   sweep: SELECT sequence, size ... ORDER BY sequence LIMIT ?; SUM(size) and DELETE over (floor, cut]
--   query: MIN/MAX over a sequence range   (primary key)
CREATE TABLE evbk__event (
    topic_uuid    BLOB    NOT NULL,
    partition     INTEGER NOT NULL,
    sequence      INTEGER NOT NULL,                        -- from 1, gapless per partition
    sequence_time INTEGER NOT NULL,                        -- when stored; never falls with sequence
    tenant_id     BLOB    NOT NULL,
    size          INTEGER NOT NULL,                        -- length(body), what max_bytes counts
    body          BLOB    NOT NULL,                        -- versioned JSON (docs/DESIGN.md §2.3)
    PRIMARY KEY (topic_uuid, partition, sequence)
);

-- Covers the age cut and resolve(At) without touching the table.
CREATE INDEX evbk__event_time ON evbk__event (topic_uuid, partition, sequence_time, sequence);

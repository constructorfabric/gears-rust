-- The plugin's own tables (docs/DESIGN.md §2.1). No IF NOT EXISTS: a database
-- that drifted from the recorded version fails startup loudly. No foreign
-- keys: InnoDB would take shared locks on the parent rows the appends lock.

-- Queries:
--   provision, once per provision call, in one transaction:
--     INSERT IGNORE INTO evbk__log (...) VALUES (...)
--     SELECT log_id, partitions FROM evbk__log WHERE topic_uuid = ? FOR UPDATE
--     UPDATE evbk__log SET log_id = ?, max_age_ms = ?, max_bytes = ? WHERE topic_uuid = ?
--   topic lookup, once per topic per process (cached), on any operation:
--     SELECT topic_id, partitions FROM evbk__log WHERE topic_uuid = ?
--   joined by the maintenance (per run) and the rescan (full scan) for the bounds (see evbk__partition)
CREATE TABLE evbk__log (                               -- one row per provisioned topic
    topic_uuid     BINARY(16)    NOT NULL PRIMARY KEY,  -- GtsId::to_uuid(topic)
    topic_id       VARCHAR(1024) NOT NULL,
    log_id         BINARY(16)    NOT NULL,              -- feature 0006
    partitions     INT           NOT NULL,              -- at most 9999 (table naming)
    max_age_ms     BIGINT        NULL,                  -- NULL = unbounded
    max_bytes      BIGINT        NULL,                  -- per partition; NULL = unbounded
    provisioned_at DATETIME(6)   NOT NULL               -- UTC
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4 COLLATE = utf8mb4_bin;

-- Queries:
--   append, every append, once per partition it touches, ascending; the row
--   lock is held until commit and orders the partition's appends:
--     UPDATE evbk__partition SET hwm = hwm + ?, bytes = bytes + ? WHERE topic_uuid = ? AND partition_no = ?
--     SELECT p.hwm, p.bytes, UTC_TIMESTAMP(6), (SELECT e.seq_time FROM {table} e WHERE e.seq = p.hwm - ?)
--      FROM evbk__partition p WHERE p.topic_uuid = ? AND p.partition_no = ?
--   provision, in its transaction, after the evbk__log row lock:
--     SELECT partition_no, table_name FROM evbk__partition WHERE topic_uuid = ? ORDER BY partition_no
--     INSERT INTO evbk__partition (topic_uuid, partition_no, table_name) VALUES (?, ?, ?)   -- per partition with no row
--   topic lookup, once per topic per process:
--     SELECT partition_no, table_name FROM evbk__partition WHERE topic_uuid = ? ORDER BY partition_no
--   resolve, every resolve:
--     SELECT p.hwm, r.floor_seq FROM evbk__partition p JOIN evbk__retention r ... WHERE p.topic_uuid = ? AND p.partition_no = ?
--   maintenance, once per run, under the partition's named lock:
--     SELECT p.table_name, p.hwm, p.bytes, r.floor_seq, r.removed_bytes, l.max_age_ms, l.max_bytes, UTC_TIMESTAMP(6)
--      FROM evbk__partition p JOIN evbk__retention r ... JOIN evbk__log l ... WHERE p.topic_uuid = ? AND p.partition_no = ?
--   watch, once when a watch starts and every poll_interval while any is open, at most 500 pairs per query:
--     SELECT topic_uuid, partition_no, hwm FROM evbk__partition WHERE (topic_uuid, partition_no) IN ((?, ?), ...)
--   rescan, every rescan_interval; full scan:
--     SELECT p.topic_uuid, p.partition_no, p.hwm, p.bytes, r.removed_bytes, l.max_bytes,
--      UTC_TIMESTAMP(6) - INTERVAL l.max_age_ms * 1000 MICROSECOND FROM evbk__partition p JOIN evbk__retention r ... JOIN evbk__log l ...
CREATE TABLE evbk__partition (                         -- one row per (topic, partition); append writes it
    topic_uuid     BINARY(16)  NOT NULL,
    partition_no   INT         NOT NULL,
    table_name     VARCHAR(64) NOT NULL,                -- the event table, as created; authoritative
    hwm            BIGINT      NOT NULL DEFAULT 0,      -- highest ever assigned; 0 = nothing yet
    bytes          BIGINT      NOT NULL DEFAULT 0,      -- weight of every event ever appended (segment::weight)
    PRIMARY KEY (topic_uuid, partition_no),
    UNIQUE KEY uq_evbk__partition__table (table_name)
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4 COLLATE = utf8mb4_bin;

-- Queries:
--   provision, in its transaction, per partition with no evbk__partition row:
--     INSERT INTO evbk__retention (topic_uuid, partition_no) VALUES (?, ?)
--   retention, once per run that expires segments, in one transaction with the segment rows:
--     UPDATE evbk__retention SET floor_seq = ?, removed_bytes = removed_bytes + ? WHERE topic_uuid = ? AND partition_no = ?
--   read and query, every call, as a subquery of their scan of the event table:
--     SELECT floor_seq FROM evbk__retention WHERE topic_uuid = ? AND partition_no = ?
--   read by resolve, the maintenance and the rescan (see evbk__partition)
CREATE TABLE evbk__retention (                         -- one row per (topic, partition); retention writes it
    topic_uuid     BINARY(16) NOT NULL,
    partition_no   INT        NOT NULL,
    floor_seq      BIGINT     NOT NULL DEFAULT 0,       -- highest removed sequence; stored = (floor_seq, hwm]
    removed_bytes  BIGINT     NOT NULL DEFAULT 0,       -- weight of the expired segments; stored = bytes - removed_bytes
    PRIMARY KEY (topic_uuid, partition_no)
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4 COLLATE = utf8mb4_bin;

-- The life of one segment (docs/DESIGN.md §4.9). DDL commits on its own, so
-- each DDL is bracketed by catalog writes: `adding` before ADD PARTITION (or
-- CREATE TABLE) and `open` after it; `expired` with the floor, and the row
-- deleted after DROP PARTITION. A crash between leaves a row the settle step
-- of the next maintenance run puts right from information_schema.PARTITIONS.
--
-- Queries, all under the partition's named lock but the reads:
--   extend (provision's first run included), once per run that adds segments, around its DDL:
--     INSERT INTO evbk__segment (...) VALUES (?, ?, ?, ?, ?, 'adding', UTC_TIMESTAMP(6)), ...
--     UPDATE evbk__segment SET state = 'open', changed_at = ... WHERE ... AND first_seq IN (?, ...)
--   maintenance, twice per run (before and after settle):
--     SELECT first_seq, end_seq, segment_name, CAST(state AS CHAR), bytes, newest_at FROM evbk__segment
--      WHERE topic_uuid = ? AND partition_no = ? ORDER BY first_seq
--   settle, per row it puts right:
--     UPDATE evbk__segment SET state = 'open' ... WHERE ... AND first_seq IN (?)   -- an adding row the server holds
--     DELETE FROM evbk__segment WHERE ... AND first_seq = ?                         -- adding or expired, not held
--     INSERT INTO evbk__segment (...) VALUES (..., 'open', ...)                     -- a held partition with no row
--   seal, once per full segment:
--     UPDATE evbk__segment SET state = 'sealed', bytes = ?, newest_at = ?, changed_at = ... WHERE ... AND first_seq = ?
--   retain, once per run that expires segments, in one transaction with the floor:
--     UPDATE evbk__segment SET state = 'expired', changed_at = ... WHERE ... AND first_seq IN (?, ...)
--   drop, once per segment, after its DROP PARTITION:
--     DELETE FROM evbk__segment WHERE ... AND first_seq = ?
--   resolve(At), as a subquery, for where to start:
--     SELECT COALESCE(MAX(end_seq - 1), 0) FROM evbk__segment WHERE ... AND state = 'sealed' AND newest_at < ?
--   query, every query that matches events:
--     SELECT segment_name, first_seq, end_seq FROM evbk__segment WHERE ... AND state IN ('open', 'sealed')
--   rescan, every rescan_interval; full scan:
--     SELECT topic_uuid, partition_no, first_seq, end_seq, segment_name, CAST(state AS CHAR), bytes, newest_at
--      FROM evbk__segment ORDER BY topic_uuid, partition_no, first_seq
CREATE TABLE evbk__segment (                           -- one row per segment, created and removed with it
    topic_uuid     BINARY(16)  NOT NULL,
    partition_no   INT         NOT NULL,
    first_seq      BIGINT      NOT NULL,                -- [first_seq, end_seq): the partition's range
    end_seq        BIGINT      NOT NULL,                -- the partition's VALUES LESS THAN
    segment_name   VARCHAR(64) NOT NULL,                -- the range partition, as created; authoritative
    state          ENUM('adding', 'open', 'sealed', 'expired') NOT NULL,
    bytes          BIGINT      NOT NULL DEFAULT 0,      -- weight of its events when sealed; 0 until then
    newest_at      DATETIME(6) NULL,                    -- its last event's seq_time when sealed; NULL until then
    changed_at     DATETIME(6) NOT NULL,                -- last change of any column, UTC
    PRIMARY KEY (topic_uuid, partition_no, first_seq)
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4 COLLATE = utf8mb4_bin;

-- Queries:
--   append, every append, first; the row lock is held until commit, so a
--   retry of an open call waits for it and then sees what it committed:
--     SELECT last_seq FROM evbk__fence WHERE fence_key = ? FOR UPDATE
--     INSERT INTO evbk__fence (fence_key, last_seq) VALUES (?, ?)      -- no row yet
--     UPDATE evbk__fence SET last_seq = ? WHERE fence_key = ?          -- a row below the seq
--     SELECT last_seq ... FOR UPDATE again                              -- the INSERT met a concurrent first row
CREATE TABLE evbk__fence (                             -- deduplication, one row per fence key
    fence_key      BINARY(16) NOT NULL PRIMARY KEY,     -- FenceKey's 16 bytes, opaque
    last_seq       BIGINT     NOT NULL                  -- last applied FenceSeq
) ENGINE = InnoDB;

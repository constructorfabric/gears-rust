-- The plugin's own tables (docs/DESIGN.md §2.1). No IF NOT EXISTS: a schema
-- that drifted from the recorded version fails startup loudly.

-- Queries:
--   provision, once per provision call, in one transaction:
--     INSERT INTO evbk__log (...) VALUES (...) ON CONFLICT (topic_uuid) DO NOTHING
--     SELECT log_id, partitions FROM evbk__log WHERE topic_uuid = $1 FOR UPDATE
--     UPDATE evbk__log SET log_id = $2, max_age = $3, max_bytes = $4 WHERE topic_uuid = $1
--   topic lookup, once per topic per process (cached), on any operation:
--     SELECT l.topic_id, l.partitions, ARRAY(SELECT p.table_name FROM evbk__partition p
--      WHERE p.topic_uuid = l.topic_uuid ORDER BY p.partition) FROM evbk__log l WHERE l.topic_uuid = $1
--   joined by the listener's republish, the extender and the rescan (see evbk__partition)
CREATE TABLE evbk__log (                             -- one row per provisioned topic
    topic_uuid     UUID          PRIMARY KEY,         -- GtsId::to_uuid(topic)
    topic_id       VARCHAR(1024) NOT NULL,
    log_id         UUID          NOT NULL,            -- feature 0006
    partitions     INTEGER       NOT NULL,            -- at most 9999 (table naming)
    max_age        INTERVAL,                          -- NULL = unbounded
    max_bytes      BIGINT,                            -- per partition; NULL = unbounded
    provisioned_at TIMESTAMPTZ   NOT NULL
);

-- Queries:
--   append, every append, once per partition it touches, ascending;
--   the row lock is held until commit and orders the partition's appends:
--     UPDATE evbk__partition SET hwm = hwm + $3, appended_bytes = appended_bytes + $4
--      WHERE topic_uuid = $1 AND partition = $2 RETURNING hwm, table_name, clock_timestamp()
--   provision, once per provision call, then once per partition:
--     SELECT partition, table_name FROM evbk__partition WHERE topic_uuid = $1
--     INSERT INTO evbk__partition (topic_uuid, partition, table_name) VALUES (...) ON CONFLICT DO NOTHING
--   resolve, every resolve, and once per partition when a watch starts:
--     SELECT p.hwm, r.floor FROM evbk__partition p JOIN evbk__retention r USING (topic_uuid, partition)
--      WHERE p.topic_uuid = $1 AND p.partition = $2
--   republish, on every LISTEN connect and reconnect; full scan:
--     SELECT l.topic_id, p.partition, p.hwm FROM evbk__partition p JOIN evbk__log l USING (topic_uuid)
--   extender, once per run, under the partition's advisory lock:
--     SELECT p.hwm, p.table_name, l.max_age IS NOT NULL OR l.max_bytes IS NOT NULL FROM evbk__partition p
--      JOIN evbk__log l USING (topic_uuid) WHERE p.topic_uuid = $1 AND p.partition = $2
--   counter, once per run, first in its transaction:
--     SELECT p.hwm, p.table_name, l.max_age IS NOT NULL OR l.max_bytes IS NOT NULL,
--      GREATEST(COALESCE(p.appended_bytes - r.removed_bytes - l.max_bytes, 0), 0)
--      FROM evbk__partition p JOIN evbk__log l USING (topic_uuid) JOIN evbk__retention r USING (topic_uuid, partition)
--      WHERE p.topic_uuid = $1 AND p.partition = $2
--   retention: joined under its row lock (see evbk__retention)
--   detacher, once per step: joined for the parent's name (see evbk__segment)
--   reconcile, once per run, under the partition's advisory lock:
--     SELECT p.hwm, r.floor, l.max_age IS NOT NULL OR l.max_bytes IS NOT NULL
--      FROM evbk__partition p JOIN evbk__retention r USING (topic_uuid, partition)
--      JOIN evbk__log l USING (topic_uuid) WHERE p.topic_uuid = $1 AND p.partition = $2
--   rescan, every rescan_interval, in one read-only snapshot; full scan:
--     SELECT p.topic_uuid, p.partition, p.hwm, r.floor, l.max_age IS NOT NULL OR l.max_bytes IS NOT NULL,
--      GREATEST(COALESCE(p.appended_bytes - r.removed_bytes - l.max_bytes, 0), 0)
--      FROM evbk__partition p JOIN evbk__log l USING (topic_uuid) JOIN evbk__retention r USING (topic_uuid, partition)
CREATE TABLE evbk__partition (                       -- one row per (topic, partition); append writes it
    topic_uuid     UUID    NOT NULL REFERENCES evbk__log,
    partition      INTEGER     NOT NULL,
    table_name     VARCHAR(63) NOT NULL UNIQUE,       -- the partitioned table, as created; authoritative
    hwm            BIGINT      NOT NULL DEFAULT 0,    -- highest ever assigned; 0 = nothing yet
    appended_bytes BIGINT      NOT NULL DEFAULT 0,    -- cumulative, never lowered
    PRIMARY KEY (topic_uuid, partition)
);

-- Queries:
--   provision, once per partition:
--     INSERT INTO evbk__retention (topic_uuid, partition) VALUES (...) ON CONFLICT DO NOTHING
--   retention, once per run, first in its transaction; 55P03 = another instance holds it:
--     SELECT l.max_age IS NOT NULL, l.max_bytes, p.appended_bytes - r.removed_bytes
--      FROM evbk__retention r JOIN evbk__partition p USING (topic_uuid, partition) JOIN evbk__log l USING (topic_uuid)
--      WHERE r.topic_uuid = $1 AND r.partition = $2 FOR UPDATE OF r NOWAIT
--   retention, once per run that expires segments, in the same transaction:
--     UPDATE evbk__retention SET floor = $3, removed_bytes = removed_bytes + $4 WHERE topic_uuid = $1 AND partition = $2
--   read, every read and every query, as a subquery of its scan of the partition's table:
--     SELECT floor FROM evbk__retention WHERE topic_uuid = $3 AND partition = $4
--   read by resolve, the counter, reconcile and the rescan (see evbk__partition)
CREATE TABLE evbk__retention (                       -- one row per (topic, partition); retention writes it
    topic_uuid     UUID    NOT NULL,
    partition      INTEGER NOT NULL,
    floor          BIGINT  NOT NULL DEFAULT 0,        -- highest removed sequence; stored = (floor, hwm]
    removed_bytes  BIGINT  NOT NULL DEFAULT 0,        -- cumulative, never lowered
    PRIMARY KEY (topic_uuid, partition),
    FOREIGN KEY (topic_uuid, partition) REFERENCES evbk__partition
);

-- The life of one segment (docs/DESIGN.md §4.9): the extender creates it
-- `open`, the counter seals it once counted to its end, retention expires it
-- with the floor, and the detacher detaches and drops it.
CREATE TYPE evbk__segment_state AS ENUM ('open', 'sealed', 'expired', 'detaching');

-- Queries:
--   provision, once per new partition table, in its transaction:
--     INSERT INTO evbk__segment (...) VALUES (..., 'open', first_seq - 1, now()), ...
--   extender, once per run, under the partition's advisory lock, in one transaction:
--     SELECT first_seq, end_seq, state, counted_to, newest_at FROM evbk__segment
--      WHERE topic_uuid = $1 AND partition = $2 AND state IN ('open', 'sealed') ORDER BY first_seq
--     INSERT INTO evbk__segment (...) VALUES ($1, $2, $3, $4, $5, 'open', $3 - 1, now())   -- per segment added
--   counter, once per run, in one transaction; another instance's rows are skipped:
--     SELECT first_seq, end_seq, counted_to FROM evbk__segment
--      WHERE topic_uuid = $1 AND partition = $2 AND state = 'open' AND counted_to < $3
--      ORDER BY first_seq FOR UPDATE SKIP LOCKED
--     UPDATE evbk__segment SET bytes = bytes + $4, counted_to = $5, newest_at = COALESCE($6, newest_at),
--      changed_at = now(), state = CASE WHEN $5 = end_seq - 1 THEN 'sealed' ELSE state END
--      WHERE topic_uuid = $1 AND partition = $2 AND first_seq = $3 RETURNING state   -- per segment counted
--     then the extender's SELECT of the open and sealed rows, for the registry
--   retention, once per run, under its evbk__retention row lock, in one transaction:
--     SELECT s.first_seq, s.end_seq, s.bytes, COALESCE(s.newest_at < now() - l.max_age, false)
--      FROM evbk__segment s JOIN evbk__log l USING (topic_uuid)
--      WHERE s.topic_uuid = $1 AND s.partition = $2 AND s.state = 'sealed' ORDER BY s.first_seq LIMIT 64
--     UPDATE evbk__segment SET state = 'expired', changed_at = now()
--      WHERE topic_uuid = $1 AND partition = $2 AND first_seq = ANY($3)
--   detacher, once per step, on its own connection, under the partition's session lock:
--     SELECT s.first_seq, s.state, s.table_name, p.table_name FROM evbk__segment s JOIN evbk__partition p
--      USING (topic_uuid, partition) WHERE s.topic_uuid = $1 AND s.partition = $2
--      AND s.state IN ('expired', 'detaching') ORDER BY s.first_seq LIMIT 1
--     UPDATE evbk__segment SET state = 'detaching', changed_at = now()
--      WHERE topic_uuid = $1 AND partition = $2 AND first_seq = $3   -- before DETACH ... CONCURRENTLY
--     DELETE FROM evbk__segment WHERE topic_uuid = $1 AND partition = $2 AND first_seq = $3   -- with DROP TABLE
--   rescan, every rescan_interval, in one read-only snapshot; full scan:
--     SELECT topic_uuid, partition, first_seq, end_seq, state, counted_to, newest_at FROM evbk__segment
--      ORDER BY topic_uuid, partition, first_seq
--   cross-check, every crosscheck_every rescans, in the rescan's snapshot; full scan, and per
--   partition in reconcile under the advisory lock:
--     SELECT topic_uuid, partition, first_seq, end_seq, state, table_name FROM evbk__segment
--      [WHERE topic_uuid = $1 AND partition = $2]
--     SELECT table_name FROM evbk__segment ...   -- the catalog query's names (docs/DESIGN.md §4.9)
--   reconcile, once per repair, under the partition's advisory lock:
--     DELETE FROM evbk__segment WHERE table_name = $1                     -- an open row above the head, its table gone
--     INSERT INTO evbk__segment (...) VALUES ($1, $2, $3, $4, $5, 'open', $3 - 1, now())   -- adopting a table
CREATE TABLE evbk__segment (                         -- one row per segment table, created and removed with it
    topic_uuid     UUID                NOT NULL,
    partition      INTEGER             NOT NULL,
    first_seq      BIGINT              NOT NULL,      -- [first_seq, end_seq): the table's bound
    end_seq        BIGINT              NOT NULL,
    table_name     VARCHAR(63)         NOT NULL UNIQUE,  -- the segment table, as created; authoritative
    state          evbk__segment_state NOT NULL,
    bytes          BIGINT              NOT NULL DEFAULT 0,  -- SUM(stored_bytes) of (first_seq - 1, counted_to]
    counted_to     BIGINT              NOT NULL,      -- last sequence summed; first_seq - 1 when new
    newest_at      TIMESTAMPTZ,                       -- sequence_time at counted_to; NULL until counted
    changed_at     TIMESTAMPTZ         NOT NULL,      -- last change of any column
    PRIMARY KEY (topic_uuid, partition, first_seq),
    FOREIGN KEY (topic_uuid, partition) REFERENCES evbk__partition
);

-- Queries:
--   append, every append, as its first statement; the row lock is held until
--   commit, so a retry of an open call waits for it; 0 rows = Duplicate:
--     INSERT INTO evbk__fence (fence_key, last_seq) VALUES ($1, $2)
--     ON CONFLICT (fence_key) DO UPDATE SET last_seq = EXCLUDED.last_seq
--     WHERE evbk__fence.last_seq < EXCLUDED.last_seq
CREATE TABLE evbk__fence (                           -- deduplication, one row per fence key
    fence_key      UUID    PRIMARY KEY,               -- FenceKey's 16 bytes, opaque
    last_seq       BIGINT  NOT NULL                   -- last applied FenceSeq
);

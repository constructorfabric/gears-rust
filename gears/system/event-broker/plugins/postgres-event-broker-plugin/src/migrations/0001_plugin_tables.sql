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
--   joined by the listener's republish, the sealer, retention and the rescan (see evbk__partition, evbk__segment)
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
--     UPDATE evbk__partition SET hwm = hwm + $3 WHERE topic_uuid = $1 AND partition = $2
--      RETURNING hwm, table_name, clock_timestamp()
--   provision, once per provision call, then once per partition:
--     SELECT partition, table_name FROM evbk__partition WHERE topic_uuid = $1
--     INSERT INTO evbk__partition (topic_uuid, partition, table_name) VALUES (...) ON CONFLICT DO NOTHING
--   resolve, every resolve:
--     SELECT p.hwm, r.floor FROM evbk__partition p JOIN evbk__retention r USING (topic_uuid, partition)
--      WHERE p.topic_uuid = $1 AND p.partition = $2
--   watch, once when a watch starts, for every partition it names:
--     SELECT p.hwm FROM UNNEST($1::uuid[], $2::int[]) WITH ORDINALITY AS w(topic_uuid, partition, n)
--      JOIN evbk__partition p USING (topic_uuid, partition) ORDER BY w.n
--   republish, on every LISTEN connect and reconnect; full scan:
--     SELECT l.topic_id, p.partition, p.hwm FROM evbk__partition p JOIN evbk__log l USING (topic_uuid)
--   extender, once per run, under the partition's advisory lock:
--     SELECT hwm, table_name FROM evbk__partition WHERE topic_uuid = $1 AND partition = $2
--   sealer, once per run, first in its transaction:
--     SELECT p.hwm, l.max_age IS NOT NULL OR l.max_bytes IS NOT NULL
--      FROM evbk__partition p JOIN evbk__log l USING (topic_uuid) WHERE p.topic_uuid = $1 AND p.partition = $2
--   detacher, once per step: joined for the parent's name (see evbk__segment)
--   reconcile, once per run, under the partition's advisory lock:
--     SELECT p.hwm, r.floor FROM evbk__partition p JOIN evbk__retention r USING (topic_uuid, partition)
--      WHERE p.topic_uuid = $1 AND p.partition = $2
--   rescan, every rescan_interval, in one read-only snapshot; full scan:
--     SELECT p.topic_uuid, p.partition, p.hwm, r.floor, l.max_bytes IS NOT NULL, now() - l.max_age
--      FROM evbk__partition p JOIN evbk__log l USING (topic_uuid) JOIN evbk__retention r USING (topic_uuid, partition)
CREATE TABLE evbk__partition (                       -- one row per (topic, partition); append writes it
    topic_uuid     UUID    NOT NULL REFERENCES evbk__log,
    partition      INTEGER     NOT NULL,
    table_name     VARCHAR(63) NOT NULL UNIQUE,       -- the partitioned table, as created; authoritative
    hwm            BIGINT      NOT NULL DEFAULT 0,    -- highest ever assigned; 0 = nothing yet
    PRIMARY KEY (topic_uuid, partition)
);

-- Queries:
--   provision, once per partition:
--     INSERT INTO evbk__retention (topic_uuid, partition) VALUES (...) ON CONFLICT DO NOTHING
--   retention, once per run, first in its transaction; 55P03 = another instance holds it:
--     SELECT l.max_age IS NOT NULL, l.max_bytes FROM evbk__retention r JOIN evbk__log l USING (topic_uuid)
--      WHERE r.topic_uuid = $1 AND r.partition = $2 FOR UPDATE OF r NOWAIT
--   retention, once per run that expires segments, in the same transaction:
--     UPDATE evbk__retention SET floor = $3 WHERE topic_uuid = $1 AND partition = $2
--   read, every read and every query, as a subquery of its scan of the partition's table:
--     SELECT floor FROM evbk__retention WHERE topic_uuid = $3 AND partition = $4
--   read by resolve, reconcile and the rescan (see evbk__partition)
CREATE TABLE evbk__retention (                       -- one row per (topic, partition); retention writes it
    topic_uuid     UUID    NOT NULL,
    partition      INTEGER NOT NULL,
    floor          BIGINT  NOT NULL DEFAULT 0,        -- highest removed sequence; stored = (floor, hwm]
    PRIMARY KEY (topic_uuid, partition),
    FOREIGN KEY (topic_uuid, partition) REFERENCES evbk__partition
);

-- The life of one segment (docs/DESIGN.md §4.9): the extender creates it
-- `open`, the sealer seals it once full with its table's size, retention
-- expires it with the floor, and the detacher detaches and drops it.
CREATE TYPE evbk__segment_state AS ENUM ('open', 'sealed', 'expired', 'detaching');

-- Queries:
--   provision, once per new partition table, in its transaction:
--     INSERT INTO evbk__segment (...) VALUES (..., 'open', now()), ...
--   extender, once per run, under the partition's advisory lock, in one transaction:
--     SELECT first_seq, end_seq, state, bytes, newest_at FROM evbk__segment
--      WHERE topic_uuid = $1 AND partition = $2 AND state IN ('open', 'sealed') ORDER BY first_seq
--     INSERT INTO evbk__segment (...) VALUES ($1, $2, $3, $4, $5, 'open', now())   -- per segment added
--   sealer, once per run, in one transaction; another instance's rows are skipped:
--     SELECT first_seq, table_name FROM evbk__segment
--      WHERE topic_uuid = $1 AND partition = $2 AND state = 'open' AND end_seq - 1 <= $3
--      ORDER BY first_seq FOR UPDATE SKIP LOCKED
--     SELECT pg_total_relation_size('{segment}'::regclass),
--      (SELECT sequence_time FROM {segment} ORDER BY sequence DESC LIMIT 1)   -- per full segment
--     UPDATE evbk__segment SET state = 'sealed', bytes = $4, newest_at = $5, changed_at = now()
--      WHERE topic_uuid = $1 AND partition = $2 AND first_seq = $3          -- per full segment
--     then the extender's SELECT of the open and sealed rows, for the registry
--   retention, once per run, under its evbk__retention row lock, in one transaction:
--     SELECT COALESCE(SUM(CASE WHEN state = 'open' THEN pg_total_relation_size(to_regclass(table_name))
--      ELSE bytes END), 0) FROM evbk__segment
--      WHERE topic_uuid = $1 AND partition = $2 AND state IN ('open', 'sealed')   -- with a byte bound only
--     SELECT s.first_seq, s.end_seq, s.bytes,
--      l.max_age IS NOT NULL AND (s.newest_at IS NULL OR s.newest_at < now() - l.max_age)
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
--     SELECT topic_uuid, partition, first_seq, end_seq, state, bytes, newest_at FROM evbk__segment
--      ORDER BY topic_uuid, partition, first_seq
--   resolve, every resolve(At), as a subquery of its scan of the partition's table, for where to start:
--     SELECT COALESCE(MAX(end_seq - 1), 0) FROM evbk__segment
--      WHERE topic_uuid = $3 AND partition = $4 AND state = 'sealed' AND newest_at < $1
--   cross-check, every crosscheck_every rescans, in the rescan's snapshot; full scan, and per
--   partition in reconcile under the advisory lock:
--     SELECT topic_uuid, partition, first_seq, end_seq, state, table_name FROM evbk__segment
--      [WHERE topic_uuid = $1 AND partition = $2]
--     SELECT table_name FROM evbk__segment ...   -- the catalog query's names (docs/DESIGN.md §4.9)
--   reconcile, once per repair, under the partition's advisory lock:
--     DELETE FROM evbk__segment WHERE table_name = $1                     -- an open row above the head, its table gone
--     INSERT INTO evbk__segment (...) VALUES ($1, $2, $3, $4, $5, 'open', now())   -- adopting a table
CREATE TABLE evbk__segment (                         -- one row per segment table, created and removed with it
    topic_uuid     UUID                NOT NULL,
    partition      INTEGER             NOT NULL,
    first_seq      BIGINT              NOT NULL,      -- [first_seq, end_seq): the table's bound
    end_seq        BIGINT              NOT NULL,
    table_name     VARCHAR(63)         NOT NULL UNIQUE,  -- the segment table, as created; authoritative
    state          evbk__segment_state NOT NULL,
    bytes          BIGINT              NOT NULL DEFAULT 0,  -- pg_total_relation_size of the table when sealed; 0 while open
    newest_at      TIMESTAMPTZ,                       -- the last event's sequence_time when sealed; NULL while open or empty
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

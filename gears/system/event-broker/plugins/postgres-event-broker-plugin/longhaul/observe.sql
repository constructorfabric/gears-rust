-- Views over the long-haul schema for an operator at psql (longhaul/README.md).
-- Everything here is created in this session's temporary schema; nothing is
-- written to the plugin's schema, and it all goes when the session ends.
--
--   \ir longhaul/observe.sql                 -- or: \set schema other_schema first
--   TABLE lh_states \watch 2
--   :check                                   -- the live invariants, one snapshot

\if :{?schema}
\else
\set schema evbk_longhaul
\endif

\ir ../tests/sql/invariants.sql

-- Per partition: head, floor, what is stored; its size on disk as retention
-- measures it (sealed segments as measured when sealed, open ones now).
CREATE OR REPLACE TEMP VIEW lh_partitions AS
SELECT regexp_replace(l.topic_id, '^.*\.x\.([^.]+)\.v1$', '\1') AS topic,
       p.partition,
       p.hwm,
       r.floor,
       p.hwm - r.floor AS stored_events,
       round(COALESCE((SELECT sum(CASE WHEN s.state = 'open'
                    THEN COALESCE(pg_total_relation_size(to_regclass(format('%I.%I', :'schema', s.table_name))), 0)
                    ELSE s.bytes END)
                 FROM :"schema".evbk__segment s
                WHERE s.topic_uuid = p.topic_uuid AND s.partition = p.partition
                  AND s.state IN ('open', 'sealed')), 0) / 1048576.0, 1) AS stored_mib
  FROM :"schema".evbk__partition p
  JOIN :"schema".evbk__retention r ON r.topic_uuid = p.topic_uuid AND r.partition = p.partition
  JOIN :"schema".evbk__log l ON l.topic_uuid = p.topic_uuid
 ORDER BY 1, 2;

-- Per segment: range, state, the size the sealer measured, whether its table
-- is attached, and its size now. A table dropped mid-query shows as null, not an error.
CREATE OR REPLACE TEMP VIEW lh_segments AS
SELECT regexp_replace(l.topic_id, '^.*\.x\.([^.]+)\.v1$', '\1') AS topic,
       s.partition,
       s.first_seq,
       s.end_seq,
       s.state,
       s.bytes,
       EXISTS (SELECT 1 FROM pg_inherits i
                WHERE i.inhrelid = to_regclass(format('%I.%I', :'schema', s.table_name))
                  AND NOT i.inhdetachpending) AS attached,
       pg_size_pretty(pg_total_relation_size(to_regclass(format('%I.%I', :'schema', s.table_name)))) AS size,
       date_trunc('second', now() - s.newest_at) AS newest_age,
       date_trunc('second', now() - s.changed_at) AS changed_ago
  FROM :"schema".evbk__segment s
  JOIN :"schema".evbk__log l ON l.topic_uuid = s.topic_uuid
 ORDER BY 1, 2, 3;

-- Segment rows per topic and state.
CREATE OR REPLACE TEMP VIEW lh_states AS
SELECT regexp_replace(l.topic_id, '^.*\.x\.([^.]+)\.v1$', '\1') AS topic,
       count(*) FILTER (WHERE s.state = 'open') AS open,
       count(*) FILTER (WHERE s.state = 'sealed') AS sealed,
       count(*) FILTER (WHERE s.state = 'expired') AS expired,
       count(*) FILTER (WHERE s.state = 'detaching') AS detaching,
       sum(s.bytes) AS sealed_bytes
  FROM :"schema".evbk__segment s
  JOIN :"schema".evbk__log l ON l.topic_uuid = s.topic_uuid
 GROUP BY 1
 ORDER BY 1;

-- The plugin's sessions: pools, LISTEN, the detacher's own connection.
CREATE OR REPLACE TEMP VIEW lh_sessions AS
SELECT pid,
       application_name,
       state,
       wait_event_type,
       wait_event,
       date_trunc('second', now() - xact_start) AS xact_age,
       left(regexp_replace(query, '\s+', ' ', 'g'), 100) AS query
  FROM pg_stat_activity
 WHERE datname = current_database() AND application_name LIKE 'evbk-%'
 ORDER BY application_name, pid;

-- Advisory locks (evbk:seg, evbk:det) and who holds or waits for them.
CREATE OR REPLACE TEMP VIEW lh_locks AS
SELECT l.pid,
       a.application_name,
       l.granted,
       l.mode,
       l.classid,
       l.objid,
       a.state,
       date_trunc('second', now() - a.xact_start) AS xact_age
  FROM pg_locks l
  JOIN pg_stat_activity a ON a.pid = l.pid
 WHERE l.locktype = 'advisory' AND l.database = (SELECT oid FROM pg_database WHERE datname = current_database())
 ORDER BY l.granted, l.pid;

\set check 'BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY; SELECT * FROM pg_temp.evbk_violations(' :'schema' ', ''live''); COMMIT;'
\set settled 'BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY; SELECT * FROM pg_temp.evbk_violations(' :'schema' ', ''settled''); COMMIT;'

\echo 'views: lh_partitions lh_segments lh_states lh_sessions lh_locks'
\echo 'checks: :check (live) :settled (once writers stopped)'
\echo 'e.g.  TABLE lh_states \\watch 2'

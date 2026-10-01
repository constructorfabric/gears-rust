-- What the plugin's tables must satisfy for stored events to be intact
-- (openspec change eb-pg-plugin-testing, `event-broker-postgres-storage-invariants`).
--
-- Creates `pg_temp.evbk_violations(schema_name, mode)` in this session only:
-- nothing is created in the plugin's schema, and the function is gone when the
-- session ends. It returns one row per violation; no rows means intact.
--
--   \i tests/sql/invariants.sql
--   BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY;
--   SELECT * FROM pg_temp.evbk_violations('evbk_longhaul', 'live');
--   COMMIT;
--
-- The caller opens the snapshot, so every plugin table is read in one. The
-- catalog is not read in it: an ATTACH or DETACH committing meanwhile can make
-- a live invariant look broken once, so a live finding counts only when two
-- checks in a row report it (design D2).
--
-- `live` holds at every instant, under load:
--   L1  0 <= floor <= hwm
--   L2  exactly hwm - floor events in (floor, hwm], none above hwm
--   L3  appended_bytes - removed_bytes = SUM(stored_bytes) over (floor, hwm]
--   L4  open and sealed rows start at floor + 1 and tile without gap or overlap
--   L5  expired and detaching rows end at or below the floor; open and sealed above it
--   L6  sealed: counted to its end, bytes = SUM(stored_bytes) of the segment
--   L7  open: counted_to within [first_seq - 1, upto], upto = max(first_seq - 1, min(hwm, end_seq - 1)),
--       bytes = SUM(stored_bytes) of what it counts, newest_at null exactly when nothing is
--   L8  open, sealed, expired: the table is attached to its parent with the row's bound
--   L9  every segment-named table has a row
-- `settled` adds, once appends stopped and maintenance ran two rescans:
--   S1  no open row is full (end_seq - 1 <= hwm)
--   S2  every open row is counted to its upto (L7)
--   S3  at least two segments lie wholly above hwm
--   S4  no expired or detaching row
--   S5  bounded by age: no sealed row older than max_age
--   S6  bounded by bytes: the oldest sealed segment is not kept while the rest still holds max_bytes
--   S7  no advisory lock held by an idle evbk-* session (this database, any schema)

CREATE OR REPLACE FUNCTION pg_temp.evbk_violations(schema_name text, mode text DEFAULT 'live')
RETURNS TABLE (invariant text, topic_uuid uuid, partition integer, first_seq bigint, detail text)
LANGUAGE plpgsql AS $fn$
#variable_conflict use_column
DECLARE
    settled  boolean;
    p        record;
    s        record;
    t        record;
    parent   oid;
    expected bigint;    -- the first_seq the next open or sealed row must have
    live     integer;
    room     integer;
    oldest   bigint;    -- bytes of the oldest sealed row, NULL before one is seen
    upto     bigint;    -- the last sequence an open row can have counted: none above hwm
    n_in     bigint;
    n_above  bigint;
    scanned  bigint;
BEGIN
    IF mode NOT IN ('live', 'settled') THEN
        RAISE EXCEPTION 'mode is live or settled, not %', mode;
    END IF;
    settled := mode = 'settled';

    FOR p IN EXECUTE format(
        'SELECT pa.topic_uuid, pa.partition, pa.table_name, pa.hwm, pa.appended_bytes,
                r.floor, r.removed_bytes, l.max_age, l.max_bytes
           FROM %1$I.evbk__partition pa
           JOIN %1$I.evbk__retention r ON r.topic_uuid = pa.topic_uuid AND r.partition = pa.partition
           JOIN %1$I.evbk__log l ON l.topic_uuid = pa.topic_uuid
          ORDER BY pa.topic_uuid, pa.partition', schema_name)
    LOOP
        topic_uuid := p.topic_uuid;
        partition := p.partition;
        first_seq := NULL;

        IF NOT (0 <= p.floor AND p.floor <= p.hwm) THEN
            invariant := 'L1';
            detail := format('floor=%s hwm=%s', p.floor, p.hwm);
            RETURN NEXT;
        END IF;

        EXECUTE format(
            'SELECT count(*) FILTER (WHERE sequence <= $2),
                    COALESCE(sum(stored_bytes) FILTER (WHERE sequence <= $2), 0)::bigint,
                    count(*) FILTER (WHERE sequence > $2)
               FROM %I.%I WHERE sequence > $1', schema_name, p.table_name)
            INTO n_in, scanned, n_above USING p.floor, p.hwm;
        IF n_in <> p.hwm - p.floor OR n_above <> 0 THEN
            invariant := 'L2';
            detail := format('count=%s expected=%s above_hwm=%s', n_in, p.hwm - p.floor, n_above);
            RETURN NEXT;
        END IF;
        IF scanned <> p.appended_bytes - p.removed_bytes THEN
            invariant := 'L3';
            detail := format('stored_bytes=%s scanned=%s', p.appended_bytes - p.removed_bytes, scanned);
            RETURN NEXT;
        END IF;

        SELECT c.oid INTO parent FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = schema_name AND c.relname = p.table_name;
        expected := p.floor + 1;
        live := 0;
        room := 0;
        oldest := NULL;

        FOR s IN EXECUTE format(
            'SELECT first_seq, end_seq, state::text AS state, counted_to, bytes, newest_at, table_name
               FROM %I.evbk__segment WHERE topic_uuid = $1 AND partition = $2
              ORDER BY first_seq', schema_name)
            USING p.topic_uuid, p.partition
        LOOP
            first_seq := s.first_seq;

            IF s.state IN ('open', 'sealed') THEN
                live := live + 1;
                IF s.first_seq <> expected OR s.end_seq <= s.first_seq THEN
                    invariant := 'L4';
                    detail := format('range=[%s,%s) expected_first=%s', s.first_seq, s.end_seq, expected);
                    RETURN NEXT;
                END IF;
                expected := s.end_seq;
                IF s.first_seq <= p.floor THEN
                    invariant := 'L5';
                    detail := format('%s first_seq=%s floor=%s', s.state, s.first_seq, p.floor);
                    RETURN NEXT;
                END IF;
            ELSIF s.end_seq - 1 > p.floor THEN
                invariant := 'L5';
                detail := format('%s end_seq=%s floor=%s', s.state, s.end_seq, p.floor);
                RETURN NEXT;
            END IF;

            IF s.state = 'sealed' THEN
                EXECUTE format(
                    'SELECT COALESCE(sum(stored_bytes), 0)::bigint FROM %I.%I
                      WHERE sequence >= $1 AND sequence < $2', schema_name, p.table_name)
                    INTO scanned USING s.first_seq, s.end_seq;
                IF s.counted_to <> s.end_seq - 1 OR s.bytes <> scanned THEN
                    invariant := 'L6';
                    detail := format('counted_to=%s end_seq=%s bytes=%s scanned=%s',
                                     s.counted_to, s.end_seq, s.bytes, scanned);
                    RETURN NEXT;
                END IF;
                IF oldest IS NULL THEN
                    oldest := s.bytes;
                    IF settled AND p.max_bytes IS NOT NULL
                       AND p.appended_bytes - p.removed_bytes - s.bytes >= p.max_bytes THEN
                        invariant := 'S6';
                        detail := format('stored_bytes=%s oldest_sealed=%s max_bytes=%s',
                                         p.appended_bytes - p.removed_bytes, s.bytes, p.max_bytes);
                        RETURN NEXT;
                    END IF;
                END IF;
                IF settled AND p.max_age IS NOT NULL AND s.newest_at < now() - p.max_age THEN
                    invariant := 'S5';
                    detail := format('newest_at=%s max_age=%s', s.newest_at, p.max_age);
                    RETURN NEXT;
                END IF;
            END IF;

            IF s.state = 'open' THEN
                upto := greatest(s.first_seq - 1, least(p.hwm, s.end_seq - 1));
                EXECUTE format(
                    'SELECT COALESCE(sum(stored_bytes), 0)::bigint FROM %I.%I
                      WHERE sequence >= $1 AND sequence <= $2', schema_name, p.table_name)
                    INTO scanned USING s.first_seq, s.counted_to;
                IF s.counted_to < s.first_seq - 1
                   OR s.counted_to > upto
                   OR (s.newest_at IS NULL) <> (s.counted_to = s.first_seq - 1)
                   OR s.bytes <> scanned THEN
                    invariant := 'L7';
                    detail := format('counted_to=%s hwm=%s end_seq=%s bytes=%s scanned=%s newest_at=%s',
                                     s.counted_to, p.hwm, s.end_seq, s.bytes, scanned,
                                     COALESCE(s.newest_at::text, 'null'));
                    RETURN NEXT;
                END IF;
                IF settled AND s.end_seq - 1 <= p.hwm THEN
                    invariant := 'S1';
                    detail := format('end_seq=%s hwm=%s', s.end_seq, p.hwm);
                    RETURN NEXT;
                ELSIF settled AND s.counted_to <> upto THEN
                    invariant := 'S2';
                    detail := format('counted_to=%s expected=%s', s.counted_to, upto);
                    RETURN NEXT;
                END IF;
                IF s.first_seq > p.hwm THEN
                    room := room + 1;
                END IF;
            END IF;

            IF s.state IN ('open', 'sealed', 'expired') THEN
                SELECT i.inhparent, i.inhdetachpending, pg_get_expr(c.relpartbound, c.oid) AS bound
                  INTO t
                  FROM pg_class c
                  JOIN pg_namespace n ON n.oid = c.relnamespace
                  LEFT JOIN pg_inherits i ON i.inhrelid = c.oid
                 WHERE n.nspname = schema_name AND c.relname = s.table_name;
                IF NOT FOUND THEN
                    invariant := 'L8';
                    detail := format('%s table %s missing', s.state, s.table_name);
                    RETURN NEXT;
                ELSIF t.inhparent IS DISTINCT FROM parent OR t.inhdetachpending THEN
                    invariant := 'L8';
                    detail := format('%s table %s not attached%s', s.state, s.table_name,
                                     CASE WHEN t.inhdetachpending THEN ' (detach pending)' ELSE '' END);
                    RETURN NEXT;
                ELSIF t.bound <> format('FOR VALUES FROM (''%s'') TO (''%s'')', s.first_seq, s.end_seq) THEN
                    invariant := 'L8';
                    detail := format('%s table %s bound %s', s.state, s.table_name, t.bound);
                    RETURN NEXT;
                END IF;
            END IF;

            IF settled AND s.state IN ('expired', 'detaching') THEN
                invariant := 'S4';
                detail := s.state;
                RETURN NEXT;
            END IF;
        END LOOP;

        first_seq := NULL;
        IF live = 0 THEN
            invariant := 'L4';
            detail := 'no open or sealed segment';
            RETURN NEXT;
        END IF;
        IF settled AND room < 2 THEN
            invariant := 'S3';
            detail := format('segments above hwm=%s: %s', p.hwm, room);
            RETURN NEXT;
        END IF;
    END LOOP;

    FOR t IN EXECUTE format(
        'SELECT c.relname::text AS relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
          WHERE n.nspname = $1 AND c.relkind = ''r''
            AND c.relname ~ ''^evbk__[0-9a-f]{32}_p[0-9]+_s[0-9a-f]{16}$''
            AND NOT EXISTS (SELECT 1 FROM %I.evbk__segment g WHERE g.table_name = c.relname)
          ORDER BY c.relname', schema_name)
        USING schema_name
    LOOP
        invariant := 'L9';
        topic_uuid := substring(t.relname FROM 7 FOR 32)::uuid;
        partition := substring(t.relname FROM '_p([0-9]+)_s')::integer;
        first_seq := ('x' || substring(t.relname FROM '_s([0-9a-f]{16})$'))::bit(64)::bigint;
        detail := format('table %s has no row', t.relname);
        RETURN NEXT;
    END LOOP;

    IF settled THEN
        FOR t IN
            SELECT a.pid, a.application_name, l.classid, l.objid
              FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid
             WHERE l.locktype = 'advisory'
               AND l.database = (SELECT d.oid FROM pg_database d WHERE d.datname = current_database())
               AND a.application_name LIKE 'evbk-%'
               AND a.state = 'idle'
             ORDER BY a.pid, l.classid, l.objid
        LOOP
            invariant := 'S7';
            topic_uuid := NULL;
            partition := NULL;
            first_seq := NULL;
            detail := format('pid=%s application_name=%s key=%s:%s', t.pid, t.application_name, t.classid, t.objid);
            RETURN NEXT;
        END LOOP;
    END IF;
END
$fn$;

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
--   L3  open and sealed rows start at floor + 1 and tile without gap or overlap
--   L4  expired and detaching rows end at or below the floor; open and sealed above it
--   L5  sealed: full (end_seq - 1 <= hwm), measured (bytes > 0), and newest_at the sequence_time of
--       its last event (NULL with none left)
--   L6  open: not measured yet, bytes = 0 and newest_at NULL
--   L7  open, sealed, expired: the table is attached to its parent with the row's bound
--   L8  every segment-named table has a row
-- `settled` adds, once appends stopped and maintenance ran two rescans:
--   S1  no open row is full (end_seq - 1 <= hwm)
--   S2  at least two segments lie wholly above hwm
--   S3  no expired or detaching row
--   S4  bounded by age: no sealed row older than max_age, nor one with no event left
--   S5  bounded by bytes: the oldest sealed segment is not kept while the rest still holds max_bytes,
--       the partition's size being the sealed rows' bytes and the open tables' size now
--   S6  no advisory lock held by an idle evbk-* session (this database, any schema)

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
    stored   bigint;    -- the partition's size on disk, as retention measures it
    newest   timestamptz;
    n_in     bigint;
    n_above  bigint;
BEGIN
    IF mode NOT IN ('live', 'settled') THEN
        RAISE EXCEPTION 'mode is live or settled, not %', mode;
    END IF;
    settled := mode = 'settled';

    FOR p IN EXECUTE format(
        'SELECT pa.topic_uuid, pa.partition, pa.table_name, pa.hwm, r.floor, l.max_age, l.max_bytes
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
            'SELECT count(*) FILTER (WHERE sequence <= $2), count(*) FILTER (WHERE sequence > $2)
               FROM %I.%I WHERE sequence > $1', schema_name, p.table_name)
            INTO n_in, n_above USING p.floor, p.hwm;
        IF n_in <> p.hwm - p.floor OR n_above <> 0 THEN
            invariant := 'L2';
            detail := format('count=%s expected=%s above_hwm=%s', n_in, p.hwm - p.floor, n_above);
            RETURN NEXT;
        END IF;

        EXECUTE format(
            'SELECT COALESCE(sum(CASE WHEN state = ''open''
                     THEN COALESCE(pg_total_relation_size(to_regclass(quote_ident($3) || ''.'' || table_name)), 0)
                     ELSE bytes END), 0)::bigint
               FROM %I.evbk__segment WHERE topic_uuid = $1 AND partition = $2 AND state IN (''open'', ''sealed'')',
            schema_name)
            INTO stored USING p.topic_uuid, p.partition, schema_name;

        SELECT c.oid INTO parent FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = schema_name AND c.relname = p.table_name;
        expected := p.floor + 1;
        live := 0;
        room := 0;
        oldest := NULL;

        FOR s IN EXECUTE format(
            'SELECT first_seq, end_seq, state::text AS state, bytes, newest_at, table_name
               FROM %I.evbk__segment WHERE topic_uuid = $1 AND partition = $2
              ORDER BY first_seq', schema_name)
            USING p.topic_uuid, p.partition
        LOOP
            first_seq := s.first_seq;

            IF s.state IN ('open', 'sealed') THEN
                live := live + 1;
                IF s.first_seq <> expected OR s.end_seq <= s.first_seq THEN
                    invariant := 'L3';
                    detail := format('range=[%s,%s) expected_first=%s', s.first_seq, s.end_seq, expected);
                    RETURN NEXT;
                END IF;
                expected := s.end_seq;
                IF s.first_seq <= p.floor THEN
                    invariant := 'L4';
                    detail := format('%s first_seq=%s floor=%s', s.state, s.first_seq, p.floor);
                    RETURN NEXT;
                END IF;
            ELSIF s.end_seq - 1 > p.floor THEN
                invariant := 'L4';
                detail := format('%s end_seq=%s floor=%s', s.state, s.end_seq, p.floor);
                RETURN NEXT;
            END IF;

            IF s.state = 'sealed' THEN
                EXECUTE format(
                    'SELECT sequence_time FROM %I.%I WHERE sequence >= $1 AND sequence < $2
                      ORDER BY sequence DESC LIMIT 1', schema_name, p.table_name)
                    INTO newest USING s.first_seq, s.end_seq;
                IF s.end_seq - 1 > p.hwm OR s.bytes <= 0 OR s.newest_at IS DISTINCT FROM newest THEN
                    invariant := 'L5';
                    detail := format('end_seq=%s hwm=%s bytes=%s newest_at=%s last_event_at=%s',
                                     s.end_seq, p.hwm, s.bytes, COALESCE(s.newest_at::text, 'null'),
                                     COALESCE(newest::text, 'null'));
                    RETURN NEXT;
                END IF;
                IF oldest IS NULL THEN
                    oldest := s.bytes;
                    IF settled AND p.max_bytes IS NOT NULL AND stored - s.bytes >= p.max_bytes THEN
                        invariant := 'S5';
                        detail := format('stored=%s oldest_sealed=%s max_bytes=%s',
                                         stored, s.bytes, p.max_bytes);
                        RETURN NEXT;
                    END IF;
                END IF;
                IF settled AND p.max_age IS NOT NULL
                   AND (s.newest_at IS NULL OR s.newest_at < now() - p.max_age) THEN
                    invariant := 'S4';
                    detail := format('newest_at=%s max_age=%s', COALESCE(s.newest_at::text, 'null'), p.max_age);
                    RETURN NEXT;
                END IF;
            END IF;

            IF s.state = 'open' THEN
                IF s.bytes <> 0 OR s.newest_at IS NOT NULL THEN
                    invariant := 'L6';
                    detail := format('bytes=%s newest_at=%s', s.bytes, COALESCE(s.newest_at::text, 'null'));
                    RETURN NEXT;
                END IF;
                IF settled AND s.end_seq - 1 <= p.hwm THEN
                    invariant := 'S1';
                    detail := format('end_seq=%s hwm=%s', s.end_seq, p.hwm);
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
                    invariant := 'L7';
                    detail := format('%s table %s missing', s.state, s.table_name);
                    RETURN NEXT;
                ELSIF t.inhparent IS DISTINCT FROM parent OR t.inhdetachpending THEN
                    invariant := 'L7';
                    detail := format('%s table %s not attached%s', s.state, s.table_name,
                                     CASE WHEN t.inhdetachpending THEN ' (detach pending)' ELSE '' END);
                    RETURN NEXT;
                ELSIF t.bound <> format('FOR VALUES FROM (''%s'') TO (''%s'')', s.first_seq, s.end_seq) THEN
                    invariant := 'L7';
                    detail := format('%s table %s bound %s', s.state, s.table_name, t.bound);
                    RETURN NEXT;
                END IF;
            END IF;

            IF settled AND s.state IN ('expired', 'detaching') THEN
                invariant := 'S3';
                detail := s.state;
                RETURN NEXT;
            END IF;
        END LOOP;

        first_seq := NULL;
        IF live = 0 THEN
            invariant := 'L3';
            detail := 'no open or sealed segment';
            RETURN NEXT;
        END IF;
        IF settled AND room < 2 THEN
            invariant := 'S2';
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
        invariant := 'L8';
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
            invariant := 'S6';
            topic_uuid := NULL;
            partition := NULL;
            first_seq := NULL;
            detail := format('pid=%s application_name=%s key=%s:%s', t.pid, t.application_name, t.classid, t.objid);
            RETURN NEXT;
        END LOOP;
    END IF;
END
$fn$;

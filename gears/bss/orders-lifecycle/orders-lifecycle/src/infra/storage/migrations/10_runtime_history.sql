-- S2-12: the restricted runtime role may read the gear's migration history so a deployment can
-- apply migrations with an admin login and then run the gear under `bss_orders_runtime` (the
-- toolkit runner probes the history table before any DDL and skips applied migrations; a pending
-- migration still fails closed under this role, which holds no DDL privilege).
DO $$ DECLARE t record; BEGIN FOR t IN SELECT tablename FROM pg_tables WHERE schemaname=current_schema() AND tablename LIKE 'toolkit\_migrations\_\_bss\_orders\_lifecycle\_\_%' LOOP EXECUTE format('GRANT SELECT ON %I.%I TO bss_orders_runtime',current_schema(),t.tablename); END LOOP; END $$;

ALTER TABLE gm_active_syncs DROP COLUMN IF EXISTS credential_key;

ALTER TABLE gm_repo_sync_status DROP COLUMN IF EXISTS credential_key;

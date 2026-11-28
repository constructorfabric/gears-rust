ALTER TABLE gm_repo_sync_status ADD COLUMN IF NOT EXISTS credential_key VARCHAR(255);

ALTER TABLE gm_active_syncs ADD COLUMN IF NOT EXISTS credential_key VARCHAR(255);

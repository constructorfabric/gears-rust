ALTER TABLE gm_repo_sync_status ADD COLUMN credential_key TEXT;

ALTER TABLE gm_active_syncs ADD COLUMN credential_key TEXT;

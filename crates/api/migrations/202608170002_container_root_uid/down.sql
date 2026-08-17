-- Reversing this does not restore the confinement it replaced. The
-- `subject:subject` uid and the read-only root live in code, not here, and a
-- host whose daemon still runs with `--userns-remap` will keep mapping
-- container root to a uid that no longer has a column to record it.
ALTER TABLE host DROP CONSTRAINT host_service_needs_container_root_uid;
ALTER TABLE host DROP COLUMN container_root_uid;

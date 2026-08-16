-- Restoring the table-wide constraint fails on any host where a name was
-- reused after a withdrawal — which is the correct outcome, and the point of
-- the migration. Those rows are real history; rolling the schema back is not
-- a reason to invent a conflict between a live container and a record of one
-- that never started.

DROP INDEX host_container_ref_live;

ALTER TABLE host_container
  ADD CONSTRAINT host_container_host_id_container_ref_key UNIQUE (host_id, container_ref);

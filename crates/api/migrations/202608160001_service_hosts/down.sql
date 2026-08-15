-- Reversing this drops every service host and everything on one: their rows
-- have no owner and there is nowhere to put them once ownership is mandatory
-- again.
DELETE FROM host WHERE user_id IS NULL;
DELETE FROM image WHERE user_id IS NULL;

DROP INDEX image_name_service;
DROP INDEX image_name_owned;
ALTER TABLE image ALTER COLUMN user_id SET NOT NULL;
ALTER TABLE image ADD CONSTRAINT image_user_id_name_key UNIQUE (user_id, name);

DROP TABLE host_user_quota;
DROP TABLE host_user;
DROP TABLE user_subject;
DROP SEQUENCE subject_seq;

DROP INDEX host_container_owner;
ALTER TABLE host_container DROP COLUMN user_id;

ALTER TABLE host DROP CONSTRAINT host_service_needs_endpoint;
ALTER TABLE host DROP CONSTRAINT host_service_needs_data_root;
ALTER TABLE host
  DROP COLUMN default_cpu_millis,
  DROP COLUMN default_memory_bytes,
  DROP COLUMN default_storage_bytes,
  DROP COLUMN default_container_max,
  DROP COLUMN user_data_root;

DROP INDEX host_name_service;
DROP INDEX host_name_owned;
ALTER TABLE host ALTER COLUMN user_id SET NOT NULL;
ALTER TABLE host ADD CONSTRAINT host_user_id_name_key UNIQUE (user_id, name);

ALTER TABLE host DROP CONSTRAINT IF EXISTS host_ssh_host_key_transport;
ALTER TABLE host DROP COLUMN IF EXISTS ssh_host_key;

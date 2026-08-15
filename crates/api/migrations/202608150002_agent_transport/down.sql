DROP TABLE IF EXISTS agent_credential;
DROP TABLE IF EXISTS agent_enrollment;

ALTER TABLE host DROP CONSTRAINT host_transport_config;
ALTER TABLE host ADD  CONSTRAINT host_transport_config CHECK (
    (transport = 'local' AND ssh_address IS NULL)
 OR (transport = 'ssh'   AND ssh_address IS NOT NULL)
);

ALTER TABLE host DROP CONSTRAINT host_transport_check;
ALTER TABLE host ADD  CONSTRAINT host_transport_check CHECK (
    transport IN ('local', 'ssh')
);

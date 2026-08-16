-- Restoring this constraint fails on any service host already registered
-- with an agent daemon, which is the correct outcome: rolling the schema
-- back does not put the API process back on the host's machine.
ALTER TABLE host ADD CONSTRAINT host_service_needs_endpoint
  CHECK (user_id IS NOT NULL OR docker_endpoint LIKE 'unix://%');

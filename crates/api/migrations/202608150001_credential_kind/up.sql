ALTER TABLE credentials
  ADD COLUMN kind text NOT NULL DEFAULT 'api_key';

ALTER TABLE credentials
  ADD CONSTRAINT credentials_kind_check CHECK (kind IN ('api_key', 'ssh_key'));

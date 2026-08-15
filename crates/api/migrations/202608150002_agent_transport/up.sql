-- Agent transport. See internal-docs/agent-transport.md (X42).
--
-- `host.transport` gains a third value. Two checks gate it, not one: the
-- inline `transport in ('local','ssh')` from 202608120001_hosts (postgres
-- named it `host_transport_check`) rejects 'agent' before the config check
-- is ever reached, so it has to be dropped and rewritten too.
ALTER TABLE host DROP CONSTRAINT host_transport_check;
ALTER TABLE host ADD  CONSTRAINT host_transport_check CHECK (
    transport IN ('local', 'ssh', 'agent')
);

-- Agent-mode hosts carry no ssh_address, since faber never dials them (R14
-- — the connection always arrives from the daemon, never the other way).
ALTER TABLE host DROP CONSTRAINT host_transport_config;
ALTER TABLE host ADD  CONSTRAINT host_transport_config CHECK (
    (transport = 'local' AND ssh_address IS NULL)
 OR (transport = 'ssh'   AND ssh_address IS NOT NULL)
 OR (transport = 'agent' AND ssh_address IS NULL)
);

-- `host_ssh_host_key_transport` (202608130001_host_key) already reads
-- `transport = 'ssh' or ssh_host_key is null`, which is correct as written
-- for agent mode and needs no change: the daemon's pinned key lives in
-- `agent_credential.host_pubkey`, not `host.ssh_host_key`.

-- One-time secret exchanged for a long-lived connection credential at first
-- daemon run. Single-use and short-lived, so it earns its own table rather
-- than living on `host`.
CREATE TABLE agent_enrollment (
    id          uuid        PRIMARY KEY,
    host_id     uuid        NOT NULL REFERENCES host(id) ON DELETE CASCADE,
    token_hash  text        NOT NULL,          -- bootstrap secret, salted hash
    expires_at  timestamptz NOT NULL,
    consumed_at timestamptz                    -- set the moment it's exchanged; null is still redeemable
);

CREATE INDEX agent_enrollment_host_id_idx ON agent_enrollment (host_id);

-- The long-lived connection credential a daemon presents on every reconnect,
-- plus the SSH host key it reported at enrollment (X39 — pinned, not TOFU;
-- the identity check already happened at enrollment, an authenticated
-- exchange).
CREATE TABLE agent_credential (
    id          uuid        PRIMARY KEY,
    host_id     uuid        NOT NULL REFERENCES host(id) ON DELETE CASCADE,
    token_hash  text        NOT NULL,          -- long-lived connection credential, salted hash
    -- Stored as the public key the daemon reported, not the SHA256
    -- fingerprint `host.ssh_host_key` holds for SSH hosts — the daemon can
    -- report the key, and `HostKey::Verify` wants the fingerprint, so the
    -- conversion happens at bind.
    host_pubkey text        NOT NULL,
    issued_at   timestamptz NOT NULL DEFAULT now(),
    revoked_at  timestamptz
);

CREATE INDEX agent_credential_host_id_idx ON agent_credential (host_id);

-- One *active* credential per host, not one ever — "regenerate token" issues
-- a new row and revokes the old one rather than overwriting it, so a revoked
-- credential stays in the log instead of disappearing.
CREATE UNIQUE INDEX agent_credential_host_id_active_idx
    ON agent_credential (host_id) WHERE revoked_at IS NULL;

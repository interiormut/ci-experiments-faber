-- Durable generations for browser-reachable live HTTP services.
--
-- The token is deliberately stored in plaintext. It is an unguessable
-- capability URL, not a first-class confidential credential: possession
-- grants access, and a database reader is allowed to learn active URLs.

ALTER TABLE host ADD COLUMN preview_network text;

COMMENT ON COLUMN host.preview_network IS
  'Docker network used to resolve live-preview container addresses; NULL permits only an unambiguous single network.';

CREATE TABLE presentation (
  id                 uuid        PRIMARY KEY,
  session_id         uuid        NOT NULL,
  environment_label  text        NOT NULL,
  port               integer     NOT NULL CHECK (port BETWEEN 1 AND 65535),
  token              text        NOT NULL UNIQUE,
  upstream_host_mode text        NOT NULL DEFAULT 'loopback'
                                  CHECK (upstream_host_mode IN ('loopback', 'preserve')),
  created_at         timestamptz NOT NULL DEFAULT now(),
  revoked_at         timestamptz,
  FOREIGN KEY (session_id, environment_label)
    REFERENCES session_environment(session_id, label) ON DELETE CASCADE
);

CREATE UNIQUE INDEX presentation_active_target
  ON presentation (session_id, environment_label, port)
  WHERE revoked_at IS NULL;

COMMENT ON TABLE presentation IS
  'Generations of unguessable capability URLs for live HTTP services. Possession grants access.';

COMMENT ON COLUMN presentation.token IS
  '32 cryptographically random bytes encoded as unpadded base64url; retained after revocation so known revoked URLs resolve to 410.';

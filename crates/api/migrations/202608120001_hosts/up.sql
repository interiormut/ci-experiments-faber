-- Execution environments. See internal-docs/host.md.
--
-- The registration primitive is the host, not the environment: every execution
-- mode bottoms out in reach the machine, then exec, and the machine is the only
-- part carrying authentication and a network path.
--
-- Two deliberate departures from the design abstract's SQL:
--   * `transport` and `exec_mode` are text with a CHECK rather than pg enums,
--     matching `models.wire` — the values still travel as a closed set, without
--     the diesel `ToSql`/`FromSql` plumbing a custom type needs.
--   * `host` and `image` carry `user_id`, like `credentials` and `models`. The
--     abstract omits it because it is describing entity shape, not tenancy;
--     every configuration table in this schema is per-user, so `name` is unique
--     per user rather than globally.

CREATE TABLE host (
  id              uuid        PRIMARY KEY,
  user_id         uuid        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name            text        NOT NULL,
  transport       text        NOT NULL CHECK (transport IN ('local', 'ssh')),
  exec_mode       text        NOT NULL CHECK (exec_mode IN ('direct', 'docker')),

  ssh_address     text,        -- user@host:port
  ssh_key_ref     text,        -- secret-store handle, never material
  docker_endpoint text,        -- unix:// or tcp://; null = local socket

  created_at      timestamptz NOT NULL DEFAULT now(),
  disabled_at     timestamptz, -- operator intent, not observed state

  UNIQUE (user_id, name),

  -- R1: reachability is never a column. `disabled_at` above is the only state
  -- here, and it records what the operator asked for, not what was observed.
  CONSTRAINT host_transport_config CHECK (
      (transport = 'local' AND ssh_address IS NULL)
   OR (transport = 'ssh'   AND ssh_address IS NOT NULL)
  )
);

CREATE INDEX host_user_id_idx ON host (user_id);

-- A row asserts *faber knows about this container*, not *this container exists*.
CREATE TABLE host_container (
  id              uuid        PRIMARY KEY,
  host_id         uuid        NOT NULL REFERENCES host(id) ON DELETE CASCADE,
  container_ref   text        NOT NULL,  -- name or id, resolved lazily
  name            text,                  -- user label
  root_path       text        NOT NULL,  -- normalized agent-visible root
  created_at      timestamptz NOT NULL DEFAULT now(),
  unregistered_at timestamptz,           -- about the registration, not the container
  UNIQUE (host_id, container_ref)
);

CREATE INDEX host_container_host_id_idx ON host_container (host_id) WHERE unregistered_at IS NULL;

-- Append-only observation log. Advisory only: the authoritative answer to "is it
-- up" is the next connection attempt, never a row in here.
CREATE TABLE host_probe (
  id           uuid        PRIMARY KEY,
  host_id      uuid        NOT NULL REFERENCES host(id) ON DELETE CASCADE,
  container_id uuid        REFERENCES host_container(id) ON DELETE CASCADE,
  probed_at    timestamptz NOT NULL DEFAULT now(),
  ok           boolean     NOT NULL,
  error        text,                    -- populated when not ok
  os           text,
  arch         text,
  shell        text,
  tools        jsonb,                   -- {"git":"2.43.0", ...}
  root_path    text
);

CREATE INDEX host_probe_host_id_probed_at_idx ON host_probe (host_id, probed_at DESC);

-- A spawn template. Not a host, not a container, not owned by either — it exists
-- so "start me a fresh one" is a convenience rather than a lifecycle commitment.
CREATE TABLE image (
  id                uuid        PRIMARY KEY,
  user_id           uuid        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name              text        NOT NULL,
  reference         text        NOT NULL,  -- registry ref
  default_mounts    jsonb,
  default_root_path text        NOT NULL,
  created_at        timestamptz NOT NULL DEFAULT now(),
  UNIQUE (user_id, name)
);

CREATE INDEX image_user_id_idx ON image (user_id);

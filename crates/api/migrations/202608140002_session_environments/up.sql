-- Environments a session is allowed to reach.
--
-- Append-only with a tombstone, mirroring the binding log the run holds in
-- memory. A label is claimed once per session and stays claimed even after it
-- is removed, which is what `UNIQUE (session_id, label)` enforces against the
-- whole table rather than against the live rows: the transcript records calls
-- as `label:path`, so a label that meant two different machines would make the
-- earlier half of that transcript quietly wrong. Removing a binding therefore
-- stamps `removed_at` and never deletes the row, and a later call against the
-- label answers "not bound" rather than reaching somewhere new.
--
-- These rows are how turn N+1 rebuilds the same target set. They are not the
-- record of *why* it has them — that is the `<environments>` message in the
-- conversation itself, which is what replays.

CREATE TABLE session_environment (
  session_id   uuid   NOT NULL REFERENCES session(id) ON DELETE CASCADE,
  label        text   NOT NULL,
  host_id      uuid   NOT NULL REFERENCES host(id) ON DELETE CASCADE,
  -- Null means the host itself, in direct exec mode.
  container_id uuid   REFERENCES host_container(id) ON DELETE CASCADE,
  added_at     bigint NOT NULL,
  removed_at   bigint,
  PRIMARY KEY (session_id, label)
);

CREATE INDEX session_environment_live_idx
  ON session_environment (session_id) WHERE removed_at IS NULL;

-- The agent-visible root of a host used in direct mode.
--
-- A container carries its own root because the registration names one; a host
-- does not, and the whole of the path contract rests on there being exactly one
-- rooted namespace per target. Nullable rather than defaulted: `/` would hand
-- an agent the entire machine because nobody filled a field in, and a host with
-- no root is simply not something a session can bind — which is a refusal the
-- user can read and fix.
ALTER TABLE host ADD COLUMN root_path text;

COMMENT ON COLUMN host.root_path IS
  'Agent-visible root for direct execution. NULL means this host cannot be bound directly.';

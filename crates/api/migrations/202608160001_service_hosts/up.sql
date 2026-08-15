-- Service-provided hosts.
--
-- A service host is a host faber operates rather than a user. It behaves
-- exactly like transport='local' + exec_mode='docker'; the only new thing is
-- that many users share it, which forces an ownership rule and a per-user
-- resource ceiling.
--
-- The ownership rule is the whole marker: `host.user_id IS NULL`. No `kind`
-- column and no `is_service` flag, because a flag that can disagree with
-- ownership is a bug surface. Every write path already filters
-- `user_id = $me`, and NULL never matches — "users cannot edit service hosts"
-- is therefore derived rather than enforced, and a read path that forgets its
-- `OR user_id IS NULL` hides the service host, which is a failure in the safe
-- direction.

-- ---------------------------------------------------------------------------
-- Ownership becomes optional
-- ---------------------------------------------------------------------------

ALTER TABLE host ALTER COLUMN user_id DROP NOT NULL;

-- The table constraint has to go before the partial indexes mean anything:
-- `UNIQUE (user_id, name)` treats every NULL owner as distinct, so it would
-- permit two service hosts with the same name.
ALTER TABLE host DROP CONSTRAINT host_user_id_name_key;
CREATE UNIQUE INDEX host_name_owned   ON host (user_id, name) WHERE user_id IS NOT NULL;
CREATE UNIQUE INDEX host_name_service ON host (name)          WHERE user_id IS NULL;

-- ---------------------------------------------------------------------------
-- Default limits and the per-user data root
-- ---------------------------------------------------------------------------

-- NULL means unlimited, everywhere, always — never "inherit". Defaults live
-- here rather than in a sentinel row because the precedent exists (the four
-- `ssh_*` columns are equally meaningless for a local host) and because it
-- keeps the override table single-meaning.
ALTER TABLE host
  ADD COLUMN default_cpu_millis    INT,
  ADD COLUMN default_memory_bytes  BIGINT,
  ADD COLUMN default_storage_bytes BIGINT,
  ADD COLUMN default_container_max INT,
  ADD COLUMN user_data_root        TEXT;

COMMENT ON COLUMN host.user_data_root IS
  'Parent of the per-user directories a service host quotas. Required for a service host, meaningless for an owned one.';

ALTER TABLE host ADD CONSTRAINT host_service_needs_data_root
  CHECK (user_id IS NOT NULL OR user_data_root IS NOT NULL);

-- The same reasoning, one field over. `validate_docker_endpoint` already
-- refuses a local docker host with no endpoint, but it runs on the user-facing
-- create and update routes — which a service host, inserted by an operator,
-- never passes through. Without this the omission surfaces as a confusing 400
-- at the first launch instead of at provisioning.
--
-- A unix socket specifically, which is the constraint that makes the rest of
-- the enforcement true. Faber writes cgroup limits and project quotas with
-- local calls — `/proc/meminfo`, `statvfs`, `systemctl`, `xfs_quota` — so a
-- service host reached over `tcp://` would have every limit written on the API
-- machine while the containers ran somewhere else: not weaker enforcement,
-- none at all, and silently. "A service host is faber's own machine" is an
-- assumption everywhere else and a constraint here.
ALTER TABLE host ADD CONSTRAINT host_service_needs_endpoint
  CHECK (user_id IS NOT NULL OR docker_endpoint LIKE 'unix://%');

-- ---------------------------------------------------------------------------
-- Container ownership
-- ---------------------------------------------------------------------------

-- On a shared host the owner cannot be derived from `host.user_id`. Putting it
-- on the container makes the count check a single indexed predicate with no
-- join, and it is the only place authorization for a container on a service
-- host can happen.
ALTER TABLE host_container
  ADD COLUMN user_id UUID REFERENCES users(id) ON DELETE CASCADE;

UPDATE host_container c SET user_id = h.user_id FROM host h WHERE h.id = c.host_id;
ALTER TABLE host_container ALTER COLUMN user_id SET NOT NULL;

-- Count and storage bill on existence, so this counts registered containers
-- regardless of run state: a stopped container still holds its quota'd
-- directory and still counts.
CREATE INDEX host_container_owner ON host_container (host_id, user_id)
  WHERE unregistered_at IS NULL;

-- ---------------------------------------------------------------------------
-- Global subject identity
-- ---------------------------------------------------------------------------

-- Storage enforcement needs a stable 32-bit integer per user (XFS project IDs
-- are 32-bit), and the same integer serves as the container's host-side UID.
-- It cannot be hashed from a UUID: 32-bit birthday collision arrives around
-- 77k users.
--
-- Allocated globally, one per user, reused across every host — so audit logs
-- from different hosts join directly. That is the reading of "global" that
-- buys the correlation, and it is why allocation belongs to the user rather
-- than to a host binding.
--
-- 500 000 clears the normal Unix UID space and SYS_UID_MAX and stays under
-- 2^31. Project ID 0 is reserved by XFS. IDs are never reused, even after
-- release, which the sequence gives for free.
CREATE SEQUENCE subject_seq START WITH 500000 MINVALUE 500000 MAXVALUE 2000000000;

CREATE TABLE user_subject (
  user_id    UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  subject_id INT  NOT NULL UNIQUE DEFAULT nextval('subject_seq'),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- Per-host materialisation
-- ---------------------------------------------------------------------------

-- Creating a row IS the storage reservation. Storage is the one resource that
-- is reserved rather than overcommitted, because ENOSPC is global and cannot
-- be repaired by the user who caused it — CPU and RAM degrade under
-- contention, storage does not.
--
-- Materialised lazily on first use rather than eagerly for every account:
-- sum-over-all-users is unbounded on a service host, sum-over-materialised-
-- users is countable. A "max users per host" field falls out of that check
-- rather than needing a column.
CREATE TABLE host_user (
  id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  host_id     UUID NOT NULL REFERENCES host(id) ON DELETE CASCADE,
  user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  released_at TIMESTAMPTZ
);

CREATE UNIQUE INDEX host_user_live ON host_user (host_id, user_id)
  WHERE released_at IS NULL;

-- No snapshot of the granted storage lives here. The reservation sum joins
-- live rows to their resolved quotas, so a grant change cannot drift from the
-- reservation it implies.

-- ---------------------------------------------------------------------------
-- Sparse quota overrides
-- ---------------------------------------------------------------------------

-- A live row IS the resolved quota, in full — row-level replacement, never a
-- field-level merge. NULL here means unlimited, exactly as it does on `host`,
-- which is what makes override-to-unlimited expressible at all.
CREATE TABLE host_user_quota (
  id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  host_id       UUID NOT NULL REFERENCES host(id) ON DELETE CASCADE,
  user_id       UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  cpu_millis    INT,
  memory_bytes  BIGINT,
  storage_bytes BIGINT,
  container_max INT,
  granted_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
  -- Deliberately no FK: a grant stays auditable after the admin who made it
  -- is gone.
  granted_by    UUID,
  expires_at    TIMESTAMPTZ,
  retired_at    TIMESTAMPTZ,
  note          TEXT
);

CREATE UNIQUE INDEX host_user_quota_live ON host_user_quota (host_id, user_id)
  WHERE retired_at IS NULL;

-- ---------------------------------------------------------------------------
-- Service-provided images
-- ---------------------------------------------------------------------------

-- Letting a user run an arbitrary reference on faber's machine is arbitrary
-- code plus unbounded pull bandwidth plus image layers sitting outside any
-- project quota. `image.user_id` goes nullable by the same rule as `host`, and
-- the bind and spawn paths refuse a user image on a service host — a rule that
-- cannot be a CHECK because it spans two tables.
ALTER TABLE image ALTER COLUMN user_id DROP NOT NULL;
ALTER TABLE image DROP CONSTRAINT image_user_id_name_key;
CREATE UNIQUE INDEX image_name_owned   ON image (user_id, name) WHERE user_id IS NOT NULL;
CREATE UNIQUE INDEX image_name_service ON image (name)          WHERE user_id IS NULL;

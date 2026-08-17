-- A tenant gets real root, so the host has to say where that root lands.
--
-- Until now a service-host container ran as `subject:subject` — the same
-- integer that owned the tenant's directory and carried their project quota.
-- That made ownership on disk free: the container's only uid was the uid that
-- owned the tree, so a file written inside had the right owner outside with no
-- mapping at all.
--
-- It also made `apt-get install` impossible. A container pinned to an
-- unprivileged uid cannot write `/usr` or `/var/lib/dpkg`, and the read-only
-- root filesystem refused before that. Neither could simply be dropped: with
-- no user namespace, container-root *is* host-root, on a machine every tenant
-- shares.
--
-- The daemon now runs with `--userns-remap`, so container uid 0 is a mapped,
-- unprivileged uid on the host. Root inside is real root over the container
-- and nothing outside it. What that costs is stated plainly because it is a
-- deliberate trade: the mapping is daemon-wide, so every tenant's
-- container-root is the *same* host uid. Tenants are separated by mount
-- namespaces and by the fact that no tenant's directory is mounted into
-- another's container — not, any more, by owning distinct host uids. A
-- container escape lands as one uid with reach over every tenant's tree rather
-- than over one.
--
-- This column is the host uid that container uid 0 maps to: the first subuid
-- of the daemon's remap user, as `/etc/subuid` records it. Faber chowns each
-- tenant's directory to it so that root inside the container owns the tree it
-- is given.
--
-- It is a column rather than something faber reads off the machine. Everything
-- this codebase does to a host comes from configuration handed in, never from
-- the machine's or the process's own — that rule is what makes faber safe to
-- run as a multi-user service, and sniffing `/etc/subuid` for docker's default
-- remap username would be exactly the ambient resolution it exists to forbid.
-- An operator who configures the daemon records the number here, and a
-- mismatch is a provisioning error with one place to look.
--
-- `subject` is unaffected as an identifier. It is still the XFS project id and
-- still names the systemd slice; it has simply stopped being a uid.

ALTER TABLE host ADD COLUMN container_root_uid BIGINT;

COMMENT ON COLUMN host.container_root_uid IS
  'Host uid that container uid 0 maps to under the daemon''s userns-remap. Required for a service host, meaningless for an owned one.';

-- Mirrors host_service_needs_data_root. A service host without this has no
-- correct owner for a tenant directory, and the failure would otherwise arrive
-- as a container that cannot write its own workspace rather than as a refusal
-- at provisioning.
--
-- `NOT VALID`, which is the whole of what this comment is about.
--
-- A deployment may already have service hosts, and there is no value this
-- migration could give them. The right number is the first subuid of that
-- machine's docker remap user — a fact about a machine the database has never
-- seen and cannot ask. Backfilling a plausible-looking default (docker's usual
-- 231072, say) would be worse than leaving it null: the row would satisfy the
-- constraint, faber would chown tenant trees to a uid that machine's containers
-- do not run as, and every container would start successfully and be unable to
-- write. That is precisely the silent failure this column exists to prevent,
-- and a guessed value walks straight into it while looking correct.
--
-- So the constraint holds for every row written or updated from now on, and
-- says nothing about rows that predate it. An existing service host keeps its
-- null and is refused at launch, loudly and with the fix named, until an
-- operator reads `/etc/subuid` on that machine and sets the column.
--
-- Once every service host has one:
--
--   ALTER TABLE host VALIDATE CONSTRAINT host_service_needs_container_root_uid;
--
-- which takes no exclusive lock and is safe to run against a live deployment.
ALTER TABLE host ADD CONSTRAINT host_service_needs_container_root_uid
  CHECK (user_id IS NOT NULL OR container_root_uid IS NOT NULL) NOT VALID;

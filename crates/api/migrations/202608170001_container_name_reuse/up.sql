-- A withdrawn registration stops claiming its name.
--
-- `UNIQUE (host_id, container_ref)` was written when a `host_container` row
-- only ever meant "a user registered a container they made", and the ref was
-- the daemon's own id. Two later changes made that constraint say something
-- it was never meant to say.
--
-- First, `unregistered_at` arrived: a row is now tombstoned rather than
-- deleted, and the read paths filter it out. Second, the service-host launch
-- path inverted the write ordering — the row has to exist before the
-- admission lock releases, so `container_ref` there is the *name the user
-- chose*, written before the daemon has been asked anything.
--
-- Together those make a failed launch permanent. The row is tombstoned, so
-- every listing hides it; the constraint does not look at `unregistered_at`,
-- so it keeps the name forever. A user whose launch failed on the machine —
-- an unmountable root path, an image that will not pull — could never use
-- that name again, and the refusal reached them as a bare unique violation
-- from a row they could not see. Deleting a container you successfully made
-- and recreating it under the same name failed identically.
--
-- The predicate is the one every other live-set index in this schema already
-- uses (`host_user_live`, `host_user_quota_live`, and `host_container`'s own
-- two lookup indexes). Only the constraint, which predates the tombstone, was
-- left reading the whole table.
--
-- What still holds: two *live* registrations cannot share a ref on one host,
-- which is the invariant that matters — it is what stops two rows resolving
-- to one container on the machine. What is given up is history-uniqueness,
-- which nothing reads: a tombstoned row is an audit record of an attempt, and
-- the same name appearing twice in that history is the truth of what
-- happened, not a conflict.

ALTER TABLE host_container DROP CONSTRAINT host_container_host_id_container_ref_key;

CREATE UNIQUE INDEX host_container_ref_live ON host_container (host_id, container_ref)
  WHERE unregistered_at IS NULL;

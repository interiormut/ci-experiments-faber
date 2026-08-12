-- Containers faber created, told apart from containers faber was told about.
--
-- Until now every `host_container` row meant the same thing: a user made a
-- container and registered it, and faber's claim over it was knowing the ref.
-- Creating one is a different claim, and the difference has to be recorded
-- rather than inferred, because it is what decides whether destroying the
-- container is faber's to do.
--
-- `image_id` is provenance and nothing else. Nothing resolves through it — the
-- container's ref is what execution uses — and it is nullable and ON DELETE
-- SET NULL precisely so deleting a template can never be blocked by, or cascade
-- into, a container that outlived it.

ALTER TABLE host_container
  ADD COLUMN managed_at timestamptz,
  ADD COLUMN image_id   uuid REFERENCES image(id) ON DELETE SET NULL;

COMMENT ON COLUMN host_container.managed_at IS
  'When faber created this container. NULL means the user did, and faber only registered it.';

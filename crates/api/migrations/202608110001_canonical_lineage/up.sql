-- The canonical lineage a run committed: the `Vec<llm::Message>` Core holds as
-- ground truth, serialized as a `harness::Seed`.
--
-- Deliberately *not* `request_blob_digest`. That column holds the provider's
-- rendered request bytes, and recovering a message list from them is the lossy
-- reconstruction path the id model exists to delete: `ThinkingContent.signature`
-- is opaque and must survive verbatim, and `UnknownContent` exists precisely so
-- a block that arrives can be sent back. Two different artifacts, two columns.
--
-- Nullable because an exchange that was never committed has no lineage attached
-- — best-of-N losers, scratch classifications, and repair probes all produce
-- exchanges that no spine row ever names.
ALTER TABLE exchange
    ADD COLUMN canonical_blob_digest BYTEA REFERENCES blob (digest);

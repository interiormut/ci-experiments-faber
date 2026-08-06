-- A personal workspace is identified by user_id, not by convention.  The
-- UNIQUE constraint is what enforces "at most one user workspace per user";
-- the CHECK ties that column to kind so a common workspace cannot claim an
-- owner and a user workspace cannot lack one.  Deleting the account deletes
-- the workspace by cascade, so no separate deletion protocol is needed.
CREATE TABLE workspace (
    id UUID PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('user', 'common')),
    user_id UUID UNIQUE REFERENCES users (id) ON DELETE CASCADE,
    created_at BIGINT NOT NULL,
    CHECK ((kind = 'user') = (user_id IS NOT NULL))
);

-- Existing users receive their required personal workspace.
INSERT INTO workspace (id, kind, user_id, created_at)
SELECT gen_random_uuid(), 'user', id, EXTRACT(EPOCH FROM created_at)::BIGINT
FROM users;

CREATE TABLE workspace_member (
    workspace_id UUID NOT NULL REFERENCES workspace (id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('owner', 'member')),
    joined_at BIGINT NOT NULL,
    PRIMARY KEY (workspace_id, user_id)
);

INSERT INTO workspace_member (workspace_id, user_id, role, joined_at)
SELECT id, user_id, 'owner', created_at
FROM workspace
WHERE kind = 'user';

CREATE UNIQUE INDEX workspace_one_owner_idx
ON workspace_member (workspace_id)
WHERE role = 'owner';

-- "Which workspaces am I in", and the cascade path from users.
CREATE INDEX workspace_member_user_id_idx ON workspace_member (user_id);

CREATE FUNCTION create_user_workspace() RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog, public, pg_temp
AS $$
DECLARE
    workspace_id UUID := gen_random_uuid();
    created BIGINT := EXTRACT(EPOCH FROM NEW.created_at)::BIGINT;
BEGIN
    INSERT INTO workspace (id, kind, user_id, created_at)
    VALUES (workspace_id, 'user', NEW.id, created);
    INSERT INTO workspace_member (workspace_id, user_id, role, joined_at)
    VALUES (workspace_id, NEW.id, 'owner', created);
    RETURN NEW;
END;
$$;

CREATE TRIGGER users_create_workspace
AFTER INSERT ON users
FOR EACH ROW EXECUTE FUNCTION create_user_workspace();

-- A referential cascade fires only after the parent row is already gone, so
-- "the owning user no longer exists" distinguishes account deletion from a
-- direct attempt to drop somebody's personal workspace.  That structural test
-- replaces an ambient session flag, which any caller could have set.
CREATE FUNCTION protect_user_workspace() RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog, public, pg_temp
AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        IF OLD.kind = 'user'
           AND EXISTS (SELECT 1 FROM users WHERE id = OLD.user_id) THEN
            RAISE EXCEPTION
                'a user workspace cannot be deleted except with its account';
        END IF;
        RETURN OLD;
    END IF;
    IF NEW.kind IS DISTINCT FROM OLD.kind THEN
        RAISE EXCEPTION 'workspace kind is immutable';
    END IF;
    IF NEW.user_id IS DISTINCT FROM OLD.user_id THEN
        RAISE EXCEPTION 'workspace ownership is immutable';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER workspace_owner_only
BEFORE DELETE OR UPDATE OF kind, user_id ON workspace
FOR EACH ROW EXECUTE FUNCTION protect_user_workspace();

-- The sole member of a user workspace is its owner.  Requiring the inserted
-- row to match workspace.user_id makes that exact, and the primary key then
-- rules out a second member without counting rows -- so there is no window in
-- which two concurrent inserts both observe an empty workspace.
CREATE FUNCTION enforce_workspace_membership() RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog, public, pg_temp
AS $$
DECLARE
    ws workspace%ROWTYPE;
    old_kind TEXT;
BEGIN
    IF TG_OP = 'DELETE' THEN
        SELECT * INTO ws FROM workspace WHERE id = OLD.workspace_id;
        -- Found only on a direct delete: both legitimate cascades (dropping
        -- the workspace, deleting the account) reach here with their parent
        -- row already removed.
        IF FOUND
           AND ws.kind = 'user'
           AND EXISTS (SELECT 1 FROM users WHERE id = OLD.user_id) THEN
            RAISE EXCEPTION
                'user workspace owner membership cannot be deleted';
        END IF;
        RETURN OLD;
    END IF;

    SELECT * INTO ws FROM workspace WHERE id = NEW.workspace_id;

    IF TG_OP = 'UPDATE' THEN
        SELECT kind INTO old_kind FROM workspace WHERE id = OLD.workspace_id;
        IF ws.kind = 'user' OR old_kind = 'user' THEN
            RAISE EXCEPTION 'user workspace owner membership is immutable';
        END IF;
        RETURN NEW;
    END IF;

    IF ws.kind = 'user'
       AND (NEW.role <> 'owner' OR NEW.user_id <> ws.user_id) THEN
        RAISE EXCEPTION 'user workspaces have exactly one owner member';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER workspace_membership_policy
BEFORE INSERT OR UPDATE OR DELETE ON workspace_member
FOR EACH ROW EXECUTE FUNCTION enforce_workspace_membership();

CREATE TABLE session (
    id UUID PRIMARY KEY,             -- uuidv7
    workspace_id UUID NOT NULL REFERENCES workspace (id) ON DELETE CASCADE,
    title TEXT,
    created_at BIGINT NOT NULL,
    closed_at BIGINT
);

CREATE INDEX session_workspace_id_idx ON session (workspace_id);

CREATE TABLE session_ref (                    -- opaque git-trailer token
    token TEXT PRIMARY KEY,               -- ≥128-bit CSPRNG, base32
    session_id UUID NOT NULL REFERENCES session (id) ON DELETE CASCADE,
    issued_at BIGINT NOT NULL,
    revoked_at BIGINT
);

CREATE TABLE thread (
    id UUID PRIMARY KEY,
    session_id UUID NOT NULL REFERENCES session (id) ON DELETE CASCADE,
    parent_id UUID REFERENCES thread (id) ON DELETE CASCADE,
    forked_at_seq INTEGER,                     -- inclusive
    next_seq INTEGER NOT NULL DEFAULT 0,  -- Core-owned allocator
    created_at BIGINT NOT NULL,
    CHECK ((parent_id IS NULL) = (forked_at_seq IS NULL))
);

-- A run groups the exchange and transcript records produced by one harness
-- invocation.  The design only requires this identity and its thread scope;
-- workflow state belongs outside this history schema.
CREATE TABLE run (
    id UUID PRIMARY KEY,
    thread_id UUID NOT NULL REFERENCES thread (id) ON DELETE CASCADE,
    created_at BIGINT NOT NULL,
    completed_at BIGINT,
    CHECK (completed_at IS NULL OR completed_at >= created_at)
);

-- Content-addressed payloads used by exchanges and references.  Large values
-- may be moved to disk while retaining their digest as the stable identity.
CREATE TABLE blob (
    digest BYTEA PRIMARY KEY,
    data BYTEA,
    storage_path TEXT,
    byte_length BIGINT NOT NULL,
    refcount BIGINT NOT NULL DEFAULT 0 CHECK (refcount >= 0),
    created_at BIGINT NOT NULL,
    CHECK ((data IS NOT NULL) OR (storage_path IS NOT NULL))
);

CREATE TABLE exchange (
    id UUID PRIMARY KEY,
    run_id UUID NOT NULL REFERENCES run (id) ON DELETE CASCADE,
    request_blob_digest BYTEA NOT NULL REFERENCES blob (digest),
    provider_events_digest BYTEA REFERENCES blob (digest),
    usage JSONB,
    outcome JSONB,
    expected_cache_tokens BIGINT NOT NULL CHECK (expected_cache_tokens >= 0),
    actual_cache_tokens BIGINT CHECK (
        actual_cache_tokens IS NULL OR actual_cache_tokens >= 0
    ),
    started_at BIGINT NOT NULL,
    completed_at BIGINT,
    CHECK (completed_at IS NULL OR completed_at >= started_at)
);

-- A span is a byte-preserving reference into the exact request blob sent by
-- Core.  end_offset is exclusive.
CREATE TABLE span (
    id UUID PRIMARY KEY,
    exchange_id UUID NOT NULL REFERENCES exchange (id) ON DELETE CASCADE,
    start_offset BIGINT NOT NULL CHECK (start_offset >= 0),
    end_offset BIGINT NOT NULL CHECK (end_offset > start_offset),
    created_at BIGINT NOT NULL,
    UNIQUE (exchange_id, start_offset, end_offset)
);

CREATE TABLE transcript (
    id UUID PRIMARY KEY,
    run_id UUID NOT NULL REFERENCES run (id) ON DELETE CASCADE,
    seq BIGINT NOT NULL CHECK (seq >= 0),
    kind TEXT NOT NULL,
    payload JSONB NOT NULL,
    created_at BIGINT NOT NULL,
    UNIQUE (run_id, seq)
);

-- The selected exchange for each position in a thread's canonical history.
-- explicit_commit distinguishes an intentional best-of-N selection from the
-- implicit last-request default.
CREATE TABLE spine (
    thread_id UUID NOT NULL REFERENCES thread (id) ON DELETE CASCADE,
    seq BIGINT NOT NULL CHECK (seq >= 0),
    exchange_id UUID NOT NULL REFERENCES exchange (id) ON DELETE CASCADE,
    explicit_commit BOOLEAN NOT NULL DEFAULT FALSE,
    created_at BIGINT NOT NULL,
    PRIMARY KEY (thread_id, seq)
);

CREATE INDEX exchange_run_id_idx ON exchange (run_id);
CREATE INDEX transcript_run_id_idx ON transcript (run_id, seq);
CREATE INDEX spine_exchange_id_idx ON spine (exchange_id);
CREATE INDEX span_exchange_id_idx ON span (exchange_id);

-- Deleting a whole session is an ordinary product action, and its history
-- goes with it.  Discarding a single run out from under a live thread is not:
-- the spine still refers to it.  So a run outlives its thread by exactly zero
-- statements, and cannot be removed on its own.
CREATE FUNCTION protect_run() RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog, public, pg_temp
AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM thread WHERE id = OLD.thread_id) THEN
        RAISE EXCEPTION 'a run cannot be deleted while its thread exists';
    END IF;
    RETURN OLD;
END;
$$;

CREATE TRIGGER run_delete_with_thread
BEFORE DELETE ON run
FOR EACH ROW EXECUTE FUNCTION protect_run();

-- History is append-only.  Rows disappear only when the run they belong to
-- does, which by the same argument as above happens on cascade alone.
CREATE FUNCTION reject_history_mutation() RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog, public, pg_temp
AS $$
BEGIN
    IF TG_OP = 'DELETE'
       AND NOT EXISTS (SELECT 1 FROM run WHERE id = OLD.run_id) THEN
        RETURN OLD;
    END IF;
    RAISE EXCEPTION '% is append-only', TG_TABLE_NAME;
END;
$$;

CREATE TRIGGER exchange_append_only
BEFORE UPDATE OR DELETE ON exchange
FOR EACH ROW EXECUTE FUNCTION reject_history_mutation();

CREATE TRIGGER transcript_append_only
BEFORE UPDATE OR DELETE ON transcript
FOR EACH ROW EXECUTE FUNCTION reject_history_mutation();

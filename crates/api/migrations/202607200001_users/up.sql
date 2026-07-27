CREATE TABLE users (
    id           UUID PRIMARY KEY,
    identity_id  UUID NOT NULL UNIQUE,
    username     VARCHAR NOT NULL UNIQUE,
    display_name TEXT NOT NULL DEFAULT '',
    avatar_url   TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

ALTER TABLE users
    ADD COLUMN username     VARCHAR NOT NULL DEFAULT '',
    ADD COLUMN display_name TEXT NOT NULL DEFAULT '',
    ADD COLUMN avatar_url   TEXT;

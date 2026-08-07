CREATE TABLE credentials (
  id             uuid        PRIMARY KEY,
  user_id       uuid        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  label          text        NOT NULL,
  key_ciphertext bytea       NOT NULL,
  key_nonce      bytea       NOT NULL,
  key_version    text        NOT NULL,
  last_four      text        NOT NULL,
  created_at     timestamptz NOT NULL DEFAULT now(),
  UNIQUE (user_id, label)
);

CREATE INDEX credentials_user_id_idx ON credentials (user_id);

CREATE TABLE models (
  id            uuid  PRIMARY KEY,
  user_id      uuid  NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  alias         text  NOT NULL,
  base_url      text  NOT NULL,
  wire          text  NOT NULL CHECK (wire IN ('openai', 'anthropic')),
  wire_id       text  NOT NULL,
  family        text,
  credential_id uuid  REFERENCES credentials(id) ON DELETE SET NULL,
  params        jsonb NOT NULL DEFAULT '{}',
  capabilities  jsonb NOT NULL DEFAULT '{}',
  created_at    timestamptz NOT NULL DEFAULT now(),
  UNIQUE (user_id, alias)
);

CREATE INDEX models_user_id_idx ON models (user_id);

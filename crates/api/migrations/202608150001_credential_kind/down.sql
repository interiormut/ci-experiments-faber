ALTER TABLE credentials
  DROP CONSTRAINT IF EXISTS credentials_kind_check;

ALTER TABLE credentials
  DROP COLUMN IF EXISTS kind;

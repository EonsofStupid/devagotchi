-- The hub's accounts, passkeys, one-time links, browser sessions and its own secrets.
-- Kept in step with ../sqlite/0001_accounts.sql: same tables, columns and meaning.
-- Times are Unix seconds; flags are 0 or 1 (BIGINT on both backends, one code path).
CREATE TABLE IF NOT EXISTS accounts (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  owner BIGINT NOT NULL DEFAULT 0,
  email TEXT,
  email_verified_at BIGINT,
  created_at BIGINT NOT NULL,
  disabled_at BIGINT
);
CREATE UNIQUE INDEX IF NOT EXISTS one_owner ON accounts (owner) WHERE owner = 1;
CREATE TABLE IF NOT EXISTS links (
  hash TEXT PRIMARY KEY,
  purpose TEXT NOT NULL,
  account TEXT NOT NULL REFERENCES accounts (id),
  expires_at BIGINT NOT NULL,
  used_at BIGINT,
  replaces BIGINT NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS secrets (
  name TEXT PRIMARY KEY,
  value BYTEA NOT NULL
);
CREATE TABLE IF NOT EXISTS spent_challenges (
  mac BYTEA PRIMARY KEY,
  expires_at BIGINT NOT NULL
);
CREATE TABLE IF NOT EXISTS passkeys (
  id BYTEA PRIMARY KEY,
  account TEXT NOT NULL REFERENCES accounts (id),
  label TEXT NOT NULL,
  public_key BYTEA NOT NULL,
  alg BIGINT NOT NULL,
  sign_count BIGINT NOT NULL,
  backup_eligible BIGINT NOT NULL,
  created_at BIGINT NOT NULL,
  last_used_at BIGINT
);
CREATE INDEX IF NOT EXISTS passkeys_by_account ON passkeys (account);
CREATE TABLE IF NOT EXISTS sessions (
  hash TEXT PRIMARY KEY,
  account TEXT NOT NULL REFERENCES accounts (id),
  passkey BYTEA REFERENCES passkeys (id) ON DELETE CASCADE,
  created_at BIGINT NOT NULL,
  last_seen BIGINT NOT NULL,
  expires_at BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS sessions_by_account ON sessions (account);

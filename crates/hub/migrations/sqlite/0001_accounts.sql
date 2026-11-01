-- The hub's accounts, passkeys, one-time links, browser sessions and its own secrets.
-- Kept in step with ../postgres/0001_accounts.sql: same tables, columns and meaning.
-- IF NOT EXISTS: a store made before migrations existed takes this as already applied.
CREATE TABLE IF NOT EXISTS accounts (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  owner INTEGER NOT NULL DEFAULT 0,
  email TEXT,
  email_verified_at INTEGER,
  created_at INTEGER NOT NULL,
  disabled_at INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS one_owner ON accounts (owner) WHERE owner = 1;
CREATE TABLE IF NOT EXISTS links (
  hash TEXT PRIMARY KEY,
  purpose TEXT NOT NULL,
  account TEXT NOT NULL REFERENCES accounts (id),
  expires_at INTEGER NOT NULL,
  used_at INTEGER,
  replaces INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS secrets (
  name TEXT PRIMARY KEY,
  value BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS spent_challenges (
  mac BLOB PRIMARY KEY,
  expires_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS passkeys (
  id BLOB PRIMARY KEY,
  account TEXT NOT NULL REFERENCES accounts (id),
  label TEXT NOT NULL,
  public_key BLOB NOT NULL,
  alg INTEGER NOT NULL,
  sign_count INTEGER NOT NULL,
  backup_eligible INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  last_used_at INTEGER
);
CREATE INDEX IF NOT EXISTS passkeys_by_account ON passkeys (account);
CREATE TABLE IF NOT EXISTS sessions (
  hash TEXT PRIMARY KEY,
  account TEXT NOT NULL REFERENCES accounts (id),
  passkey BLOB REFERENCES passkeys (id) ON DELETE CASCADE,
  created_at INTEGER NOT NULL,
  last_seen INTEGER NOT NULL,
  expires_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS sessions_by_account ON sessions (account);

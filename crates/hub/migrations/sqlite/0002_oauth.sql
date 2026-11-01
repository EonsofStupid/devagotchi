-- The hub as its own OAuth 2.1 authorization server for AI clients (32b): registered
-- clients, one-time authorization codes and opaque tokens, every secret as its SHA-256.
-- Kept in step with ../postgres/0002_oauth.sql.
CREATE TABLE oauth_clients (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  redirect_uris TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  last_used INTEGER
);
CREATE TABLE oauth_codes (
  hash TEXT PRIMARY KEY,
  client TEXT NOT NULL,
  account TEXT NOT NULL REFERENCES accounts (id),
  redirect_uri TEXT NOT NULL,
  challenge TEXT NOT NULL,
  resource TEXT NOT NULL,
  scope TEXT NOT NULL,
  expires_at INTEGER NOT NULL
);
CREATE TABLE oauth_tokens (
  hash TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  family TEXT NOT NULL,
  client TEXT NOT NULL,
  account TEXT NOT NULL REFERENCES accounts (id),
  scope TEXT NOT NULL,
  resource TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  used_at INTEGER
);
CREATE INDEX oauth_tokens_by_family ON oauth_tokens (family);
CREATE INDEX oauth_tokens_by_account ON oauth_tokens (account);
CREATE INDEX oauth_tokens_by_expiry ON oauth_tokens (expires_at);

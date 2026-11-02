-- Tenants and the accounts a hosted hub makes on first connect (accounts/tenants.rs). Every
-- account belongs to one tenant `t-<uuid>`; the hub's first tenant (`home`) is its
-- operator's, and a one-owner hub's only one: accounts made before this migration join it
-- when the hub opens the store. An account nobody has secured yet (`provisional`) ends
-- after it goes unused (`last_active`); `identities` are the sign-ins linked to an account;
-- `account_starts` rate-limits new accounts per client address (kept as an HMAC, never the
-- address); `tenant_events` is each tenant's own hash chain of what happened to it.
-- Add-only. Kept in step with ../postgres/0005_tenancy.sql.
CREATE TABLE tenants (
  id TEXT PRIMARY KEY,
  home INTEGER NOT NULL DEFAULT 0,
  slug TEXT,
  isolation TEXT NOT NULL DEFAULT 'pooled',
  created_at INTEGER NOT NULL,
  disabled_at INTEGER
);
CREATE UNIQUE INDEX one_home_tenant ON tenants (home) WHERE home = 1;
CREATE UNIQUE INDEX tenants_by_slug ON tenants (slug) WHERE slug IS NOT NULL;
ALTER TABLE accounts ADD COLUMN tenant TEXT REFERENCES tenants (id);
ALTER TABLE accounts ADD COLUMN provisional INTEGER NOT NULL DEFAULT 0;
ALTER TABLE accounts ADD COLUMN last_active INTEGER;
CREATE INDEX accounts_by_tenant ON accounts (tenant);
CREATE TABLE identities (
  provider TEXT NOT NULL,
  subject TEXT NOT NULL,
  account TEXT NOT NULL REFERENCES accounts (id),
  email TEXT,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (provider, subject)
);
CREATE INDEX identities_by_account ON identities (account);
CREATE TABLE account_starts (
  source TEXT NOT NULL,
  at INTEGER NOT NULL
);
CREATE INDEX account_starts_by_source ON account_starts (source, at);
CREATE TABLE tenant_events (
  tenant TEXT NOT NULL REFERENCES tenants (id),
  seq INTEGER NOT NULL,
  at INTEGER NOT NULL,
  actor TEXT NOT NULL,
  event TEXT NOT NULL,
  detail TEXT NOT NULL,
  prev_hash TEXT NOT NULL,
  hash TEXT NOT NULL,
  PRIMARY KEY (tenant, seq)
);

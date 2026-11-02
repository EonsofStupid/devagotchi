-- no-transaction
-- Database-enforced isolation of tenants (increment 60), SQLite's half: the same tables
-- and columns as ../postgres/0007_tenant_rls.sql, with no schemas and no row-level
-- security (a standalone hub's store is its own file; the hub's code scopes every query
-- the same way on both). The one migration that does not only add: an older hub cannot
-- read the store after it, so a rollback across it restores a backup taken before.
--
-- SQLite changes a key only by building the table again: the old tables are renamed
-- aside, the new ones made with tenant_id leading their keys, the rows copied across
-- joined to their account (so each row's tenant is its account's), and the old ones
-- dropped, all in one transaction.
PRAGMA foreign_keys = OFF;
BEGIN IMMEDIATE;

ALTER TABLE accounts RENAME TO old_accounts;
ALTER TABLE passkeys RENAME TO old_passkeys;
ALTER TABLE sessions RENAME TO old_sessions;
ALTER TABLE links RENAME TO old_links;
ALTER TABLE identities RENAME TO old_identities;
ALTER TABLE oauth_codes RENAME TO old_oauth_codes;
ALTER TABLE oauth_tokens RENAME TO old_oauth_tokens;
ALTER TABLE oauth_devices RENAME TO old_oauth_devices;
ALTER TABLE email_links RENAME TO old_email_links;
ALTER TABLE tenant_events RENAME TO old_tenant_events;
ALTER TABLE tenants RENAME TO tenancy_tenants;
ALTER TABLE secrets RENAME TO hub_secrets;
ALTER TABLE spent_challenges RENAME TO hub_spent_challenges;
ALTER TABLE claim_codes RENAME TO auth_claim_codes;
ALTER TABLE account_starts RENAME TO auth_account_starts;
ALTER TABLE email_sends RENAME TO auth_email_sends;

-- Accounts made before tenants (a store that skipped 0005's settling) join the first one.
INSERT INTO tenancy_tenants (id, home, created_at)
  SELECT 't-' || lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4'
         || substr(lower(hex(randomblob(2))), 2) || '-'
         || substr('89ab', 1 + (abs(random()) % 4), 1) || substr(lower(hex(randomblob(2))), 2)
         || '-' || lower(hex(randomblob(6))),
         1, CAST(strftime('%s', 'now') AS INTEGER)
  WHERE NOT EXISTS (SELECT 1 FROM tenancy_tenants WHERE home = 1)
    AND EXISTS (SELECT 1 FROM old_accounts);
UPDATE old_accounts SET tenant = (SELECT id FROM tenancy_tenants WHERE home = 1)
  WHERE tenant IS NULL;

CREATE TABLE auth_accounts (
  tenant_id TEXT NOT NULL REFERENCES tenancy_tenants (id),
  id TEXT NOT NULL UNIQUE,
  name TEXT NOT NULL,
  owner INTEGER NOT NULL DEFAULT 0,
  email TEXT,
  email_verified_at INTEGER,
  created_at INTEGER NOT NULL,
  disabled_at INTEGER,
  provisional INTEGER NOT NULL DEFAULT 0,
  last_active INTEGER,
  PRIMARY KEY (tenant_id, id)
);
INSERT INTO auth_accounts (tenant_id, id, name, owner, email, email_verified_at, created_at,
    disabled_at, provisional, last_active)
  SELECT tenant, id, name, owner, email, email_verified_at, created_at, disabled_at,
    provisional, last_active FROM old_accounts;

CREATE TABLE auth_passkeys (
  tenant_id TEXT NOT NULL,
  id BLOB NOT NULL UNIQUE,
  account TEXT NOT NULL,
  label TEXT NOT NULL,
  public_key BLOB NOT NULL,
  alg INTEGER NOT NULL,
  sign_count INTEGER NOT NULL,
  backup_eligible INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  last_used_at INTEGER,
  PRIMARY KEY (tenant_id, id),
  FOREIGN KEY (tenant_id, account) REFERENCES auth_accounts (tenant_id, id)
);
INSERT INTO auth_passkeys (tenant_id, id, account, label, public_key, alg, sign_count,
    backup_eligible, created_at, last_used_at)
  SELECT a.tenant_id, p.id, p.account, p.label, p.public_key, p.alg, p.sign_count,
    p.backup_eligible, p.created_at, p.last_used_at
  FROM old_passkeys p JOIN auth_accounts a ON a.id = p.account;

CREATE TABLE auth_sessions (
  tenant_id TEXT NOT NULL,
  hash TEXT NOT NULL UNIQUE,
  account TEXT NOT NULL,
  passkey BLOB,
  created_at INTEGER NOT NULL,
  last_seen INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, hash),
  FOREIGN KEY (tenant_id, account) REFERENCES auth_accounts (tenant_id, id),
  FOREIGN KEY (tenant_id, passkey) REFERENCES auth_passkeys (tenant_id, id) ON DELETE CASCADE
);
INSERT INTO auth_sessions (tenant_id, hash, account, passkey, created_at, last_seen, expires_at)
  SELECT a.tenant_id, s.hash, s.account, s.passkey, s.created_at, s.last_seen, s.expires_at
  FROM old_sessions s JOIN auth_accounts a ON a.id = s.account;

CREATE TABLE auth_links (
  tenant_id TEXT NOT NULL,
  hash TEXT NOT NULL UNIQUE,
  purpose TEXT NOT NULL,
  account TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  used_at INTEGER,
  replaces INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (tenant_id, hash),
  FOREIGN KEY (tenant_id, account) REFERENCES auth_accounts (tenant_id, id)
);
INSERT INTO auth_links (tenant_id, hash, purpose, account, expires_at, used_at, replaces)
  SELECT a.tenant_id, l.hash, l.purpose, l.account, l.expires_at, l.used_at, l.replaces
  FROM old_links l JOIN auth_accounts a ON a.id = l.account;

CREATE TABLE auth_identities (
  tenant_id TEXT NOT NULL,
  provider TEXT NOT NULL,
  subject TEXT NOT NULL,
  account TEXT NOT NULL,
  email TEXT,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, provider, subject),
  UNIQUE (provider, subject),
  FOREIGN KEY (tenant_id, account) REFERENCES auth_accounts (tenant_id, id)
);
INSERT INTO auth_identities (tenant_id, provider, subject, account, email, created_at)
  SELECT a.tenant_id, i.provider, i.subject, i.account, i.email, i.created_at
  FROM old_identities i JOIN auth_accounts a ON a.id = i.account;

CREATE TABLE oauth_codes (
  tenant_id TEXT NOT NULL,
  hash TEXT NOT NULL UNIQUE,
  client TEXT NOT NULL,
  account TEXT NOT NULL,
  redirect_uri TEXT NOT NULL,
  challenge TEXT NOT NULL,
  resource TEXT NOT NULL,
  scope TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, hash),
  FOREIGN KEY (tenant_id, account) REFERENCES auth_accounts (tenant_id, id)
);
INSERT INTO oauth_codes (tenant_id, hash, client, account, redirect_uri, challenge, resource,
    scope, expires_at)
  SELECT a.tenant_id, c.hash, c.client, c.account, c.redirect_uri, c.challenge, c.resource,
    c.scope, c.expires_at
  FROM old_oauth_codes c JOIN auth_accounts a ON a.id = c.account;

CREATE TABLE oauth_tokens (
  tenant_id TEXT NOT NULL,
  hash TEXT NOT NULL UNIQUE,
  kind TEXT NOT NULL,
  family TEXT NOT NULL,
  client TEXT NOT NULL,
  account TEXT NOT NULL,
  scope TEXT NOT NULL,
  resource TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  used_at INTEGER,
  PRIMARY KEY (tenant_id, hash),
  FOREIGN KEY (tenant_id, account) REFERENCES auth_accounts (tenant_id, id)
);
INSERT INTO oauth_tokens (tenant_id, hash, kind, family, client, account, scope, resource,
    created_at, expires_at, used_at)
  SELECT a.tenant_id, t.hash, t.kind, t.family, t.client, t.account, t.scope, t.resource,
    t.created_at, t.expires_at, t.used_at
  FROM old_oauth_tokens t JOIN auth_accounts a ON a.id = t.account;

CREATE TABLE oauth_devices (
  hash TEXT PRIMARY KEY,
  user_code TEXT NOT NULL UNIQUE,
  client TEXT NOT NULL,
  scope TEXT NOT NULL,
  resource TEXT NOT NULL,
  account TEXT REFERENCES auth_accounts (id),
  denied INTEGER NOT NULL DEFAULT 0,
  polled_at INTEGER,
  expires_at INTEGER NOT NULL
);
INSERT INTO oauth_devices SELECT * FROM old_oauth_devices;

CREATE TABLE auth_email_links (
  hash TEXT PRIMARY KEY,
  email TEXT NOT NULL,
  account TEXT REFERENCES auth_accounts (id),
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  used_at INTEGER
);
INSERT INTO auth_email_links SELECT * FROM old_email_links;

CREATE TABLE tenancy_events (
  tenant_id TEXT NOT NULL REFERENCES tenancy_tenants (id),
  seq INTEGER NOT NULL,
  at INTEGER NOT NULL,
  actor TEXT NOT NULL,
  event TEXT NOT NULL,
  detail TEXT NOT NULL,
  prev_hash TEXT NOT NULL,
  hash TEXT NOT NULL,
  PRIMARY KEY (tenant_id, seq)
);
INSERT INTO tenancy_events (tenant_id, seq, at, actor, event, detail, prev_hash, hash)
  SELECT tenant, seq, at, actor, event, detail, prev_hash, hash FROM old_tenant_events;

DROP TABLE old_tenant_events;
DROP TABLE old_email_links;
DROP TABLE old_oauth_devices;
DROP TABLE old_oauth_tokens;
DROP TABLE old_oauth_codes;
DROP TABLE old_identities;
DROP TABLE old_links;
DROP TABLE old_sessions;
DROP TABLE old_passkeys;
DROP TABLE old_accounts;

CREATE UNIQUE INDEX one_owner ON auth_accounts (owner) WHERE owner = 1;
CREATE INDEX accounts_by_tenant ON auth_accounts (tenant_id);
CREATE INDEX passkeys_by_account ON auth_passkeys (account);
CREATE INDEX sessions_by_account ON auth_sessions (account);
CREATE INDEX identities_by_account ON auth_identities (account);
CREATE INDEX auth_identities_by_email ON auth_identities (email);
CREATE INDEX oauth_tokens_by_family ON oauth_tokens (family);
CREATE INDEX oauth_tokens_by_account ON oauth_tokens (account);
CREATE INDEX oauth_tokens_by_expiry ON oauth_tokens (expires_at);
CREATE INDEX oauth_devices_by_expiry ON oauth_devices (expires_at);
CREATE INDEX email_links_by_email ON auth_email_links (email, created_at);

COMMIT;
PRAGMA foreign_keys = ON;

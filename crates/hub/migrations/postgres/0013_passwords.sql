-- Password sign-in (spec first-connect, PR A1; accounts/passwords.rs): an email and a
-- password make an account on a hosted hub, and sign in to it.
--
-- passwords: one row per account that has a password. `email` is trimmed and lowercased,
--   one account per address across the hub; `hash` is the argon2id PHC string, never the
--   password. The account is made by the same path an identity sign-in takes, with the
--   linked sign-in ('email', <email>) beside it (auth_identities): that sign-in is how a
--   password sign-in finds its tenant (devagotchi.tenant_of 'identity'), so this table needs
--   no lookup of its own. `updated_at` moves when the password is set again.
-- A tenant's rows like the others since 0007: tenant_id with the account as (tenant_id,
-- account_id), and row-level security, forced, shows a row only to a transaction scoped to
-- its tenant. Add-only. Kept in step with ../sqlite/0013_passwords.sql.

CREATE TABLE devagotchi.passwords (
  account_id TEXT NOT NULL PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  email TEXT NOT NULL,
  hash TEXT NOT NULL,
  created_at BIGINT NOT NULL,
  updated_at BIGINT NOT NULL,
  CONSTRAINT passwords_email UNIQUE (email),
  FOREIGN KEY (tenant_id, account_id) REFERENCES devagotchi.auth_accounts (tenant_id, id)
);

ALTER TABLE devagotchi.passwords ENABLE ROW LEVEL SECURITY;
ALTER TABLE devagotchi.passwords FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_only ON devagotchi.passwords
  USING (tenant_id = current_setting('devagotchi.tenant', true))
  WITH CHECK (tenant_id = current_setting('devagotchi.tenant', true));

DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'devagotchi_hub') THEN
    GRANT SELECT, INSERT, UPDATE, DELETE ON devagotchi.passwords TO devagotchi_hub;
  END IF;
END $$;

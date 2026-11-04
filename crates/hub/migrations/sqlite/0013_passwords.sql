-- Password sign-in (spec first-connect, PR A1; accounts/passwords.rs): the same table and
-- columns as ../postgres/0013_passwords.sql, which says what each holds (no row-level
-- security on SQLite, whose store is one hub's own file; the same queries scope the same
-- way). Add-only.
CREATE TABLE passwords (
  account_id TEXT NOT NULL PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  email TEXT NOT NULL UNIQUE,
  hash TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  FOREIGN KEY (tenant_id, account_id) REFERENCES auth_accounts (tenant_id, id)
);

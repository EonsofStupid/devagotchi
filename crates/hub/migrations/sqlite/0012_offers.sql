-- The site's offers (increment 73, src/offers.rs): the same table and columns as
-- ../postgres/0012_offers.sql, which says what each holds (no row-level security on SQLite,
-- whose store is one hub's own file; the same queries scope the same way). Add-only.
CREATE TABLE tenancy_offers (
  tenant_id TEXT NOT NULL PRIMARY KEY,
  account TEXT NOT NULL,
  consent TEXT NOT NULL,
  offerings TEXT NOT NULL DEFAULT '[]',
  accepted_at INTEGER NOT NULL,
  link_state TEXT NOT NULL DEFAULT 'pending' CHECK (link_state IN ('pending', 'linked')),
  linked_via TEXT CHECK (linked_via IN ('provisioned', 'sign_in')),
  linked_at INTEGER,
  link_hash TEXT UNIQUE,
  link_expires_at INTEGER,
  FOREIGN KEY (tenant_id, account) REFERENCES auth_accounts (tenant_id, id)
);

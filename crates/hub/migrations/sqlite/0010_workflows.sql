-- Workflows with direct links (increment 68, src/workflows.rs): the same tables and columns
-- as ../postgres/0010_workflows.sql, which says what each holds (no row-level security on
-- SQLite, whose store is one hub's own file; the same queries scope the same way). Add-only.
CREATE TABLE tenancy_workflows (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  account TEXT NOT NULL,
  workflow TEXT NOT NULL,
  definition TEXT NOT NULL,
  link_hash TEXT NOT NULL UNIQUE,
  machine_name TEXT,
  state TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'completed', 'cancelled')),
  progress TEXT NOT NULL DEFAULT '{}',
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  ended_at INTEGER,
  PRIMARY KEY (tenant_id, id),
  FOREIGN KEY (tenant_id, account) REFERENCES auth_accounts (tenant_id, id)
);
CREATE INDEX tenancy_workflows_by_account
  ON tenancy_workflows (tenant_id, account, workflow, state);

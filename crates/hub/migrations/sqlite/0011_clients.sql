-- The AI clients a tenant has working (increment 69, src/workflows.rs): the same table and
-- columns as ../postgres/0011_clients.sql, which says what each holds (no row-level security
-- on SQLite, whose store is one hub's own file; the same queries scope the same way).
-- Add-only.
CREATE TABLE tenancy_clients (
  tenant_id TEXT NOT NULL,
  family TEXT NOT NULL,
  first_call_at INTEGER NOT NULL,
  last_call_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, family)
);

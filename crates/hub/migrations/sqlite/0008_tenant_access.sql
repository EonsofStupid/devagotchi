-- Each tenant's access and plan (increment 63, accounts/access.rs): whether an operator has
-- let it in yet (site.json `early_access`) and which of the site's plans it is on.
-- `state` is `admitted` or `waiting`; `waiting_since` is when it joined the early-access
-- list; `admitted_at` and `admitted_by` say when it was let in and by whom (an operator's
-- account, `open sign-up`, or `before early access` for the tenants this migration finds);
-- `plan` is a plan id from site.json `plans` (none, or one the site no longer has: the
-- site's default plan). Add-only. Kept in step with ../postgres/0008_tenant_access.sql
-- (the same table and columns; no row-level security on SQLite, whose store is one hub's
-- own file, and the operator's lists are a plain query).
CREATE TABLE tenancy_access (
  tenant_id TEXT PRIMARY KEY REFERENCES tenancy_tenants (id),
  state TEXT NOT NULL DEFAULT 'admitted' CHECK (state IN ('admitted', 'waiting')),
  waiting_since INTEGER,
  admitted_at INTEGER,
  admitted_by TEXT,
  plan TEXT
);
CREATE INDEX tenancy_access_by_state ON tenancy_access (state);

-- Every tenant there is already is in.
INSERT INTO tenancy_access (tenant_id, state, admitted_at, admitted_by)
  SELECT id, 'admitted', created_at, 'before early access' FROM tenancy_tenants;

-- Each tenant's access and plan (increment 63, accounts/access.rs): whether an operator has
-- let it in yet (site.json `early_access`) and which of the site's plans it is on.
-- `state` is `admitted` or `waiting`; `waiting_since` is when it joined the early-access
-- list; `admitted_at` and `admitted_by` say when it was let in and by whom (an operator's
-- account, `open sign-up`, or `before early access` for the tenants this migration finds);
-- `plan` is a plan id from site.json `plans` (none, or one the site no longer has: the
-- site's default plan). Add-only. Kept in step with ../sqlite/0008_tenant_access.sql.
--
-- A tenant's own row, held like the others since 0007: tenant_id is its key, and
-- row-level security, forced, shows it only to a transaction scoped to that tenant. The
-- operator's lists (who waits, who is in) go through devagotchi.tenants_in, which answers
-- the ids of the tenants in one state and nothing else; each one's details are then read
-- in a transaction scoped to that tenant, as its own requests are.

CREATE TABLE devagotchi.tenancy_access (
  tenant_id TEXT PRIMARY KEY REFERENCES devagotchi.tenancy_tenants (id),
  state TEXT NOT NULL DEFAULT 'admitted' CHECK (state IN ('admitted', 'waiting')),
  waiting_since BIGINT,
  admitted_at BIGINT,
  admitted_by TEXT,
  plan TEXT
);
CREATE INDEX tenancy_access_by_state ON devagotchi.tenancy_access (state);

-- Every tenant there is already is in.
INSERT INTO devagotchi.tenancy_access (tenant_id, state, admitted_at, admitted_by)
  SELECT id, 'admitted', created_at, 'before early access' FROM devagotchi.tenancy_tenants;

ALTER TABLE devagotchi.tenancy_access ENABLE ROW LEVEL SECURITY;
ALTER TABLE devagotchi.tenancy_access FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_only ON devagotchi.tenancy_access
  USING (tenant_id = current_setting('devagotchi.tenant', true))
  WITH CHECK (tenant_id = current_setting('devagotchi.tenant', true));

-- The tenants in one state, at most `most`: the longest waiting first, the latest let in
-- first. Ids only.
CREATE FUNCTION devagotchi.tenants_in(wanted text, most bigint)
RETURNS SETOF text LANGUAGE sql STABLE SECURITY DEFINER SET search_path = devagotchi, pg_temp AS $$
  SELECT tenant_id FROM tenancy_access WHERE state = wanted
  ORDER BY CASE WHEN state = 'waiting' THEN coalesce(waiting_since, 0)
                ELSE -coalesce(admitted_at, 0) END, tenant_id
  LIMIT most
$$;
REVOKE ALL ON FUNCTION devagotchi.tenants_in(text, bigint) FROM PUBLIC;

DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'devagotchi_hub') THEN
    GRANT SELECT, INSERT, UPDATE, DELETE ON devagotchi.tenancy_access TO devagotchi_hub;
    GRANT EXECUTE ON FUNCTION devagotchi.tenants_in(text, bigint) TO devagotchi_hub;
  END IF;
END $$;

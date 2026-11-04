-- The AI clients a tenant has working (increment 69, src/workflows.rs): one row per family of
-- AI client (claude, chatgpt, grok, cursor: analytics::funnel::client_family of the name the
-- client registered with) that made a successful MCP call with one of the tenant's tokens,
-- with the first such call and the latest one noted (at most hourly). A workflow's
-- `ai_client` step is done once its family has a row. Nothing else about the client or the
-- call is kept.
--
-- A tenant's rows like the others since 0007: tenant_id leads the key, and row-level
-- security, forced, shows a row only to a transaction scoped to its tenant. Add-only. Kept in
-- step with ../sqlite/0011_clients.sql.

CREATE TABLE devagotchi.tenancy_clients (
  tenant_id TEXT NOT NULL,
  family TEXT NOT NULL,
  first_call_at BIGINT NOT NULL,
  last_call_at BIGINT NOT NULL,
  PRIMARY KEY (tenant_id, family)
);

ALTER TABLE devagotchi.tenancy_clients ENABLE ROW LEVEL SECURITY;
ALTER TABLE devagotchi.tenancy_clients FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_only ON devagotchi.tenancy_clients
  USING (tenant_id = current_setting('devagotchi.tenant', true))
  WITH CHECK (tenant_id = current_setting('devagotchi.tenant', true));

DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'devagotchi_hub') THEN
    GRANT SELECT, INSERT, UPDATE, DELETE ON devagotchi.tenancy_clients TO devagotchi_hub;
  END IF;
END $$;

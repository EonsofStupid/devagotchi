-- Workflows with direct links (increment 68, src/workflows.rs, accounts/workflows.rs): an
-- AI starts one for its person (`workflow_start`) and hands them one link, `/go/<token>`,
-- where they click through its steps (connect a machine, turn on the phone, keep the
-- account) with no sign-in: the link is the credential, as machine_add's line is. The hub
-- ticks each step as it sees it done.
--
-- tenancy_workflows: one row per workflow started. `account` is the account whose AI
--   started it, the only one its page acts for; `workflow` is the site's workflow id
--   (content/workflows.json) and `definition` the workflow as the site said it then (its
--   steps' kinds and words), so a site changed later never reshapes a workflow under way;
--   `link_hash` is the SHA-256 of the link's token, never the token; `state` is open,
--   completed or cancelled, and only an open one before `expires_at` lets its link do
--   anything; `progress` (JSON) is what each step has done so far: the OS picked, the
--   machine's stored name, when its join code was first issued (never the code), when the
--   hub first saw the step done, and the SHA-256 of each browser that did a step through
--   the link (never the browser's cookie), the ones that may keep the account after.
-- A tenant's rows like the others since 0007: tenant_id leads every key, a workflow points
-- at its account as (tenant_id, account), and row-level security, forced, shows a row only
-- to a transaction scoped to its tenant. The link is the one way in that names no tenant:
-- devagotchi.workflow_tenant answers the tenant of a link's hash and nothing else, as
-- devagotchi.tenant_of does for sessions and tokens. Add-only. Kept in step with
-- ../sqlite/0010_workflows.sql.

CREATE TABLE devagotchi.tenancy_workflows (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL,
  account TEXT NOT NULL,
  workflow TEXT NOT NULL,
  definition TEXT NOT NULL,
  link_hash TEXT NOT NULL,
  machine_name TEXT,
  state TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'completed', 'cancelled')),
  progress TEXT NOT NULL DEFAULT '{}',
  created_at BIGINT NOT NULL,
  expires_at BIGINT NOT NULL,
  ended_at BIGINT,
  PRIMARY KEY (tenant_id, id),
  CONSTRAINT tenancy_workflows_link UNIQUE (link_hash),
  FOREIGN KEY (tenant_id, account) REFERENCES devagotchi.auth_accounts (tenant_id, id)
);
CREATE INDEX tenancy_workflows_by_account
  ON devagotchi.tenancy_workflows (tenant_id, account, workflow, state);

DO $$
DECLARE
  name text;
BEGIN
  FOREACH name IN ARRAY ARRAY['tenancy_workflows'] LOOP
    EXECUTE format('ALTER TABLE devagotchi.%I ENABLE ROW LEVEL SECURITY', name);
    EXECUTE format('ALTER TABLE devagotchi.%I FORCE ROW LEVEL SECURITY', name);
    EXECUTE format(
      'CREATE POLICY tenant_only ON devagotchi.%I
         USING (tenant_id = current_setting(''devagotchi.tenant'', true))
         WITH CHECK (tenant_id = current_setting(''devagotchi.tenant'', true))', name);
  END LOOP;
END $$;

-- The tenant a workflow link (by its token's SHA-256) belongs to, and nothing else.
CREATE FUNCTION devagotchi.workflow_tenant(link text)
RETURNS text LANGUAGE sql STABLE SECURITY DEFINER SET search_path = devagotchi, pg_temp AS $$
  SELECT tenant_id FROM tenancy_workflows WHERE link_hash = link
$$;
REVOKE ALL ON FUNCTION devagotchi.workflow_tenant(text) FROM PUBLIC;

DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'devagotchi_hub') THEN
    GRANT SELECT, INSERT, UPDATE, DELETE ON devagotchi.tenancy_workflows TO devagotchi_hub;
    GRANT EXECUTE ON FUNCTION devagotchi.workflow_tenant(text) TO devagotchi_hub;
  END IF;
END $$;

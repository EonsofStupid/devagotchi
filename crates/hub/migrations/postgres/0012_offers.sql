-- The site's offers (increment 73, src/offers.rs, accounts/offers.rs): a person says yes in
-- the chat to the network the site belongs to and its free offerings, and the hub links
-- their account to their account on that network.
--
-- tenancy_offers: one row per tenant that said yes. `account` is the account whose AI asked;
--   `consent` is the person's own words, exactly as the AI passed them (also on the tenant's
--   chain as `offers.accepted`, with the time); `offerings` (JSON) the names of the
--   offerings the site showed then; `link_state` is pending until the account is linked,
--   then linked, with how (`provisioned`: the hub made or found the account on the network
--   itself; `sign_in`: the person signed in with it) and when, also on the chain as
--   `offers.linked`. `link_hash` is the SHA-256 of the one-tap link's token, never the
--   token, live until `link_expires_at`; a new yes replaces it.
-- A tenant's rows like the others since 0007: tenant_id is the key, the row points at its
-- account as (tenant_id, account), and row-level security, forced, shows a row only to a
-- transaction scoped to its tenant. The link is the one way in that names no tenant:
-- devagotchi.offer_tenant answers the tenant of a link's hash and nothing else, as
-- devagotchi.workflow_tenant does for a workflow's. Add-only. Kept in step with
-- ../sqlite/0012_offers.sql.

CREATE TABLE devagotchi.tenancy_offers (
  tenant_id TEXT NOT NULL PRIMARY KEY,
  account TEXT NOT NULL,
  consent TEXT NOT NULL,
  offerings TEXT NOT NULL DEFAULT '[]',
  accepted_at BIGINT NOT NULL,
  link_state TEXT NOT NULL DEFAULT 'pending' CHECK (link_state IN ('pending', 'linked')),
  linked_via TEXT CHECK (linked_via IN ('provisioned', 'sign_in')),
  linked_at BIGINT,
  link_hash TEXT,
  link_expires_at BIGINT,
  CONSTRAINT tenancy_offers_link UNIQUE (link_hash),
  FOREIGN KEY (tenant_id, account) REFERENCES devagotchi.auth_accounts (tenant_id, id)
);

ALTER TABLE devagotchi.tenancy_offers ENABLE ROW LEVEL SECURITY;
ALTER TABLE devagotchi.tenancy_offers FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_only ON devagotchi.tenancy_offers
  USING (tenant_id = current_setting('devagotchi.tenant', true))
  WITH CHECK (tenant_id = current_setting('devagotchi.tenant', true));

-- The tenant an offers link (by its token's SHA-256) belongs to, and nothing else.
CREATE FUNCTION devagotchi.offer_tenant(link text)
RETURNS text LANGUAGE sql STABLE SECURITY DEFINER SET search_path = devagotchi, pg_temp AS $$
  SELECT tenant_id FROM tenancy_offers WHERE link_hash = link
$$;
REVOKE ALL ON FUNCTION devagotchi.offer_tenant(text) FROM PUBLIC;

DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'devagotchi_hub') THEN
    GRANT SELECT, INSERT, UPDATE, DELETE ON devagotchi.tenancy_offers TO devagotchi_hub;
    GRANT EXECUTE ON FUNCTION devagotchi.offer_tenant(text) TO devagotchi_hub;
  END IF;
END $$;

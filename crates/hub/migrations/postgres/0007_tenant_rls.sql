-- Database-enforced isolation of tenants (increment 60). The one migration that does not
-- only add: an older hub cannot read the store after it, so a rollback across it restores
-- the backup update.sh takes before every deploy (deploy/compose/README.md, "Rolling back
-- across the tenant migration").
--
-- 1. Every table moves into the `devagotchi` schema under a name with its domain's prefix:
--    auth_*, oauth_*, tenancy_*, hub_*. Renamed once, here.
-- 2. `tenant_id` leads the key of every table that holds a tenant's rows (accounts,
--    passkeys, sessions, one-time links, linked sign-ins, OAuth codes and tokens, the
--    tenant's events), and a child row's tenant is its account's: the foreign keys are
--    (tenant_id, account), so no row can point into another tenant.
-- 3. Row-level security, forced, on each of those tables: a row is visible and writable
--    only when the transaction set `devagotchi.tenant` to its tenant (store.rs sets it
--    per transaction, `SET LOCAL` semantics). The hub's queries run as the role
--    `devagotchi_hub` (no superuser, no BYPASSRLS), which the store switches to on every
--    connection when it may.
-- 4. Lookups that must find a tenant before they can set it (a session, a token, a code,
--    a passkey, a link, a linked sign-in, an account by id) go through
--    `devagotchi.tenant_of`, which answers the tenant id and nothing else.
-- Kept in step with ../sqlite/0007_tenant_rls.sql (same tables and columns; no RLS there).

CREATE SCHEMA IF NOT EXISTS devagotchi;

-- The store's tables are where the search path finds them first: the schema the store
-- began in (`public`, or `storage.schema`), which keeps the migration record. One database
-- holds one hub: tables of another already in `devagotchi` stop this migration.
DO $$
DECLARE
  pair text[];
  found regclass;
  taken regclass;
BEGIN
  FOREACH pair SLICE 1 IN ARRAY ARRAY[
    ['accounts', 'auth_accounts'], ['passkeys', 'auth_passkeys'], ['sessions', 'auth_sessions'],
    ['links', 'auth_links'], ['identities', 'auth_identities'], ['email_links', 'auth_email_links'],
    ['email_sends', 'auth_email_sends'], ['account_starts', 'auth_account_starts'],
    ['claim_codes', 'auth_claim_codes'], ['tenants', 'tenancy_tenants'],
    ['tenant_events', 'tenancy_events'], ['secrets', 'hub_secrets'],
    ['spent_challenges', 'hub_spent_challenges'], ['oauth_clients', 'oauth_clients'],
    ['oauth_codes', 'oauth_codes'], ['oauth_tokens', 'oauth_tokens'],
    ['oauth_devices', 'oauth_devices']
  ] LOOP
    found := to_regclass(quote_ident(pair[1]));
    taken := to_regclass(format('devagotchi.%I', pair[2]));
    IF found IS NULL THEN
      RAISE EXCEPTION 'the hub''s table % is not on the search path', pair[1];
    END IF;
    IF taken IS NOT NULL AND taken <> found THEN
      RAISE EXCEPTION 'schema devagotchi already holds another hub''s %; a database holds one hub', pair[2];
    END IF;
    IF (SELECT relnamespace FROM pg_class WHERE oid = found) <> 'devagotchi'::regnamespace THEN
      EXECUTE format('ALTER TABLE %s SET SCHEMA devagotchi', found);
    END IF;
    IF pair[1] <> pair[2] THEN
      EXECUTE format('ALTER TABLE devagotchi.%I RENAME TO %I', pair[1], pair[2]);
    END IF;
  END LOOP;
END $$;

-- Back to the caller's search path at the end (sqlx records the migration in the schema
-- the store began in, inside this transaction).
SELECT set_config('devagotchi.migrating_path', current_setting('search_path'), true);
SET LOCAL search_path TO devagotchi;

-- Accounts made before tenants (a store that skipped 0005's settling) join the first one.
INSERT INTO tenancy_tenants (id, home, created_at)
  SELECT 't-' || gen_random_uuid()::text, 1, extract(epoch FROM now())::bigint
  WHERE NOT EXISTS (SELECT 1 FROM tenancy_tenants WHERE home = 1)
    AND EXISTS (SELECT 1 FROM auth_accounts);
ALTER TABLE auth_accounts RENAME COLUMN tenant TO tenant_id;
UPDATE auth_accounts SET tenant_id = (SELECT id FROM tenancy_tenants WHERE home = 1)
  WHERE tenant_id IS NULL;
ALTER TABLE auth_accounts ALTER COLUMN tenant_id SET NOT NULL;
ALTER TABLE tenancy_events RENAME COLUMN tenant TO tenant_id;

-- The keys: tenant_id first. Dropping the old keys drops the foreign keys that used them;
-- they come back below, on (tenant_id, account).
ALTER TABLE auth_accounts DROP CONSTRAINT accounts_pkey CASCADE;
ALTER TABLE auth_accounts ADD PRIMARY KEY (tenant_id, id);
ALTER TABLE auth_accounts ADD CONSTRAINT auth_accounts_id UNIQUE (id);

ALTER TABLE auth_passkeys ADD COLUMN tenant_id TEXT;
UPDATE auth_passkeys p SET tenant_id = a.tenant_id FROM auth_accounts a WHERE a.id = p.account;
ALTER TABLE auth_passkeys ALTER COLUMN tenant_id SET NOT NULL;
ALTER TABLE auth_passkeys DROP CONSTRAINT passkeys_pkey CASCADE;
ALTER TABLE auth_passkeys ADD PRIMARY KEY (tenant_id, id);
ALTER TABLE auth_passkeys ADD CONSTRAINT auth_passkeys_id UNIQUE (id);
ALTER TABLE auth_passkeys ADD FOREIGN KEY (tenant_id, account) REFERENCES auth_accounts (tenant_id, id);

ALTER TABLE auth_sessions ADD COLUMN tenant_id TEXT;
UPDATE auth_sessions s SET tenant_id = a.tenant_id FROM auth_accounts a WHERE a.id = s.account;
ALTER TABLE auth_sessions ALTER COLUMN tenant_id SET NOT NULL;
ALTER TABLE auth_sessions DROP CONSTRAINT sessions_pkey;
ALTER TABLE auth_sessions ADD PRIMARY KEY (tenant_id, hash);
ALTER TABLE auth_sessions ADD CONSTRAINT auth_sessions_hash UNIQUE (hash);
ALTER TABLE auth_sessions ADD FOREIGN KEY (tenant_id, account) REFERENCES auth_accounts (tenant_id, id);
ALTER TABLE auth_sessions ADD FOREIGN KEY (tenant_id, passkey) REFERENCES auth_passkeys (tenant_id, id) ON DELETE CASCADE;

ALTER TABLE auth_links ADD COLUMN tenant_id TEXT;
UPDATE auth_links l SET tenant_id = a.tenant_id FROM auth_accounts a WHERE a.id = l.account;
ALTER TABLE auth_links ALTER COLUMN tenant_id SET NOT NULL;
ALTER TABLE auth_links DROP CONSTRAINT links_pkey;
ALTER TABLE auth_links ADD PRIMARY KEY (tenant_id, hash);
ALTER TABLE auth_links ADD CONSTRAINT auth_links_hash UNIQUE (hash);
ALTER TABLE auth_links ADD FOREIGN KEY (tenant_id, account) REFERENCES auth_accounts (tenant_id, id);

ALTER TABLE auth_identities ADD COLUMN tenant_id TEXT;
UPDATE auth_identities i SET tenant_id = a.tenant_id FROM auth_accounts a WHERE a.id = i.account;
ALTER TABLE auth_identities ALTER COLUMN tenant_id SET NOT NULL;
ALTER TABLE auth_identities DROP CONSTRAINT identities_pkey;
ALTER TABLE auth_identities ADD PRIMARY KEY (tenant_id, provider, subject);
ALTER TABLE auth_identities ADD CONSTRAINT auth_identities_sign_in UNIQUE (provider, subject);
ALTER TABLE auth_identities ADD FOREIGN KEY (tenant_id, account) REFERENCES auth_accounts (tenant_id, id);
CREATE INDEX auth_identities_by_email ON auth_identities (email);

ALTER TABLE oauth_codes ADD COLUMN tenant_id TEXT;
UPDATE oauth_codes c SET tenant_id = a.tenant_id FROM auth_accounts a WHERE a.id = c.account;
ALTER TABLE oauth_codes ALTER COLUMN tenant_id SET NOT NULL;
ALTER TABLE oauth_codes DROP CONSTRAINT oauth_codes_pkey;
ALTER TABLE oauth_codes ADD PRIMARY KEY (tenant_id, hash);
ALTER TABLE oauth_codes ADD CONSTRAINT oauth_codes_hash UNIQUE (hash);
ALTER TABLE oauth_codes ADD FOREIGN KEY (tenant_id, account) REFERENCES auth_accounts (tenant_id, id);

ALTER TABLE oauth_tokens ADD COLUMN tenant_id TEXT;
UPDATE oauth_tokens t SET tenant_id = a.tenant_id FROM auth_accounts a WHERE a.id = t.account;
ALTER TABLE oauth_tokens ALTER COLUMN tenant_id SET NOT NULL;
ALTER TABLE oauth_tokens DROP CONSTRAINT oauth_tokens_pkey;
ALTER TABLE oauth_tokens ADD PRIMARY KEY (tenant_id, hash);
ALTER TABLE oauth_tokens ADD CONSTRAINT oauth_tokens_hash UNIQUE (hash);
ALTER TABLE oauth_tokens ADD FOREIGN KEY (tenant_id, account) REFERENCES auth_accounts (tenant_id, id);

-- Handshakes that belong to nobody yet (a device code before it is approved, an emailed
-- sign-in link) stay outside any tenant; the account they name is checked by its id.
ALTER TABLE oauth_devices ADD FOREIGN KEY (account) REFERENCES auth_accounts (id);
ALTER TABLE auth_email_links ADD FOREIGN KEY (account) REFERENCES auth_accounts (id);

-- Row-level security, forced: the table's owner is held to it too.
DO $$
DECLARE
  name text;
BEGIN
  FOREACH name IN ARRAY ARRAY['auth_accounts', 'auth_passkeys', 'auth_sessions', 'auth_links',
      'auth_identities', 'oauth_codes', 'oauth_tokens', 'tenancy_events'] LOOP
    EXECUTE format('ALTER TABLE devagotchi.%I ENABLE ROW LEVEL SECURITY', name);
    EXECUTE format('ALTER TABLE devagotchi.%I FORCE ROW LEVEL SECURITY', name);
    EXECUTE format(
      'CREATE POLICY tenant_only ON devagotchi.%I
         USING (tenant_id = current_setting(''devagotchi.tenant'', true))
         WITH CHECK (tenant_id = current_setting(''devagotchi.tenant'', true))', name);
  END LOOP;
END $$;

-- The tenant a lookup key belongs to, and nothing else: how the hub finds the tenant to
-- set before it can read the row. Runs as this migration's role, which RLS does not hold
-- when it is a superuser or has BYPASSRLS (store.rs checks that at start).
CREATE FUNCTION devagotchi.tenant_of(kind text, key text, extra text DEFAULT NULL)
RETURNS text LANGUAGE sql STABLE SECURITY DEFINER SET search_path = devagotchi, pg_temp AS $$
  SELECT CASE kind
    WHEN 'account' THEN (SELECT tenant_id FROM auth_accounts WHERE id = key)
    WHEN 'session' THEN (SELECT tenant_id FROM auth_sessions WHERE hash = key)
    WHEN 'token' THEN (SELECT tenant_id FROM oauth_tokens WHERE hash = key)
    WHEN 'code' THEN (SELECT tenant_id FROM oauth_codes WHERE hash = key)
    WHEN 'link' THEN (SELECT tenant_id FROM auth_links WHERE hash = key)
    WHEN 'passkey' THEN (SELECT tenant_id FROM auth_passkeys WHERE id = decode(key, 'hex'))
    WHEN 'identity' THEN (SELECT tenant_id FROM auth_identities WHERE provider = key AND subject = extra)
    WHEN 'email' THEN (SELECT tenant_id FROM auth_identities WHERE email = key
                       ORDER BY created_at, provider, subject LIMIT 1)
    WHEN 'owner' THEN (SELECT tenant_id FROM auth_accounts WHERE owner = 1)
  END
$$;
-- Accounts nobody secured yet, live, across the hub: the cap on making more.
CREATE FUNCTION devagotchi.live_unsecured(since bigint)
RETURNS bigint LANGUAGE sql STABLE SECURITY DEFINER SET search_path = devagotchi, pg_temp AS $$
  SELECT count(*) FROM auth_accounts
  WHERE provisional = 1 AND disabled_at IS NULL AND last_active > since
$$;
REVOKE ALL ON FUNCTION devagotchi.tenant_of(text, text, text) FROM PUBLIC;
REVOKE ALL ON FUNCTION devagotchi.live_unsecured(bigint) FROM PUBLIC;

-- The role the hub's queries run as: no superuser, no BYPASSRLS. Made when this role may
-- make roles; otherwise an administrator makes it (crates/hub/README.md, "The store").
DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'devagotchi_hub') THEN
    BEGIN
      CREATE ROLE devagotchi_hub NOLOGIN NOSUPERUSER NOBYPASSRLS NOINHERIT;
    EXCEPTION WHEN duplicate_object OR unique_violation OR insufficient_privilege THEN
      NULL;
    END;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'devagotchi_hub') THEN
    BEGIN
      EXECUTE format('GRANT devagotchi_hub TO %I', current_user);
    EXCEPTION WHEN OTHERS THEN
      NULL;
    END;
    GRANT USAGE ON SCHEMA devagotchi TO devagotchi_hub;
    GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA devagotchi TO devagotchi_hub;
    ALTER DEFAULT PRIVILEGES IN SCHEMA devagotchi
      GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO devagotchi_hub;
    GRANT EXECUTE ON FUNCTION devagotchi.tenant_of(text, text, text) TO devagotchi_hub;
    GRANT EXECUTE ON FUNCTION devagotchi.live_unsecured(bigint) TO devagotchi_hub;
  END IF;
END $$;

SELECT set_config('search_path', current_setting('devagotchi.migrating_path'), true);

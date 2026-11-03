-- First-party analytics for a hosted hub's operator (increment 66, src/analytics.rs). The
-- hub counts what it serves itself: no script, no cookie, nothing from anyone else.
--
-- hub_analytics_views: one row per counted visit to a public page. The UTC day (Unix
--   seconds at midnight) and hour, the path, the link's utm_source, utm_medium and
--   utm_campaign (the rest of the query is dropped), the referring site's host (never this
--   hub's own), the device class (mobile, tablet or desktop) and the visitor: an HMAC of
--   the client's address and User-Agent under a key held only in memory and replaced each
--   UTC day, so a visitor cannot be followed from one day to the next. No address and no
--   User-Agent are stored. Kept for analytics.retention_days, then deleted; the day's
--   totals stay in hub_analytics_daily.
-- hub_analytics_events: an account's milestones, each once per tenant (its key): made (with
--   the AI client family), secured (with the sign-in), waiting, admitted, its first
--   machine, its first operation (last_at: its latest) and its first board. The operator's
--   own product data, kept with the tenant. The home tenant (the operator's) never has one.
-- hub_analytics_daily: the daily totals, kept. metric is total, path, referrer, campaign,
--   device (views and visitors), or event, client, sign_in (views is the count).
--
-- Hub-scoped like hub_secrets: no tenant's row, so no tenant policy; the hub's role reads
-- and writes them. Add-only. Kept in step with ../sqlite/0009_analytics.sql.

CREATE TABLE devagotchi.hub_analytics_views (
  day BIGINT NOT NULL,
  hour BIGINT NOT NULL,
  path TEXT NOT NULL,
  utm_source TEXT NOT NULL DEFAULT '',
  utm_medium TEXT NOT NULL DEFAULT '',
  utm_campaign TEXT NOT NULL DEFAULT '',
  referrer TEXT NOT NULL DEFAULT '',
  device TEXT NOT NULL,
  visitor TEXT NOT NULL
);
CREATE INDEX hub_analytics_views_by_day ON devagotchi.hub_analytics_views (day);

CREATE TABLE devagotchi.hub_analytics_events (
  tenant_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  detail TEXT NOT NULL DEFAULT '',
  at BIGINT NOT NULL,
  last_at BIGINT NOT NULL,
  PRIMARY KEY (tenant_id, kind)
);
CREATE INDEX hub_analytics_events_by_at ON devagotchi.hub_analytics_events (at);

CREATE TABLE devagotchi.hub_analytics_daily (
  day BIGINT NOT NULL,
  metric TEXT NOT NULL,
  item TEXT NOT NULL,
  views BIGINT NOT NULL,
  visitors BIGINT NOT NULL,
  PRIMARY KEY (day, metric, item)
);

DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'devagotchi_hub') THEN
    GRANT SELECT, INSERT, UPDATE, DELETE ON devagotchi.hub_analytics_views,
      devagotchi.hub_analytics_events, devagotchi.hub_analytics_daily TO devagotchi_hub;
  END IF;
END $$;

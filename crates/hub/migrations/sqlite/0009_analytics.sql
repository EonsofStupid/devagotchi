-- First-party analytics for a hosted hub's operator (increment 66, src/analytics.rs): the
-- same tables and columns as ../postgres/0009_analytics.sql, which says what each holds.
-- Add-only.
CREATE TABLE hub_analytics_views (
  day INTEGER NOT NULL,
  hour INTEGER NOT NULL,
  path TEXT NOT NULL,
  utm_source TEXT NOT NULL DEFAULT '',
  utm_medium TEXT NOT NULL DEFAULT '',
  utm_campaign TEXT NOT NULL DEFAULT '',
  referrer TEXT NOT NULL DEFAULT '',
  device TEXT NOT NULL,
  visitor TEXT NOT NULL
);
CREATE INDEX hub_analytics_views_by_day ON hub_analytics_views (day);

CREATE TABLE hub_analytics_events (
  tenant_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  detail TEXT NOT NULL DEFAULT '',
  at INTEGER NOT NULL,
  last_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, kind)
);
CREATE INDEX hub_analytics_events_by_at ON hub_analytics_events (at);

CREATE TABLE hub_analytics_daily (
  day INTEGER NOT NULL,
  metric TEXT NOT NULL,
  item TEXT NOT NULL,
  views INTEGER NOT NULL,
  visitors INTEGER NOT NULL,
  PRIMARY KEY (day, metric, item)
);

-- Device authorization (RFC 8628) for the hub's own accounts: `devagotchi login` signing a
-- terminal in (accounts/oauth/device.rs). The device code and the user code are kept only
-- as their SHA-256; a grant is gone once decided and polled, or expired. Kept in step with
-- ../postgres/0004_device.sql.
CREATE TABLE oauth_devices (
  hash TEXT PRIMARY KEY,
  user_code TEXT NOT NULL UNIQUE,
  client TEXT NOT NULL,
  scope TEXT NOT NULL,
  resource TEXT NOT NULL,
  account TEXT REFERENCES accounts (id),
  denied INTEGER NOT NULL DEFAULT 0,
  polled_at INTEGER,
  expires_at INTEGER NOT NULL
);
CREATE INDEX oauth_devices_by_expiry ON oauth_devices (expires_at);

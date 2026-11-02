-- Sign-in links sent by email (accounts/email.rs): only the link's SHA-256, the address it
-- went to, and, for a link that adds the address to an account, that account. Spent once,
-- good for 15 minutes. `email_sends` rate-limits links per client address (an HMAC of it).
-- Add-only. Kept in step with ../sqlite/0006_email_links.sql.
CREATE TABLE email_links (
  hash TEXT PRIMARY KEY,
  email TEXT NOT NULL,
  account TEXT REFERENCES accounts (id),
  created_at BIGINT NOT NULL,
  expires_at BIGINT NOT NULL,
  used_at BIGINT
);
CREATE INDEX email_links_by_email ON email_links (email, created_at);
CREATE TABLE email_sends (
  source TEXT NOT NULL,
  at BIGINT NOT NULL
);
CREATE INDEX email_sends_by_source ON email_sends (source, at);

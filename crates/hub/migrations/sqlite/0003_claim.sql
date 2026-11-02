-- The claim code a hub with no owner prints when it starts (accounts/claim.rs): only its
-- SHA-256, and when it stops working. One row at most; it goes when the owner's first
-- passkey is made. Kept in step with ../postgres/0003_claim.sql.
CREATE TABLE claim_codes (
  hash TEXT PRIMARY KEY,
  expires_at INTEGER NOT NULL
);

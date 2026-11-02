import assert from "node:assert/strict";

const { linkProfile } = await import("../lib/hub-account.ts");

const profile = { email: "Ada@Example.com", devagotchiAccountId: null };

assert.deepEqual(
  linkProfile(profile, { accountId: "dg_ab", email: "ada@example.com", emailVerified: true }),
  { kind: "link", accountId: "dg_ab" },
);
assert.equal(
  linkProfile(profile, { accountId: "dg_ab", email: "ada@example.com", emailVerified: false }).kind,
  "refuse",
);
assert.equal(
  linkProfile(profile, { accountId: "dg_ab", email: "other@example.com", emailVerified: true }).kind,
  "refuse",
);
assert.deepEqual(
  linkProfile(
    { ...profile, devagotchiAccountId: "dg_old" },
    { accountId: "dg_new", email: "ada@example.com", emailVerified: true },
  ),
  { kind: "already", accountId: "dg_old" },
);

console.log("ok hub account link");

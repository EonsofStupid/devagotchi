/**
 * How a devagotchi.com profile becomes a Devagotchi account.
 *
 * The site keeps its own `profiles` row (today keyed by the ChatGPT user id, with a
 * unique email). The hub is the account system. This function is the whole bridge:
 * a verified email match links, an already stored hub account id is kept, and an
 * unverified address never selects an account.
 */
export type SiteProfile = {
  email: string;
  devagotchiAccountId?: string | null;
};

export type HubIdentity = {
  /** Devagotchi account id from the hub's access token (`sub`). */
  accountId: string;
  email?: string | null;
  emailVerified: boolean;
};

export type LinkDecision =
  | { kind: "already"; accountId: string }
  | { kind: "link"; accountId: string }
  | { kind: "refuse"; reason: string };

export function linkProfile(profile: SiteProfile, hub: HubIdentity): LinkDecision {
  if (profile.devagotchiAccountId) {
    return { kind: "already", accountId: profile.devagotchiAccountId };
  }
  const profileEmail = profile.email.trim().toLowerCase();
  const hubEmail = hub.email?.trim().toLowerCase() ?? "";
  if (!hub.emailVerified || !hubEmail || hubEmail !== profileEmail) {
    return {
      kind: "refuse",
      reason: "a profile links only when the hub token carries the same verified email",
    };
  }
  return { kind: "link", accountId: hub.accountId };
}

import { env } from "cloudflare:workers";
import type { ChatGPTUser } from "@/app/chatgpt-auth";

type PlatformEnv = { DB: D1Database; PET_ASSETS: R2Bucket };

export type PetRecord = {
  id: string;
  name: string;
  species: string;
  assetUrl: string | null;
  isPrimary: boolean;
  level: number;
  xp: number;
  bond: number;
  energy: number;
  focus: number;
  streak: number;
  createdAt: number;
};

type PetRow = Omit<PetRecord, "assetUrl" | "isPrimary" | "createdAt"> & {
  asset_key: string | null;
  is_primary: number;
  created_at: number;
};

function platform() {
  return env as unknown as PlatformEnv;
}

let schemaReady = false;

export async function ensureSchema() {
  if (schemaReady) return;
  const db = platform().DB;
  await db.batch([
    db.prepare(`CREATE TABLE IF NOT EXISTS profiles (
      id TEXT PRIMARY KEY,
      email TEXT NOT NULL UNIQUE,
      display_name TEXT NOT NULL,
      devagotchi_account_id TEXT,
      remember_until INTEGER,
      notifications_enabled INTEGER NOT NULL DEFAULT 0,
      created_at INTEGER NOT NULL,
      last_seen_at INTEGER NOT NULL
    )`),
    db.prepare(`CREATE TABLE IF NOT EXISTS pets (
      id TEXT PRIMARY KEY,
      owner_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
      name TEXT NOT NULL,
      species TEXT NOT NULL,
      asset_key TEXT,
      asset_type TEXT,
      is_primary INTEGER NOT NULL DEFAULT 0,
      level INTEGER NOT NULL DEFAULT 1,
      xp INTEGER NOT NULL DEFAULT 0,
      bond INTEGER NOT NULL DEFAULT 20,
      energy INTEGER NOT NULL DEFAULT 80,
      focus INTEGER NOT NULL DEFAULT 50,
      streak INTEGER NOT NULL DEFAULT 1,
      last_care_at INTEGER,
      created_at INTEGER NOT NULL
    )`),
    db.prepare(`CREATE TABLE IF NOT EXISTS pet_actions (
      id TEXT PRIMARY KEY,
      pet_id TEXT NOT NULL REFERENCES pets(id) ON DELETE CASCADE,
      owner_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
      kind TEXT NOT NULL,
      xp_delta INTEGER NOT NULL,
      created_at INTEGER NOT NULL
    )`),
    db.prepare(`CREATE TABLE IF NOT EXISTS push_subscriptions (
      id TEXT PRIMARY KEY,
      owner_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
      endpoint TEXT NOT NULL UNIQUE,
      subscription_json TEXT NOT NULL,
      created_at INTEGER NOT NULL
    )`),
    db.prepare("CREATE INDEX IF NOT EXISTS pets_owner_idx ON pets(owner_id)"),
    db.prepare("CREATE UNIQUE INDEX IF NOT EXISTS pets_one_primary_idx ON pets(owner_id) WHERE is_primary = 1"),
    db.prepare("CREATE INDEX IF NOT EXISTS pet_actions_owner_created_idx ON pet_actions(owner_id, created_at)"),
    db.prepare("CREATE INDEX IF NOT EXISTS push_owner_idx ON push_subscriptions(owner_id)"),
  ]);
  try {
    await db.prepare("ALTER TABLE profiles ADD COLUMN devagotchi_account_id TEXT").run();
  } catch {
    // The column is already there. SQLite has no ADD COLUMN IF NOT EXISTS.
  }
  schemaReady = true;
}

export async function ensureProfile(user: ChatGPTUser) {
  await ensureSchema();
  const db = platform().DB;
  const now = Date.now();
  await db.batch([
    db.prepare("INSERT OR IGNORE INTO profiles (id, email, display_name, created_at, last_seen_at) VALUES (?, ?, ?, ?, ?)")
      .bind(user.userId, user.email, user.displayName, now, now),
    db.prepare("UPDATE profiles SET email = ?, display_name = ?, last_seen_at = ? WHERE id = ?")
      .bind(user.email, user.displayName, now, user.userId),
  ]);

  const count = await db.prepare("SELECT COUNT(*) AS count FROM pets WHERE owner_id = ?").bind(user.userId).first<{ count: number }>();
  if (!count?.count) {
    await db.prepare(`INSERT INTO pets
      (id, owner_id, name, species, is_primary, level, xp, bond, energy, focus, streak, created_at)
      VALUES (?, ?, 'Clyffy.AI', 'Builder cow', 1, 12, 1120, 64, 77, 82, 7, ?)`)
      .bind(crypto.randomUUID(), user.userId, now).run();
  }
}

function present(row: PetRow): PetRecord {
  return {
    id: row.id,
    name: row.name,
    species: row.species,
    assetUrl: row.asset_key ? `/api/pets/${row.id}/asset` : null,
    isPrimary: Boolean(row.is_primary),
    level: row.level,
    xp: row.xp,
    bond: row.bond,
    energy: row.energy,
    focus: row.focus,
    streak: row.streak,
    createdAt: row.created_at,
  };
}

export async function listPets(user: ChatGPTUser) {
  await ensureProfile(user);
  const result = await platform().DB.prepare(`SELECT id, name, species, asset_key, is_primary, level, xp, bond, energy, focus, streak, created_at
    FROM pets WHERE owner_id = ? ORDER BY is_primary DESC, created_at DESC`).bind(user.userId).all<PetRow>();
  return result.results.map(present);
}

export async function createPet(user: ChatGPTUser, file: File, name: string, species: string, makePrimary: boolean) {
  await ensureProfile(user);
  const runtime = platform();
  const id = crypto.randomUUID();
  const safeName = file.name.replace(/[^a-zA-Z0-9._-]/g, "-").slice(-90) || "pet-art";
  const key = `${user.userId}/${id}/${safeName}`;
  await runtime.PET_ASSETS.put(key, file.stream(), {
    httpMetadata: { contentType: file.type || "application/octet-stream" },
    customMetadata: { ownerId: user.userId, petId: id },
  });
  const existingPrimary = await runtime.DB.prepare("SELECT id FROM pets WHERE owner_id = ? AND is_primary = 1").bind(user.userId).first();
  const primary = makePrimary || !existingPrimary;
  const now = Date.now();
  const statements = [];
  if (primary) statements.push(runtime.DB.prepare("UPDATE pets SET is_primary = 0 WHERE owner_id = ?").bind(user.userId));
  statements.push(runtime.DB.prepare(`INSERT INTO pets
    (id, owner_id, name, species, asset_key, asset_type, is_primary, level, xp, bond, energy, focus, streak, created_at)
    VALUES (?, ?, ?, ?, ?, ?, ?, 1, 0, 20, 80, 50, 1, ?)`)
    .bind(id, user.userId, name, species, key, file.type, primary ? 1 : 0, now));
  await runtime.DB.batch(statements);
  return (await getPet(user, id))!;
}

export async function getPet(user: ChatGPTUser, id: string) {
  await ensureProfile(user);
  const row = await platform().DB.prepare(`SELECT id, name, species, asset_key, is_primary, level, xp, bond, energy, focus, streak, created_at
    FROM pets WHERE id = ? AND owner_id = ?`).bind(id, user.userId).first<PetRow>();
  return row ? present(row) : null;
}

export async function setPrimaryPet(user: ChatGPTUser, id: string) {
  await ensureProfile(user);
  const db = platform().DB;
  const owned = await db.prepare("SELECT id FROM pets WHERE id = ? AND owner_id = ?").bind(id, user.userId).first();
  if (!owned) return null;
  await db.batch([
    db.prepare("UPDATE pets SET is_primary = 0 WHERE owner_id = ?").bind(user.userId),
    db.prepare("UPDATE pets SET is_primary = 1 WHERE id = ? AND owner_id = ?").bind(id, user.userId),
  ]);
  return getPet(user, id);
}

const CARE_RULES = {
  focus: { xp: 18, bond: 3, energy: -8, focus: 12 },
  treat: { xp: 6, bond: 5, energy: 18, focus: 0 },
  play: { xp: 10, bond: 10, energy: -4, focus: 4 },
} as const;

export async function careForPet(user: ChatGPTUser, id: string, kind: keyof typeof CARE_RULES) {
  await ensureProfile(user);
  const db = platform().DB;
  const current = await db.prepare("SELECT xp, streak, last_care_at FROM pets WHERE id = ? AND owner_id = ?").bind(id, user.userId).first<{ xp: number; streak: number; last_care_at: number | null }>();
  if (!current) return null;
  const rule = CARE_RULES[kind];
  const now = Date.now();
  const today = new Date(now); today.setHours(0, 0, 0, 0);
  const last = current.last_care_at ? new Date(current.last_care_at) : null;
  if (last) last.setHours(0, 0, 0, 0);
  const dayDelta = last ? Math.round((today.getTime() - last.getTime()) / 86400000) : 1;
  const streak = dayDelta === 0 ? current.streak : dayDelta === 1 ? current.streak + 1 : 1;
  const xp = current.xp + rule.xp;
  const level = Math.max(1, Math.floor(xp / 100) + 1);
  await db.batch([
    db.prepare(`UPDATE pets SET xp = ?, level = ?, bond = MIN(100, bond + ?), energy = MAX(0, MIN(100, energy + ?)),
      focus = MAX(0, MIN(100, focus + ?)), streak = ?, last_care_at = ? WHERE id = ? AND owner_id = ?`)
      .bind(xp, level, rule.bond, rule.energy, rule.focus, streak, now, id, user.userId),
    db.prepare("INSERT INTO pet_actions (id, pet_id, owner_id, kind, xp_delta, created_at) VALUES (?, ?, ?, ?, ?, ?)")
      .bind(crypto.randomUUID(), id, user.userId, kind, rule.xp, now),
  ]);
  return getPet(user, id);
}

export async function getOwnedAsset(user: ChatGPTUser, id: string) {
  await ensureProfile(user);
  const row = await platform().DB.prepare("SELECT asset_key FROM pets WHERE id = ? AND owner_id = ?").bind(id, user.userId).first<{ asset_key: string | null }>();
  if (!row?.asset_key) return null;
  return platform().PET_ASSETS.get(row.asset_key);
}

export async function savePreferences(user: ChatGPTUser, notificationsEnabled: boolean, rememberDevice: boolean, subscription?: PushSubscriptionJSON) {
  await ensureProfile(user);
  const db = platform().DB;
  const rememberUntil = rememberDevice ? Date.now() + 30 * 24 * 60 * 60 * 1000 : null;
  await db.prepare("UPDATE profiles SET notifications_enabled = ?, remember_until = ? WHERE id = ?")
    .bind(notificationsEnabled ? 1 : 0, rememberUntil, user.userId).run();
  if (subscription?.endpoint) {
    await db.prepare(`INSERT INTO push_subscriptions (id, owner_id, endpoint, subscription_json, created_at)
      VALUES (?, ?, ?, ?, ?) ON CONFLICT(endpoint) DO UPDATE SET owner_id = excluded.owner_id, subscription_json = excluded.subscription_json`)
      .bind(crypto.randomUUID(), user.userId, subscription.endpoint, JSON.stringify(subscription), Date.now()).run();
  }
  return { rememberUntil, notificationsEnabled };
}

import { index, integer, sqliteTable, text, uniqueIndex } from "drizzle-orm/sqlite-core";
import { sql } from "drizzle-orm";

export const profiles = sqliteTable("profiles", {
  id: text("id").primaryKey(),
  email: text("email").notNull(),
  displayName: text("display_name").notNull(),
  rememberUntil: integer("remember_until"),
  notificationsEnabled: integer("notifications_enabled", { mode: "boolean" }).notNull().default(false),
  createdAt: integer("created_at").notNull(),
  lastSeenAt: integer("last_seen_at").notNull(),
}, (table) => [uniqueIndex("profiles_email_idx").on(table.email)]);

export const pets = sqliteTable("pets", {
  id: text("id").primaryKey(),
  ownerId: text("owner_id").notNull().references(() => profiles.id, { onDelete: "cascade" }),
  name: text("name").notNull(),
  species: text("species").notNull(),
  assetKey: text("asset_key"),
  assetType: text("asset_type"),
  isPrimary: integer("is_primary", { mode: "boolean" }).notNull().default(false),
  level: integer("level").notNull().default(1),
  xp: integer("xp").notNull().default(0),
  bond: integer("bond").notNull().default(20),
  energy: integer("energy").notNull().default(80),
  focus: integer("focus").notNull().default(50),
  streak: integer("streak").notNull().default(1),
  lastCareAt: integer("last_care_at"),
  createdAt: integer("created_at").notNull(),
}, (table) => [
  index("pets_owner_idx").on(table.ownerId),
  uniqueIndex("pets_one_primary_idx").on(table.ownerId).where(sql`${table.isPrimary} = 1`),
]);

export const petActions = sqliteTable("pet_actions", {
  id: text("id").primaryKey(),
  petId: text("pet_id").notNull().references(() => pets.id, { onDelete: "cascade" }),
  ownerId: text("owner_id").notNull().references(() => profiles.id, { onDelete: "cascade" }),
  kind: text("kind").notNull(),
  xpDelta: integer("xp_delta").notNull(),
  createdAt: integer("created_at").notNull(),
}, (table) => [index("pet_actions_owner_created_idx").on(table.ownerId, table.createdAt)]);

export const pushSubscriptions = sqliteTable("push_subscriptions", {
  id: text("id").primaryKey(),
  ownerId: text("owner_id").notNull().references(() => profiles.id, { onDelete: "cascade" }),
  endpoint: text("endpoint").notNull(),
  subscriptionJson: text("subscription_json").notNull(),
  createdAt: integer("created_at").notNull(),
}, (table) => [
  uniqueIndex("push_endpoint_idx").on(table.endpoint),
  index("push_owner_idx").on(table.ownerId),
]);

import { getChatGPTUser } from "@/app/chatgpt-auth";
import { createPet, listPets } from "@/db/repository";

export const dynamic = "force-dynamic";

export async function GET() {
  const user = await getChatGPTUser();
  if (!user) return Response.json({ error: "Sign in required" }, { status: 401 });
  return Response.json(await listPets(user));
}

export async function POST(request: Request) {
  const user = await getChatGPTUser();
  if (!user) return Response.json({ error: "Sign in required" }, { status: 401 });
  const data = await request.formData();
  const file = data.get("file");
  const name = String(data.get("name") ?? "").trim();
  const species = String(data.get("species") ?? "").trim();
  const allowed = new Set(["image/png", "image/webp", "image/gif"]);
  if (!(file instanceof File) || !allowed.has(file.type)) return Response.json({ error: "Choose a PNG, WebP, or GIF pet image." }, { status: 400 });
  if (file.size > 8 * 1024 * 1024) return Response.json({ error: "Pet artwork must be 8 MB or smaller." }, { status: 400 });
  if (!name || !species || name.length > 40 || species.length > 40) return Response.json({ error: "Add a short name and species." }, { status: 400 });
  return Response.json(await createPet(user, file, name, species, data.get("primary") === "true"), { status: 201 });
}

import { getChatGPTUser } from "@/app/chatgpt-auth";
import { careForPet } from "@/db/repository";

const kinds = new Set(["focus", "treat", "play"]);

export async function POST(request: Request, context: { params: Promise<{ id: string }> }) {
  const user = await getChatGPTUser();
  if (!user) return Response.json({ error: "Sign in required" }, { status: 401 });
  const body = await request.json().catch(() => ({})) as { kind?: string };
  if (!body.kind || !kinds.has(body.kind)) return Response.json({ error: "Unknown care action" }, { status: 400 });
  const { id } = await context.params;
  const pet = await careForPet(user, id, body.kind as "focus" | "treat" | "play");
  return pet ? Response.json(pet) : Response.json({ error: "Pet not found" }, { status: 404 });
}

import { getChatGPTUser } from "@/app/chatgpt-auth";
import { setPrimaryPet } from "@/db/repository";

export async function POST(_request: Request, context: { params: Promise<{ id: string }> }) {
  const user = await getChatGPTUser();
  if (!user) return Response.json({ error: "Sign in required" }, { status: 401 });
  const { id } = await context.params;
  const pet = await setPrimaryPet(user, id);
  return pet ? Response.json(pet) : Response.json({ error: "Pet not found" }, { status: 404 });
}

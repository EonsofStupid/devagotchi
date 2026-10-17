import { getChatGPTUser } from "@/app/chatgpt-auth";
import { getOwnedAsset } from "@/db/repository";

export async function GET(_request: Request, context: { params: Promise<{ id: string }> }) {
  const user = await getChatGPTUser();
  if (!user) return new Response("Sign in required", { status: 401 });
  const { id } = await context.params;
  const object = await getOwnedAsset(user, id);
  if (!object) return new Response("Not found", { status: 404 });
  const headers = new Headers();
  object.writeHttpMetadata(headers);
  headers.set("etag", object.httpEtag);
  headers.set("cache-control", "private, max-age=3600");
  return new Response(object.body, { headers });
}

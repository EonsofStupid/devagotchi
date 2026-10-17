import { getChatGPTUser } from "@/app/chatgpt-auth";
import { savePreferences } from "@/db/repository";

export async function POST(request: Request) {
  const user = await getChatGPTUser();
  if (!user) return Response.json({ error: "Sign in required" }, { status: 401 });
  const body = await request.json().catch(() => ({})) as { notificationsEnabled?: boolean; rememberDevice?: boolean; subscription?: PushSubscriptionJSON };
  const result = await savePreferences(user, Boolean(body.notificationsEnabled), Boolean(body.rememberDevice), body.subscription);
  const headers = new Headers({ "content-type": "application/json" });
  if (body.rememberDevice) headers.append("set-cookie", "devagotchi_device=remembered; Max-Age=2592000; Path=/; Secure; HttpOnly; SameSite=Lax");
  else headers.append("set-cookie", "devagotchi_device=; Max-Age=0; Path=/; Secure; HttpOnly; SameSite=Lax");
  return new Response(JSON.stringify(result), { headers });
}

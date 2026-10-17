import { chatGPTSignOutPath, requireChatGPTUser } from "@/app/chatgpt-auth";
import { SanctuaryClient } from "./sanctuary-client";

export const dynamic = "force-dynamic";

export default async function SanctuaryPage() {
  const user = await requireChatGPTUser("/sanctuary");
  return <SanctuaryClient displayName={user.displayName} email={user.email} signOutHref={chatGPTSignOutPath()} />;
}

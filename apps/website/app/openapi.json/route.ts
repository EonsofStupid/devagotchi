import { openApi, referenceJson } from "../../lib/devagotchi-reference";

export function GET() {
  return referenceJson(openApi());
}

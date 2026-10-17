import { capabilities, referenceJson } from "../../../../lib/devagotchi-reference";

export function GET() {
  return referenceJson(capabilities());
}

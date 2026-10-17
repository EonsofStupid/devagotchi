import { documentation, documentationTopics, referenceJson, REFERENCE_VERSION, type DocumentationTopic } from "../../../../lib/devagotchi-reference";

export function GET(request: Request) {
  const query = new URL(request.url).searchParams;
  if ([...query.keys()].some(key => key !== "topic") || query.getAll("topic").length > 1) {
    return referenceJson({ error: "Use one optional topic parameter.", topics: documentationTopics }, 400);
  }
  const topic = query.get("topic");
  if (!topic) return referenceJson({ reference_version: REFERENCE_VERSION, topics: documentationTopics });
  if (!documentationTopics.includes(topic as DocumentationTopic)) return referenceJson({ error: "Unknown reference topic.", topics: documentationTopics }, 400);
  return referenceJson({ reference_version: REFERENCE_VERSION, topic, reference: documentation[topic as DocumentationTopic] });
}

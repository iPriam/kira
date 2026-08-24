import { source } from "@/lib/source";
import { llms } from "fumadocs-core/source";

export async function GET() {
  return new Response(llms(source).index());
}

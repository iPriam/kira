import { getLLMText, source } from "@/lib/source";

export async function GET(
  _request: Request,
  { params }: { params: Promise<{ slug?: string[] }> },
) {
  const { slug = [] } = await params;
  const slugs = [...slug];

  // Remove the last element (content.md)
  slugs.pop();

  if (slugs.length === 0) {
    return new Response("not found", { status: 404 });
  }

  const page = source.getPage(slugs);
  if (!page) {
    return new Response("not found", { status: 404 });
  }

  return new Response(await getLLMText(page), {
    headers: {
      "Content-Type": "text/markdown",
    },
  });
}

import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { z } from "zod";

import { type DocsContext, resolvePage } from "./docs-source";
import { pagePayload } from "./resources";
import { searchDocs } from "./search";

export function registerDocsTools(server: McpServer, context: DocsContext) {
  server.registerTool(
    "get_doc_page",
    {
      title: "Get Kira Documentation Page",
      description:
        "Fetch one Kira docs page by slug from the canonical Fumadocs source.",
      inputSchema: {
        slug: z
          .union([z.string(), z.array(z.string())])
          .describe(
            "Page slug such as 'language-guide/functions'. Use '' or 'index' for the root page.",
          ),
      },
    },
    async ({ slug }) => {
      const page = await resolvePage(context.source, slug);

      if (!page) {
        return {
          isError: true,
          content: [
            {
              type: "text",
              text: `No Kira docs page exists for slug ${JSON.stringify(slug)}.`,
            },
          ],
        };
      }

      return jsonToolResult(await pagePayload(page));
    },
  );

  server.registerTool(
    "search_docs",
    {
      title: "Search Kira Documentation",
      description:
        "Search Kira docs by title, slug, URL, and processed Markdown text.",
      inputSchema: {
        query: z.string().min(1).describe("Free-text search query."),
        limit: z
          .number()
          .int()
          .min(1)
          .max(20)
          .default(5)
          .describe("Maximum number of ranked matches to return."),
      },
    },
    async ({ query, limit }) =>
      jsonToolResult({
        query,
        results: await searchDocs(context, query, limit),
      }),
  );
}

function jsonToolResult(value: unknown) {
  return {
    content: [
      {
        type: "text" as const,
        text: JSON.stringify(value, null, 2),
      },
    ],
  };
}

import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";

import {
  type DocsContext,
  type DocsPage,
  getLLMText,
  getPageResourceUri,
  pageMetadata,
} from "./docs-source";

export function registerDocsResources(server: McpServer, context: DocsContext) {
  server.registerResource(
    "docs-tree",
    "docs://tree",
    {
      title: "Kira Docs Tree",
      description: "Fumadocs documentation tree and navigation metadata.",
      mimeType: "application/json",
    },
    async (uri) =>
      jsonResource(uri.href, {
        source: "fumadocs",
        sourceMode: context.sourceMode,
        tree: context.source.getPageTree(),
      }),
  );

  server.registerResource(
    "docs-pages",
    "docs://pages",
    {
      title: "Kira Docs Pages",
      description: "All Kira documentation pages with canonical page metadata.",
      mimeType: "application/json",
    },
    async (uri) =>
      jsonResource(uri.href, {
        source: "fumadocs",
        sourceMode: context.sourceMode,
        count: context.source.getPages().length,
        pages: context.source
          .getPages()
          .map((page) => pageMetadata(page as DocsPage)),
      }),
  );

  for (const page of context.source.getPages()) {
    const typedPage = page as DocsPage;
    const metadata = pageMetadata(typedPage);

    server.registerResource(
      `doc-page-${metadata.slug || "index"}`,
      getPageResourceUri(typedPage),
      {
        title: metadata.title,
        description:
          metadata.description ??
          `Processed LLM Markdown for ${metadata.slug || "index"}.`,
        mimeType: "application/json",
      },
      async (uri) => jsonResource(uri.href, await pagePayload(typedPage)),
    );
  }
}

export async function pagePayload(page: DocsPage) {
  return {
    ...pageMetadata(page),
    llmText: await getLLMText(page),
  };
}

function jsonResource(uri: string, value: unknown) {
  return {
    contents: [
      {
        uri,
        mimeType: "application/json",
        text: toJson(value),
      },
    ],
  };
}

function toJson(value: unknown) {
  const seen = new WeakSet<object>();

  return JSON.stringify(
    value,
    (_key, nested) => {
      if (typeof nested === "function") return undefined;
      if (typeof nested !== "object" || nested === null) return nested;
      if (seen.has(nested)) return "[Circular]";

      seen.add(nested);
      return nested;
    },
    2,
  );
}

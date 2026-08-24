import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { CallToolResultSchema } from "@modelcontextprotocol/sdk/types.js";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("../", import.meta.url));
const serverPath = fileURLToPath(new URL("./server.ts", import.meta.url));

const client = new Client({
  name: "kira-docs-smoke-test",
  version: "0.1.0",
});

const transport = new StdioClientTransport({
  command: process.execPath,
  args: ["run", serverPath],
  cwd: repoRoot,
  stderr: "pipe",
});

try {
  await client.connect(transport);

  const resources = await client.listResources();
  assert(
    resources.resources.some((resource) => resource.uri === "docs://tree"),
    "docs://tree resource is missing",
  );
  assert(
    resources.resources.some((resource) => resource.uri === "docs://pages"),
    "docs://pages resource is missing",
  );
  assert(
    resources.resources.some(
      (resource) => resource.uri === "docs://page/language-guide/functions",
    ),
    "function page resource is missing",
  );

  const tree = await client.readResource({ uri: "docs://tree" });
  assert(
    "text" in tree.contents[0] &&
      tree.contents[0].text.includes("Language Guide"),
    "tree is empty",
  );

  const pageResource = await client.readResource({
    uri: "docs://page/language-guide/functions",
  });
  assert(
    "text" in pageResource.contents[0] &&
      pageResource.contents[0].text.includes('"title": "Functions"'),
    "page resource did not return the functions page",
  );

  const page = await client.callTool(
    {
      name: "get_doc_page",
      arguments: {
        slug: "language-guide/functions",
      },
    },
    CallToolResultSchema,
  );
  assert(
    firstTextContent(page).includes("Functions"),
    "get_doc_page did not return the functions page",
  );

  const search = await client.callTool(
    {
      name: "search_docs",
      arguments: {
        query: "@Main",
        limit: 3,
      },
    },
    CallToolResultSchema,
  );
  assert(firstTextContent(search).includes("results"), "search_docs failed");

  console.log("MCP smoke test passed");
} finally {
  await client.close();
}

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

function firstTextContent(result: unknown) {
  const content = (result as { content?: unknown }).content;
  assert(Array.isArray(content), "tool result content is missing");

  const first = content[0] as { type?: unknown; text?: unknown } | undefined;
  assert(first?.type === "text", "first tool content is not text");
  assert(typeof first.text === "string", "first tool text is missing");

  return first.text;
}

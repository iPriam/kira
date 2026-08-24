import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { fileURLToPath, pathToFileURL } from "node:url";

import { loadDocsContext } from "./docs-source";
import { registerDocsResources } from "./resources";
import { registerDocsTools } from "./tools";

process.chdir(fileURLToPath(new URL("../", import.meta.url)));

export async function createKiraDocsMcpServer() {
  const context = await loadDocsContext();
  const server = new McpServer({
    name: "kira-docs",
    version: "0.1.0",
  });

  registerDocsResources(server, context);
  registerDocsTools(server, context);

  return server;
}

async function main() {
  const server = await createKiraDocsMcpServer();
  await server.connect(new StdioServerTransport());
}

if (process.argv[1] && pathToFileURL(process.argv[1]).href === import.meta.url) {
  main().catch((error) => {
    console.error(error);
    process.exit(1);
  });
}

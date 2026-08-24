import { loader, type InferPageType } from "fumadocs-core/source";
import { dynamic } from "fumadocs-mdx/runtime/dynamic";
import { readFile } from "node:fs/promises";
import { glob } from "tinyglobby";

import {
  getLLMText as getAppLLMText,
  getPageMarkdownUrl as getAppPageMarkdownUrl,
  source as appSource,
} from "../app/lib/source";
import { docsContentRoute, docsRoute } from "../app/lib/shared";
import * as sourceConfig from "../source.config";

type AppPage = InferPageType<typeof appSource>;
type DocsSource = typeof appSource;

export type DocsPage = AppPage;

export interface DocsContext {
  source: DocsSource;
  sourceMode: "app-source" | "fumadocs-runtime";
}

let contextPromise: Promise<DocsContext> | undefined;

export function loadDocsContext() {
  contextPromise ??= loadDocsContextUncached();
  return contextPromise;
}

export function getPageMarkdownUrl(page: DocsPage) {
  return getAppPageMarkdownUrl(page);
}

export async function getLLMText(page: DocsPage) {
  return getAppLLMText(page);
}

export function getPageSlug(page: DocsPage) {
  return page.slugs.join("/");
}

export function getPageResourceUri(page: DocsPage) {
  return `docs://page/${getPageSlug(page) || "index"}`;
}

export function normalizeSlugInput(slug: string | string[] | undefined) {
  if (Array.isArray(slug)) {
    return slug.map((segment) => segment.trim()).filter(Boolean);
  }

  const clean = (slug ?? "")
    .trim()
    .replace(/^docs:\/{0,2}page\/?/i, "")
    .replace(/^\/?docs\/?/i, "")
    .replace(/^\/+|\/+$/g, "");

  if (!clean || clean === "index") return [];

  return clean.split("/").filter(Boolean).map(decodeURIComponent);
}

export async function resolvePage(
  source: DocsSource,
  slug: string | string[] | undefined,
) {
  const slugs = normalizeSlugInput(slug);
  let page = source.getPage(slugs);

  if (!page && slugs.at(-1) === "index") {
    page = source.getPage(slugs.slice(0, -1));
  }

  return page as DocsPage | undefined;
}

async function loadDocsContextUncached(): Promise<DocsContext> {
  if (await sourceHasProcessedMarkdown(appSource)) {
    return {
      source: appSource,
      sourceMode: "app-source",
    };
  }

  return {
    source: await createRuntimeSource(),
    sourceMode: "fumadocs-runtime",
  };
}

async function sourceHasProcessedMarkdown(source: DocsSource) {
  const first = source.getPages()[0];
  if (!first) return true;

  try {
    await getAppLLMText(first);
    return true;
  } catch {
    return false;
  }
}

async function createRuntimeSource() {
  const create = await dynamic(
    sourceConfig,
    {
      configPath: "./source.config.ts",
      environment: "node",
      outDir: ".source",
    },
    {
      doc: {
        passthroughs: ["extractedReferences"],
      },
    },
  );

  const [docFiles, metaFiles] = await Promise.all([
    glob("**/*.mdx", { cwd: "content/docs" }),
    glob("**/meta.json", { cwd: "content/docs" }),
  ]);

  const docs = await create.docs(
    "docs",
    "content/docs",
    Object.fromEntries(
      await Promise.all(
        metaFiles.map(async (file) => [
          file,
          JSON.parse(await readFile(`content/docs/${file}`, "utf8")),
        ]),
      ),
    ),
    await Promise.all(
      docFiles.map(async (file) => ({
        info: {
          path: file,
          fullPath: `content/docs/${file}`,
        },
        data: parseFrontmatter(await readFile(`content/docs/${file}`, "utf8")),
      })),
    ),
  );

  return loader({
    source: docs.toFumadocsSource(),
    baseUrl: docsRoute,
  }) as unknown as DocsSource;
}

function parseFrontmatter(source: string) {
  const match = source.match(/^---\r?\n([\s\S]*?)\r?\n---/);
  if (!match) return {};

  const data: Record<string, string> = {};

  for (const line of match[1].split(/\r?\n/)) {
    const parsed = line.match(/^([A-Za-z0-9_-]+):\s*(.*)$/);
    if (!parsed) continue;

    data[parsed[1]] = parsed[2].replace(/^['"]|['"]$/g, "");
  }

  return data;
}

export function pageMetadata(page: DocsPage) {
  const markdown = getPageMarkdownUrl(page);

  return {
    title: page.data.title,
    description: page.data.description,
    slug: getPageSlug(page),
    slugs: page.slugs,
    url: page.url,
    path: page.path,
    markdownUrl: markdown.url,
    markdownUrlSegments: markdown.segments,
    resourceUri: getPageResourceUri(page),
    contentRoute: docsContentRoute,
  };
}

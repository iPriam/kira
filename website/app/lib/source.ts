import { loader, type InferPageType } from "fumadocs-core/source";
import { docs } from "fumadocs-mdx:collections/server";
import { docsContentRoute, docsRoute } from "./shared";

export const source = loader({
  source: docs.toFumadocsSource(),
  baseUrl: docsRoute,
});

export function getSlugsFromParams({
  slug,
}: {
  slug?: string[] | undefined;
}) {
  return slug ?? [];
}

export function getPageMarkdownUrl(page: InferPageType<typeof source>) {
  const segments = [...page.slugs, "content.md"];

  return {
    segments,
    url: `${docsContentRoute}/${segments.join("/")}`,
  };
}

export async function getLLMText(page: InferPageType<typeof source>) {
  const processed = await page.data.getText("processed");

  return `# ${page.data.title} (${page.url})

${processed}`;
}

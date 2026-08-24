import {
  type DocsContext,
  type DocsPage,
  getLLMText,
  getPageSlug,
  pageMetadata,
} from "./docs-source";

export interface SearchResult {
  score: number;
  title: string;
  slug: string;
  url: string;
  path: string;
  snippet: string;
}

interface SearchEntry {
  page: DocsPage;
  llmText: string;
  normalized: {
    title: string;
    slug: string;
    url: string;
    text: string;
  };
}

let indexPromise: Promise<SearchEntry[]> | undefined;

export async function searchDocs(
  context: DocsContext,
  query: string,
  limit = 5,
): Promise<SearchResult[]> {
  const tokens = tokenize(query);
  if (tokens.length === 0) return [];

  const normalizedQuery = normalize(query);
  const entries = await getSearchIndex(context);

  return entries
    .map((entry) => ({
      entry,
      score: scoreEntry(entry, normalizedQuery, tokens),
    }))
    .filter((result) => result.score > 0)
    .sort((a, b) => b.score - a.score)
    .slice(0, limit)
    .map(({ entry, score }) => {
      const metadata = pageMetadata(entry.page);

      return {
        score,
        title: metadata.title,
        slug: metadata.slug,
        url: metadata.url,
        path: metadata.path,
        snippet: makeSnippet(entry.llmText, normalizedQuery, tokens),
      };
    });
}

async function getSearchIndex(context: DocsContext) {
  indexPromise ??= Promise.all(
    context.source.getPages().map(async (page) => {
      const typedPage = page as DocsPage;
      const llmText = await getLLMText(typedPage);

      return {
        page: typedPage,
        llmText,
        normalized: {
          title: normalize(typedPage.data.title ?? ""),
          slug: normalize(getPageSlug(typedPage)),
          url: normalize(typedPage.url),
          text: normalize(llmText),
        },
      };
    }),
  );

  return indexPromise;
}

function scoreEntry(entry: SearchEntry, query: string, tokens: string[]) {
  let score = 0;
  const { title, slug, url, text } = entry.normalized;
  const slugSegments = slug.split("/");

  if (slug === query) score += 100;
  if (slug.includes(query)) score += 70;
  if (title.includes(query)) score += 60;
  if (url.includes(query)) score += 25;
  if (text.includes(query)) score += 15;

  for (const token of tokens) {
    if (slugSegments.includes(token)) score += 30;
    else if (slug.includes(token)) score += 18;

    if (title.split(/\s+/).includes(token)) score += 24;
    else if (title.includes(token)) score += 14;

    if (url.includes(token)) score += 8;

    score += Math.min(countOccurrences(text, token), 10) * 2;
  }

  return score;
}

function makeSnippet(text: string, query: string, tokens: string[]) {
  const normalizedText = normalize(text);
  let index = normalizedText.indexOf(query);

  if (index < 0) {
    index = Math.min(
      ...tokens
        .map((token) => normalizedText.indexOf(token))
        .filter((candidate) => candidate >= 0),
    );
  }

  if (!Number.isFinite(index) || index < 0) {
    return compactWhitespace(text).slice(0, 280);
  }

  const start = Math.max(0, index - 120);
  const end = Math.min(text.length, index + 220);
  const prefix = start > 0 ? "..." : "";
  const suffix = end < text.length ? "..." : "";

  return `${prefix}${compactWhitespace(text.slice(start, end))}${suffix}`;
}

function tokenize(value: string) {
  return normalize(value).match(/[a-z0-9_@#:$.-]+/g) ?? [];
}

function normalize(value: string) {
  return value.toLowerCase();
}

function compactWhitespace(value: string) {
  return value.replace(/\s+/g, " ").trim();
}

function countOccurrences(value: string, token: string) {
  let count = 0;
  let index = value.indexOf(token);

  while (index >= 0) {
    count += 1;
    index = value.indexOf(token, index + token.length);
  }

  return count;
}

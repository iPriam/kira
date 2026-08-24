# Kira Docs MCP

This directory contains a local stdio MCP server for the Kira documentation.
It lets models inspect, fetch, and search the current docs from the canonical
Fumadocs source layer instead of guessing language syntax from partial context.

## Run

```bash
bun run mcp
```

For a quick end-to-end check:

```bash
bun run mcp:smoke
```

## Resources

- `docs://tree` returns the Fumadocs page tree/navigation structure.
- `docs://pages` returns all page metadata, slugs, URLs, and markdown URLs.
- `docs://page/<slug>` returns one page payload with metadata and processed
  LLM Markdown. The root page is available as `docs://page/index`.

## Tools

- `get_doc_page` accepts `slug` as a string like
  `language-guide/functions` or a string array of slug segments. It returns the
  page title, canonical docs URL, markdown-content URL, source path, slugs, and
  processed Markdown from `getLLMText(page)`.
- `search_docs` accepts a free-text `query` and optional `limit`. It ranks
  matches across page title, slug, URL, and processed Markdown text, returning
  compact snippets so a model can choose the right page before fetching it.

## Source Layer

The server prefers the existing app source module at `app/lib/source.ts` and
reuses `source`, `getPageMarkdownUrl`, and `getLLMText` when the runtime can
load processed Fumadocs Markdown directly.

For local stdio execution, the Next.js MDX transform is not always available.
When that happens, the MCP falls back to Fumadocs' runtime source builder with
the same `source.config.ts` and `content/docs` files. This is still the
canonical Fumadocs source layer; it does not crawl rendered pages, scrape HTML,
or depend on a deployed website.

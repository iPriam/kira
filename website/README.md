<picture>
  <source media="(prefers-color-scheme: dark)" srcset="Images/KiraDocumentationBannerDark.png">
  <source media="(prefers-color-scheme: light)" srcset="Images/KiraDocumentationBannerLight.png">
  <img alt="Kira Documentation" src="Images/KiraDocumentationBannerLight.png">
</picture>

# Kira Documentation Site

This app contains the Bun-powered Kira documentation website built with Next.js and Fumadocs.

## Run Locally

```bash
bun install
bun run dev
```

The dev server is served by Next.js.

## Build Static Assets

```bash
bun run build
```

Next builds the app into `.next/`.

## Preview The Static Build

```bash
bun run start
```

That serves the Next production build from `.next/`.

## Content Layout

- `app/` contains the Next.js app router pages, layout, shared helpers, and search wiring.
- `content/docs/` contains the English documentation source in MDX.
- `source.config.ts` defines the Fumadocs MDX source.

## Notes

- The site is structured for future versioning, but currently ships only the latest English docs.
- Search uses Fumadocs static Orama indexes so the site can be hosted as static files without a separate search service.

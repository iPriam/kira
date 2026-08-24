import { defineConfig, defineDocs } from "fumadocs-mdx/config";
import { rehypeCodeDefaultOptions } from "fumadocs-core/mdx-plugins";
import type { LanguageRegistration } from "shiki";

import kiraGrammar from "./lib/highlighting/kira.tmLanguage.json";
import kslGrammar from "./lib/highlighting/ksl.tmLanguage.json";

// Kira and KSL are not bundled Shiki languages, so register the repo's own
// TextMate grammars for ```kira / ```ksl fences. These are preloaded; all other
// languages (bash, ts, toml, text, ...) continue to load lazily from the bundle.
const kiraLanguages: LanguageRegistration[] = [
  kiraGrammar as unknown as LanguageRegistration,
  kslGrammar as unknown as LanguageRegistration,
];

export const docs = defineDocs({
  dir: "content/docs",
  docs: {
    postprocess: {
      includeProcessedMarkdown: true,
    },
  },
});

export default defineConfig({
  mdxOptions: {
    rehypeCodeOptions: {
      ...rehypeCodeDefaultOptions,
      langs: kiraLanguages,
    },
  },
});

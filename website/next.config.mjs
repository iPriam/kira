import { createMDX } from "fumadocs-mdx/next";
import { fileURLToPath } from "node:url";

const rootDir = fileURLToPath(new URL(".", import.meta.url));

const withMDX = createMDX({
  configPath: "./source.config.ts",
});

const nextConfig = {
  reactStrictMode: true,
  turbopack: {
    root: rootDir,
  },
  experimental: {
    optimizePackageImports: ["fumadocs-ui", "fumadocs-core"],
  },
};

export default withMDX(nextConfig);

import type { Config } from "tailwindcss";

export default {
  content: [
    "./app/**/*.{js,jsx,ts,tsx}",
    "./node_modules/fumadocs-ui/dist/**/*.{js,jsx,ts,tsx}",
  ],
} satisfies Config;

import type { Config } from "tailwindcss";

// Tailwind v4 is primarily CSS-driven (via `@import "tailwindcss"` and
// `@theme` blocks in `src/styles.css`). This file exists as an optional
// extension point for any future JS-driven customisation (custom plugins,
// runtime-computed theme tokens, etc.).
export default {
	content: ["./index.html", "./src/**/*.{ts,tsx}"],
} satisfies Config;

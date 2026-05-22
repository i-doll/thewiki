import tailwindcss from "@tailwindcss/vite";
import { TanStackRouterVite } from "@tanstack/router-plugin/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// SPA-only build: no SSR, no server functions. Output is a static `dist/`
// directory that the Rust backend embeds at build time via `rust-embed`.
export default defineConfig({
	plugins: [
		TanStackRouterVite({
			target: "react",
			autoCodeSplitting: true,
			routesDirectory: "./src/routes",
			generatedRouteTree: "./src/routeTree.gen.ts",
		}),
		react(),
		tailwindcss(),
	],
	server: {
		port: 5173,
		proxy: {
			// Forward API calls to the Rust backend during development.
			"/api": {
				target: "http://localhost:8080",
				changeOrigin: true,
			},
			// Liveness/readiness probes live at the root on the backend (not
			// under /api), so the dev proxy must forward them explicitly. In
			// production the SPA is served same-origin from the binary, so
			// these resolve directly without a proxy.
			"/healthz": {
				target: "http://localhost:8080",
				changeOrigin: true,
			},
			"/readyz": {
				target: "http://localhost:8080",
				changeOrigin: true,
			},
		},
	},
	build: {
		outDir: "dist",
		emptyOutDir: true,
		// No source maps in production — dist/ gets baked into the Rust
		// binary via rust-embed; debug info would bloat the release artefact.
		sourcemap: false,
	},
});

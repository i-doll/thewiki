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
	],
	server: {
		port: 5173,
		proxy: {
			// Forward API calls to the Rust backend during development.
			"/api": {
				target: "http://localhost:8080",
				changeOrigin: true,
			},
		},
	},
	build: {
		outDir: "dist",
		emptyOutDir: true,
		sourcemap: true,
	},
});

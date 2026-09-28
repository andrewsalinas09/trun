import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// `npm run dev` proxies the API to a running hub. Open the dev server once with
// `/?t=<token from ~/.trun/hub.token>` so the hub sets its auth cookie.
export default defineConfig({
  plugins: [svelte()],
  build: { outDir: "dist", emptyOutDir: true, target: "es2022" },
  server: {
    proxy: {
      "/api": {
        target: "http://127.0.0.1:7317",
        changeOrigin: true,
      },
    },
  },
});

import { defineConfig } from "vite";

/** Explicit synthetic-response harness. Never used by the product build. */
export default defineConfig({
  clearScreen: false,
  server: { host: "127.0.0.1", port: 1421, strictPort: true },
  build: {
    outDir: "qa-dist",
    rollupOptions: { input: "tests/ui/fixture.html" },
  },
});

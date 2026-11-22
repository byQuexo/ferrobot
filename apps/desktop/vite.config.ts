import { readFileSync } from "node:fs";

import preact from "@preact/preset-vite";
import { defineConfig, searchForWorkspaceRoot } from "vite";

// Tauri loads the built dist/ (no devUrl); `npm run dev` serves the UI in a browser on
// localhost:1420, which the daemon's CORS policy allows.
export default defineConfig({
  plugins: [preact()],
  define: {
    __APP_VERSION__: JSON.stringify(
      (
        JSON.parse(readFileSync(new URL("package.json", import.meta.url), "utf8")) as {
          version: string;
        }
      ).version,
    ),
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    // The in-app guides are the wiki pages in docs/wiki.
    fs: { allow: [searchForWorkspaceRoot(process.cwd()), "../../docs/wiki"] },
  },
  preview: { port: 1420, strictPort: true },
  build: { target: "es2022", outDir: "dist", emptyOutDir: true, sourcemap: false },
});

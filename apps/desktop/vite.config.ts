import preact from "@preact/preset-vite";
import { defineConfig } from "vite";

// Tauri serves the built files; there is no dev server in the Tauri config.
// `npm run dev` is for working on the UI in a browser at localhost:1420
// (allowed by the daemon's CORS policy).
export default defineConfig({
  plugins: [preact()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  preview: { port: 1420, strictPort: true },
  build: { target: "es2022", outDir: "dist", emptyOutDir: true, sourcemap: false },
});

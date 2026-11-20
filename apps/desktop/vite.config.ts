import preact from "@preact/preset-vite";
import { defineConfig } from "vite";

// Tauri loads the built dist/ (no devUrl); `npm run dev` serves the UI in a browser on
// localhost:1420, which the daemon's CORS policy allows.
export default defineConfig({
  plugins: [preact()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  preview: { port: 1420, strictPort: true },
  build: { target: "es2022", outDir: "dist", emptyOutDir: true, sourcemap: false },
});

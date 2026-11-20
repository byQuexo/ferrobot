// @ts-check
import js from "@eslint/js";
import tseslint from "typescript-eslint";

export default tseslint.config(
  {
    ignores: [
      "dist/",
      "node_modules/",
      "src-tauri/",
      "public/",
      "src/generated/",
      "eslint.config.js",
    ],
  },
  js.configs.recommended,
  ...tseslint.configs.strictTypeChecked,
  {
    languageOptions: {
      parserOptions: { projectService: true, tsconfigRootDir: import.meta.dirname },
    },
    rules: {
      "@typescript-eslint/restrict-template-expressions": ["error", { allowNumber: true }],
      "@typescript-eslint/no-floating-promises": [
        "error",
        {
          allowForKnownSafeCalls: [
            { from: "package", package: "node:test", name: ["describe", "it"] },
          ],
        },
      ],
      "no-restricted-syntax": [
        "error",
        { selector: "ExportDefaultDeclaration", message: "Use named exports." },
      ],
    },
  },
  // Vite reads its config from a default export.
  { files: ["vite.config.ts"], rules: { "no-restricted-syntax": "off" } },
);

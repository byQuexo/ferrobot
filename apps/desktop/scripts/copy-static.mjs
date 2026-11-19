// Copies the static frontend files next to the compiled JS.
import { copyFileSync, mkdirSync } from "node:fs";

mkdirSync("dist", { recursive: true });
for (const file of ["index.html", "styles.css"]) {
  copyFileSync(`src/${file}`, `dist/${file}`);
}

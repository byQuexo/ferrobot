import { render } from "preact";

import { App } from "./app.tsx";
import { applyAppearance, loadAppearance } from "./lib/appearance.ts";
import "./styles.css";

applyAppearance(loadAppearance());
// The shell leaves file drops to the page; a stray one must not open the file instead of the app.
for (const type of ["dragover", "drop"]) {
  window.addEventListener(type, (e) => {
    e.preventDefault();
  });
}

const root = document.getElementById("app");
if (root) render(<App />, root);

import { render } from "preact";

import { App } from "./app.tsx";
import { applyAppearance, loadAppearance } from "./lib/appearance.ts";
import "./styles.css";

applyAppearance(loadAppearance());

const root = document.getElementById("app");
if (root) render(<App />, root);

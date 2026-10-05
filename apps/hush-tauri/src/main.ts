import "./styles/tokens.css";
import "./styles/base.css";
import "./styles/motion.css";
import "./styles/app.css";
import { applyTheme, getThemeMode } from "./lib/theme";
import { initPerfGuard } from "./lib/perf";
import { App } from "./app";

applyTheme(getThemeMode());
initPerfGuard();

const root = document.getElementById("app");
if (!root) throw new Error("#app missing");

const app = new App(root);
void app.start();

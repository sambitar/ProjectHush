import "./styles/tokens.css";
import "./styles/base.css";
import "./styles/motion.css";
import "./styles/app.css";
import { initThemeFromDaemon } from "./lib/theme";
import { initPerfGuard } from "./lib/perf";
import { App } from "./app";

const root = document.getElementById("app");
if (!root) throw new Error("#app missing");

const app = new App(root);
void initThemeFromDaemon().then(() => {
  initPerfGuard();
  return app.start();
});

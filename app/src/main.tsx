import "@fontsource-variable/onest";
import "./styles.css";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);

// Для сквозных проверок в dev-сборке (см. --dev-eval в lib.rs).
if (import.meta.env.DEV) {
  void import("./lib/store").then(({ useStore }) => Object.assign(window, { __store: useStore }));
}

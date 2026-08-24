import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
import "./styles.css";

function showFatal(msg: string) {
  document.body.style.cssText = "background:#fff;color:#c00;padding:20px;font-family:monospace";
  document.body.innerHTML = "<pre><b>Fatal error:</b>\n" + msg + "</pre>";
}

const root = document.getElementById("root");
if (!root) throw new Error("#root is missing from index.html");

ReactDOM.createRoot(root, {
  onUncaughtError(error) {
    showFatal(error instanceof Error ? (error.stack ?? error.message) : String(error));
  },
  onRecoverableError(error) {
    console.warn("Recoverable React error:", error);
  },
}).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </React.StrictMode>,
);

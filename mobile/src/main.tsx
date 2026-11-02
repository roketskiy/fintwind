import { createRoot } from "react-dom/client";
import { App } from "@/app";
import { AppBoundary } from "@/components/app-boundary";
import "@/index.css";
import { applyTheme, savedTheme } from "@/lib/theme";

applyTheme(savedTheme());

createRoot(document.getElementById("root")!).render(
  <AppBoundary>
    <App />
  </AppBoundary>,
);

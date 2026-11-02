import { createRoot } from "react-dom/client";
import { App } from "@/app";
import { AppBoundary } from "@/components/app-boundary";
import "@/index.css";

createRoot(document.getElementById("root")!).render(
  <AppBoundary>
    <App />
  </AppBoundary>,
);

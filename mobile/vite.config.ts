import { fileURLToPath, URL } from "node:url";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) } },
  build: {
    sourcemap: false,
    // Production only allows same-origin fonts, including the small KaTeX faces.
    assetsInlineLimit: (path) => /\.(woff2?|ttf|otf)$/i.test(path) ? false : undefined,
  },
});

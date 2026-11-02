import { fileURLToPath, URL } from "node:url";
import { defineConfig } from "vite";
import base from "./vite.config";

// An explicit, separate build. Never a production input or a real pairing.
export default defineConfig({
  ...base,
  resolve: { alias: [
    { find: "@/lib/remote-client", replacement: fileURLToPath(new URL("./src/fixtures/remote-client.ts", import.meta.url)) },
    { find: "@", replacement: fileURLToPath(new URL("./src", import.meta.url)) },
  ] },
  build: { outDir: "../target/mobile-visual", emptyOutDir: true, rollupOptions: { input: "remote-fixture.html" } },
});

import { defineConfig, mergeConfig } from "vite";
import base from "./vite.config";

// Separate entry points: neither the fixtures nor this page are in the production build.
export default mergeConfig(base, defineConfig({
  build: {
    outDir: "../target/mobile-visual",
    emptyOutDir: true,
    rollupOptions: { input: ["visual.html", "visual-frame.html"] },
  },
}));

import { defineConfig } from "vite";
import { fileURLToPath } from "node:url";
import activeScene from "./scenes/active.json" with { type: "json" };

const app = fileURLToPath(new URL(".", import.meta.url));
if (typeof activeScene.folder !== "string" || !/^[a-zA-Z0-9_-]+$/.test(activeScene.folder)) {
  throw new Error("scenes/active.json must name a folder using letters, digits, hyphens or underscores.");
}
const sceneProfile = fileURLToPath(new URL(`./scenes/${activeScene.folder}/profile.json`, import.meta.url));

export default defineConfig({
  resolve: { alias: { "@active-scene": sceneProfile } },
  clearScreen: false,
  server: {
    host: "127.0.0.1",
    port: 1443,
    strictPort: true,
    fs: { strict: true, allow: [app] },
    watch: {
      ignored: ["**/src-tauri/**", "**/.local/**", "**/release/**", "**/dist/**"]
    }
  },
  build: { target: "es2022", assetsInlineLimit: 0 }
});


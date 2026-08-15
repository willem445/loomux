import { defineConfig } from "vite";

export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    // The repo is a cargo workspace (root Cargo.toml), so build output lives in the
    // workspace-root `target/`, not `src-tauri/target/` (#989). Vite must not watch it:
    // during a fresh `tauri dev` the watcher races cargo writing a build script and dies
    // with EBUSY. `node_modules` is excluded defensively for the same reason.
    watch: { ignored: ["**/src-tauri/**", "**/target/**", "**/node_modules/**"] },
  },
  build: { target: "es2022" },
});

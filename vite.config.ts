import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

// Tauri's dev server for Glimpsfy. The port is 1430 and strict, because the author's other
// Tauri app keeps 1420 open all the time; a busy port fails the start instead of moving.
export default defineConfig(() => ({
  // `g-icon` is the kit's icon element, styled by the kit's CSS and filled by `GIcon.vue`.
  plugins: [vue({ template: { compilerOptions: { isCustomElement: (tag) => tag === "g-icon" } } })],
  clearScreen: false,
  server: {
    port: 1430,
    strictPort: true,
    host: false,
    watch: {
      ignored: ["**/src-tauri/**", "**/crates/**", "**/target/**"],
    },
  },
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
}));

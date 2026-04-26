import { defineConfig } from "vitest/config";
import celox from "@celox-sim/vite-plugin";

export default defineConfig({
  plugins: [celox()],
  test: {
    exclude: ["test/e2e/**/*.spec.ts"],
  },
});

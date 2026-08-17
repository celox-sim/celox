import { configDefaults, defineConfig } from "vitest/config";
import celox from "@celox-sim/vite-plugin";

export default defineConfig({
	plugins: [celox()],
	test: {
		exclude: [
			...configDefaults.exclude,
			"test/e2e/**/*.spec.ts",
			"test/fixtures/**",
		],
	},
});

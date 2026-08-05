import celox from "@celox-sim/vite-plugin";
import { defineConfig } from "vitest/config";

export default defineConfig({
	root: import.meta.dirname,
	plugins: [
		celox({
			testbenchComponents: "./tb-components.ts",
		}),
	],
	test: { include: ["entry.test.ts"] },
});

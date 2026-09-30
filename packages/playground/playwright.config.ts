import { defineConfig } from "@playwright/test";

export default defineConfig({
	testDir: "./test/e2e",
	timeout: 30_000,
	// Include server startup and teardown in the deadline as well as tests.
	globalTimeout: 240_000,
	use: {
		baseURL: "http://127.0.0.1:4173",
		headless: true,
	},
	webServer: {
		// Start Vite directly so Playwright can terminate it without pnpm
		// leaving the server running and hanging teardown.
		command:
			"node node_modules/vite/bin/vite.js preview --host 127.0.0.1 --port 4173",
		// Check the same IPv4 HTTP endpoint used by the tests. The port-only
		// probe also checks IPv6 and can stall before Vite starts.
		url: "http://127.0.0.1:4173",
		reuseExistingServer: !process.env.CI,
		timeout: 120_000,
	},
});

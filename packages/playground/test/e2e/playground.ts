import { expect, type Page } from "@playwright/test";

export type PlaygroundTestMarker = {
	code?: string | number;
	message: string;
};

type PlaygroundTestApi = {
	loadExample: (name: string) => void;
	setFileContent: (path: string, content: string) => void;
	getModelMarkers: (path: string) => PlaygroundTestMarker[];
	getTypeScriptDiagnostics: (
		path: string,
	) => Promise<PlaygroundTestMarker[]>;
	getStatusText: () => string;
};

declare global {
	interface Window {
		__CELOX_PLAYGROUND_TEST_API__?: PlaygroundTestApi;
	}
}

export type BrowserErrorMonitor = {
	errors: string[];
	assertEmpty: () => void;
};

export async function openPlayground(
	page: Page,
): Promise<BrowserErrorMonitor> {
	const errors: string[] = [];
	page.on("console", (message) => {
		if (message.type() === "error") errors.push(message.text());
	});
	const pageError = new Promise<never>((_, reject) => {
		page.on("pageerror", (error) => {
			errors.push(error.message);
			reject(new Error(`Playground browser error: ${error.message}`));
		});
	});

	const response = await page.goto("/");
	expect(response?.headers()["cross-origin-opener-policy"]).toBe("same-origin");
	expect(response?.headers()["cross-origin-embedder-policy"]).toBe(
		"credentialless",
	);
	expect(await page.evaluate(() => window.crossOriginIsolated)).toBe(true);

	await Promise.race([
		page.waitForFunction(() => {
			const api = window.__CELOX_PLAYGROUND_TEST_API__;
			return api && api.getStatusText() !== "Loading WASM…";
		}),
		pageError,
	]);
	const status = await page.evaluate(() =>
		window.__CELOX_PLAYGROUND_TEST_API__?.getStatusText(),
	);
	expect(status, `Browser errors: ${errors.join("\n") || "none"}`).toBe(
		"Ready",
	);

	return {
		errors,
		assertEmpty() {
			expect(errors).toEqual([]);
		},
	};
}

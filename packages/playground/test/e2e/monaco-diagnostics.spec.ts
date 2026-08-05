import { expect, type Page, test } from "@playwright/test";

type PlaygroundTestMarker = {
	code?: string | number;
	message: string;
};

declare global {
	interface Window {
		__CELOX_PLAYGROUND_TEST_API__?: {
			loadExample: (name: string) => void;
			setFileContent: (path: string, content: string) => void;
			getModelMarkers: (path: string) => PlaygroundTestMarker[];
			getTypeScriptDiagnostics: (path: string) => Promise<PlaygroundTestMarker[]>;
			getStatusText: () => string;
		};
	}
}

const bigintTestbench = `import { describe, it, expect } from "vitest";

describe("bigint literals", () => {
	it("accepts bigint literals in Monaco diagnostics", () => {
		const value = 100n;
		expect(value).toBe(100n);
	});
});
`;

async function openPlayground(page: Page) {
	const browserErrors: string[] = [];
	page.on("console", (message) => {
		if (message.type() === "error") browserErrors.push(message.text());
	});
	const pageError = new Promise<never>((_, reject) => {
		page.on("pageerror", (error) => {
			browserErrors.push(error.message);
			reject(new Error(`Playground browser error: ${error.message}`));
		});
	});

	await page.goto("/");

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
	expect(
		status,
		`Browser errors: ${browserErrors.join("\n") || "none"}`,
	).toBe("Ready");

	return browserErrors;
}

test("playground starts and accepts bigint literals", async ({ page }) => {
	const browserErrors = await openPlayground(page);
	await expect(page.locator("#editor .monaco-editor")).toBeVisible();

	await page.evaluate((source) => {
		const api = window.__CELOX_PLAYGROUND_TEST_API__;
		if (!api) throw new Error("Missing playground test API");
		api.loadExample("adder");
		api.setFileContent("test/adder.test.ts", source);
	}, bigintTestbench);

	await expect
		.poll(
			async () => {
				const diagnostics = await page.evaluate(async () => {
					const api = window.__CELOX_PLAYGROUND_TEST_API__;
					if (!api) throw new Error("Missing playground test API");
					return api.getTypeScriptDiagnostics("test/adder.test.ts");
				});
				return !diagnostics.some((marker) => {
					const code = String(marker.code ?? "");
					return (
						code === "2737" ||
						/BigInt literals are not available when targeting lower than ES2020/i.test(
							marker.message,
						)
					);
				});
			},
			{ timeout: 60_000 },
		)
		.toBe(true);
	expect(browserErrors).toEqual([]);
});

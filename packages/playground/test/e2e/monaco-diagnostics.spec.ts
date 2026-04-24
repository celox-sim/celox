import { expect, test } from "@playwright/test";

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

test("playground does not report TS2737 for bigint literals", async ({ page }) => {
	await page.goto("/");

	await page.waitForFunction(
		() => typeof window.__CELOX_PLAYGROUND_TEST_API__ !== "undefined",
	);

	await page.evaluate((source) => {
		const api = window.__CELOX_PLAYGROUND_TEST_API__;
		if (!api) throw new Error("Missing playground test API");
		api.loadExample("adder");
		api.setFileContent("test/adder.test.ts", source);
	}, bigintTestbench);

	await expect
		.poll(
			async () => {
				const markers = await page.evaluate(() => {
					const api = window.__CELOX_PLAYGROUND_TEST_API__;
					if (!api) throw new Error("Missing playground test API");
					return api.getModelMarkers("test/adder.test.ts");
				});
				return !markers.some((marker) => {
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
});

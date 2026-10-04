import { expect, test } from "@playwright/test";
import { openPlayground } from "./playground.js";

const bigintTestbench = `import { describe, it, expect } from "vitest";

describe("bigint literals", () => {
	it("accepts bigint literals in Monaco diagnostics", () => {
		const value = 100n;
		expect(value).toBe(100n);
	});
});
`;

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
	browserErrors.assertEmpty();
});

test("every bundled example has no TypeScript diagnostics", async ({
	page,
}) => {
	test.setTimeout(120_000);
	const browserErrors = await openPlayground(page);

	const examples = await page
		.locator("#examples option")
		.evaluateAll((options) =>
			options
				.map((option) => (option as HTMLOptionElement).value)
				.filter(Boolean),
		);
	expect(examples.length).toBeGreaterThan(0);

	for (const example of examples) {
		await test.step(example, async () => {
			const testFile = `test/${example}.test.ts`;
			await page.evaluate((name) => {
				const api = window.__CELOX_PLAYGROUND_TEST_API__;
				if (!api) throw new Error("Missing playground test API");
				api.loadExample(name);
			}, example);

			// DUT types are injected after the Veryl source compiles, so poll
			// until the diagnostics settle to an empty list.
			await expect
				.poll(
					() =>
						page.evaluate(async (path) => {
							const api = window.__CELOX_PLAYGROUND_TEST_API__;
							if (!api) throw new Error("Missing playground test API");
							const diagnostics = await api.getTypeScriptDiagnostics(path);
							return diagnostics.map(
								(marker) => `TS${marker.code ?? "?"}: ${marker.message}`,
							);
						}, testFile),
					{ timeout: 30_000 },
				)
				.toEqual([]);
		});
	}

	browserErrors.assertEmpty();
});

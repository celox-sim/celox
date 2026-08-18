import { expect, type Page, test } from "@playwright/test";
import { openPlayground } from "./playground.js";

const RUN_TIMEOUT_MS = 15_000;
const MAX_MAIN_THREAD_GAP_MS = 1_000;

const HIERARCHICAL_ADDER = `module Adder (
    clk: input clock,
    rst: input reset,
    a:   input logic<16>,
    b:   input logic<16>,
    sum: output logic<17>,
) {
    inst u_leaf: AddLeaf (
        a,
        b,
        sum,
    );
}

module AddLeaf (
    a:   input logic<16>,
    b:   input logic<16>,
    sum: output logic<17>,
) {
    assign sum = a + b;
}
`;

async function runAndExpectPass(page: Page, expectedPassed = 2) {
	await page.locator("#run").click();

	try {
		await page.waitForFunction(
			() => {
				const run = document.querySelector<HTMLButtonElement>("#run");
				const status = document.querySelector("#status")?.textContent;
				return run?.disabled === false && status !== "Ready";
			},
			undefined,
			{ timeout: RUN_TIMEOUT_MS },
		);
	} catch {
		const status = await page.locator("#status").textContent();
		const output = await page.locator("#console").textContent();
		throw new Error(
			`Run did not finish within ${RUN_TIMEOUT_MS}ms (status: ${status ?? "missing"}).\n${output ?? ""}`,
		);
	}

	const status = await page.locator("#status").textContent();
	const output = await page.locator("#console").textContent();
	expect(status, output ?? "No playground output").toBe("Done");
	expect(output).toContain(`${expectedPassed} passed`);
}

test("Run stays responsive while compiling and finishes before the deadline", async ({
	page,
}) => {
	const browserErrors = await openPlayground(page);

	await page.evaluate(() => {
		const scope = globalThis as typeof globalThis & {
			__celoxHeartbeat?: { ticks: number; maxGapMs: number };
		};
		const heartbeat = { ticks: 0, maxGapMs: 0 };
		scope.__celoxHeartbeat = heartbeat;
		let previous = performance.now();
		setInterval(() => {
			const now = performance.now();
			heartbeat.ticks++;
			heartbeat.maxGapMs = Math.max(heartbeat.maxGapMs, now - previous);
			previous = now;
		}, 10);
	});
	await page.waitForFunction(
		() =>
			(
				globalThis as typeof globalThis & {
					__celoxHeartbeat?: { ticks: number };
				}
			).__celoxHeartbeat?.ticks,
	);

	await runAndExpectPass(page);

	const heartbeat = await page.evaluate(
		() =>
			(
				globalThis as typeof globalThis & {
					__celoxHeartbeat?: { ticks: number; maxGapMs: number };
				}
			).__celoxHeartbeat,
	);
	expect(heartbeat?.ticks).toBeGreaterThan(1);
	expect(heartbeat?.maxGapMs).toBeLessThan(MAX_MAIN_THREAD_GAP_MS);
	browserErrors.assertEmpty();
});

test("every bundled example runs successfully", async ({ page }) => {
	test.setTimeout(90_000);
	const browserErrors = await openPlayground(page);

	for (const [example, expectedPassed] of Object.entries({
		adder: 2,
		counter: 2,
		counter_sim: 2,
		counter_vcd: 2,
		four_state: 5,
	})) {
		await test.step(example, async () => {
			await page.evaluate((name) => {
				const api = window.__CELOX_PLAYGROUND_TEST_API__;
				if (!api) throw new Error("Missing playground test API");
				api.loadExample(name);
			}, example);
			await runAndExpectPass(page, expectedPassed);
		});
	}

	browserErrors.assertEmpty();
});

test("a hierarchical design compiles and runs in the browser", async ({ page }) => {
	const browserErrors = await openPlayground(page);
	await page.evaluate((source) => {
		const api = window.__CELOX_PLAYGROUND_TEST_API__;
		if (!api) throw new Error("Missing playground test API");
		api.setFileContent("src/Adder.veryl", source);
	}, HIERARCHICAL_ADDER);

	await runAndExpectPass(page);
	browserErrors.assertEmpty();
});

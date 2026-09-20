import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { describe, expect, test } from "vitest";

const PROBE_PATH = fileURLToPath(
	new URL("../fixtures/napi-shared-memory-lifetime-child.mjs", import.meta.url),
);

const HANDLE_VARIANTS = [
	["NativeSimulatorHandle", "default"],
	["NativeSimulatorHandle (tiered)", "tiered"],
	["NativeSimulationHandle", "simulation"],
] as const;
const LIFETIME_ENDS = ["dispose", "gc"] as const;

describe.each(HANDLE_VARIANTS)("%s shared memory lifetime", (_, handleKind) => {
	test.each(LIFETIME_ENDS)("keeps existing views safe after %s", (mode) => {
		const result = spawnSync(
			process.execPath,
			["--expose-gc", PROBE_PATH, handleKind, mode],
			{
				encoding: "utf8",
				timeout: 75_000,
				windowsHide: true,
			},
		);
		const diagnostics = [
			`status: ${String(result.status)}`,
			`signal: ${String(result.signal)}`,
			`error: ${String(result.error)}`,
			`stdout: ${result.stdout}`,
			`stderr: ${result.stderr}`,
		].join("\n");

		expect(result.status, diagnostics).toBe(0);
	});
});

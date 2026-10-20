import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const vitest = join(
	dirname(fileURLToPath(import.meta.resolve("vitest/package.json"))),
	"vitest.mjs",
);
for (const layout of ["internal", "external"]) {
	const cache = mkdtempSync(join(tmpdir(), "celox-vitest-cache-"));
	const client =
		layout === "external"
			? mkdtempSync(join(import.meta.dirname, ".cache-client-"))
			: undefined;
	try {
		for (const phase of ["cold", "warm"]) {
			const result = spawnSync(
				process.execPath,
				[
					vitest,
					"run",
					"--config",
					join(import.meta.dirname, "vitest.config.ts"),
					"--fsModuleCachePath",
					cache,
				],
				{
					cwd: import.meta.dirname,
					env: { ...process.env, CELOX_TEST_EXTERNAL_VITE_ROOT: client ?? "" },
					encoding: "utf8",
					timeout: 120_000,
				},
			);
			process.stdout.write(result.stdout ?? "");
			process.stderr.write(result.stderr ?? "");
			assert.ifError(result.error);
			assert.equal(
				result.status,
				0,
				`${layout} ${phase} module-cache run failed`,
			);
		}
	} finally {
		rmSync(cache, { recursive: true, force: true });
		if (client) rmSync(client, { recursive: true, force: true });
	}
}

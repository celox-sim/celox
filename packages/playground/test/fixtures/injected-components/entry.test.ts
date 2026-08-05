import { existsSync, readFileSync } from "node:fs";
import { expect, test } from "vitest";
import "./src/InjectedTestbench.veryl";

test("generates the injected component manifest sidecar", () => {
	const path = `${import.meta.dirname}/.celox/testbench-components.manifest.json`;
	expect(existsSync(path)).toBe(true);
	const sidecar = JSON.parse(readFileSync(path, "utf8")) as {
		types: Record<string, unknown>;
	};
	expect(sidecar.types).toHaveProperty("vite_store");
});

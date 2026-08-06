import { existsSync, readFileSync } from "node:fs";
import { expect, test } from "vitest";
import "./src/InjectedTestbench.veryl";

test("generates the injected component manifest sidecar", () => {
	const path = `${import.meta.dirname}/.celox/testbench-components/veryl.manifest.json`;
	expect(existsSync(path)).toBe(true);
	const sidecar = JSON.parse(readFileSync(path, "utf8")) as {
		source: string;
		types: Record<string, unknown>;
	};
	expect(sidecar.source).toBe("tb-components.ts");
	expect(sidecar.types).toHaveProperty("vite_store");
});

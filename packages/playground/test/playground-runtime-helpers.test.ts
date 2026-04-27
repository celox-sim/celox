import { describe, expect, test } from "vitest";

import { FourState, X, Z } from "../src/playground-runtime-helpers";

describe("playground-runtime-helpers", () => {
	test("provides 4-state sentinels compatible with celox runtime encoding", () => {
		expect(X).toBe(Symbol.for("veryl:X"));
		expect(Z).toBe(Symbol.for("veryl:Z"));

		const value = FourState(0x05, 0xf0);

		expect(value).toEqual({
			__fourState: true,
			value: 0x05n,
			mask: 0xf0n,
		});
	});
});

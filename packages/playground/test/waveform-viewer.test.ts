import { expect, test } from "vitest";
import { generateVcdText } from "../src/waveform-viewer";

test("downloaded Veryl waveforms use picosecond simulation ticks", () => {
	const text = generateVcdText({ signals: [{ name: "y", width: 1 }], snapshots: [{ timestamp: 123, values: [1n] }] });
	expect(text).toContain("$timescale 1ps $end");
	expect(text).toContain("#123\n");
});

#!/usr/bin/env node
// Re-render captured JSON after updating the viewer, without recompilation.
import { readFileSync, writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

export function renderReport(report) {
	if (report.schema !== 1 || !Array.isArray(report.events)) {
		throw new Error("Expected a schema 1 parallel-report JSON document");
	}
	const template = readFileSync(new URL("../crates/celox-bench/src/bin/parallel-report/report.html", import.meta.url), "utf8");
	const viewer = readFileSync(new URL("./parallel-report.mjs", import.meta.url), "utf8");
	// Function replacements preserve literal $ sequences in source code.
	return template.replace("__REPORT_JS__", () => viewer).replace("__REPORT_JSON__", () => JSON.stringify(report).replaceAll("<", "\\u003c"));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
	const [, , input, output] = process.argv;
	if (!input || !output) throw new Error("Usage: node scripts/render-parallel-report.mjs report.json report.html");
	writeFileSync(output, renderReport(JSON.parse(readFileSync(input, "utf8"))));
}

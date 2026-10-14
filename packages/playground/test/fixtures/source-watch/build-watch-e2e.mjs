import assert from "node:assert/strict";
import {
	existsSync,
	mkdirSync,
	mkdtempSync,
	readFileSync,
	rmSync,
	writeFileSync,
} from "node:fs";
import { join } from "node:path";
import celox from "../../../../vite-plugin/dist/index.js";

// Also exercise the oldest supported Vite using an installed module URL.
const { build, version } = await import(process.env.CELOX_TEST_VITE || "vite");
for (const layout of ["external", "default", "custom"]) {
	const fixture = mkdtempSync(join(import.meta.dirname, ".build-project-"));
	const root = join(fixture, "hdl");
	const viteRoot = layout === "external" ? join(fixture, "client") : root;
	let watcher;
	let builds = 0;
	let failure;
	async function waitForBuild(previous) {
		const deadline = Date.now() + 15_000;
		while (builds <= previous && !failure && Date.now() < deadline)
			await new Promise((resolve) => setTimeout(resolve, 25));
		if (failure) throw failure;
		assert.ok(builds > previous, "build watch missed a project change");
		await new Promise((resolve) => setTimeout(resolve, 500));
		// More than one notification can arrive for a catalogue edit, but output
		// writes must never sustain a rebuild cycle.
		const maximum = 2;
		assert.ok(
			builds <= previous + maximum,
			"generated files caused a rebuild loop",
		);
	}
	try {
		mkdirSync(join(root, "src"), { recursive: true });
		mkdirSync(join(root, "tb"));
		mkdirSync(viteRoot, { recursive: true });
		writeFileSync(
			join(root, "Veryl.toml"),
			'[project]\nname="build_watch"\nversion="0.1.0"\n[build]\nsources=["src"]\n',
		);
		writeFileSync(
			join(root, "src/Main.veryl"),
			"module Main(a: input logic, o: output logic) { assign o = a; }\n",
		);
		writeFileSync(join(root, "tb/TestOnly.veryl"), "module TestOnly {}\n");
		writeFileSync(
			join(viteRoot, "entry.ts"),
			`export { Main } from ${JSON.stringify(join(root, "src/Main.veryl"))};`,
		);
		watcher = await build({
			root: fixture,
			configFile: false,
			publicDir: false,
			logLevel: "silent",
			plugins: [
				celox(layout === "external" ? { projectRoot: root } : undefined),
				{ name: "fixture-final-root", config: () => ({ root: viteRoot }) },
			],
			build: {
				watch: {},
				outDir: layout === "custom" ? "build-output" : "dist",
				lib: {
					entry: join(viteRoot, "entry.ts"),
					formats: ["es"],
					fileName: "watch-fixture",
				},
			},
		});
		watcher.on("event", (event) => {
			if (event.code === "ERROR") failure = event.error;
			if (event.code === "END") builds++;
		});
		await waitForBuild(0);
		for (const [name, edit, verify] of [
			[
				"new source",
				() => writeFileSync(join(root, "src/Extra.veryl"), "module Extra {}\n"),
				() => assert.ok(existsSync(join(root, ".celox/src/Extra.d.veryl.ts"))),
			],
			[
				"source deletion",
				() => rmSync(join(root, "src/Extra.veryl")),
				() => assert.ok(!existsSync(join(root, ".celox/src/Extra.d.veryl.ts"))),
			],
			[
				"optional config creation",
				() =>
					writeFileSync(join(root, "celox.toml"), '[test]\nsources=["tb"]\n'),
				() =>
					assert.ok(existsSync(join(root, ".celox/tb/TestOnly.d.veryl.ts"))),
			],
			[
				"optional config deletion",
				() => rmSync(join(root, "celox.toml")),
				() =>
					assert.ok(!existsSync(join(root, ".celox/tb/TestOnly.d.veryl.ts"))),
			],
			[
				"lockfile edit",
				() => {
					const lock = join(root, "Veryl.lock");
					writeFileSync(
						lock,
						`${readFileSync(lock, "utf8")}\n# external edit\n`,
					);
				},
				() => {},
			],
		]) {
			const previous = builds;
			edit();
			await waitForBuild(previous);
			verify();
			console.log(`passed: Vite ${version} ${layout} build watch ${name}`);
		}
	} finally {
		await watcher?.close();
		rmSync(fixture, { recursive: true, force: true });
	}
}

import { type ChildProcess, spawn } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import {
	existsSync,
	mkdirSync,
	mkdtempSync,
	readdirSync,
	readFileSync,
	rmSync,
	statSync,
	utimesSync,
	writeFileSync,
} from "node:fs";
import { hostname, tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { afterEach, expect, test } from "vitest";
import { generateSidecars } from "../../vite-plugin/src/sidecar.js";
import { withSidecarLock } from "../../vite-plugin/src/sidecar-lock.js";
import type { GenTsJsonOutput } from "../../vite-plugin/src/types.js";

const roots: string[] = [];
const children = new Set<ChildProcess>();
afterEach(async () => {
	for (const child of children) {
		if (child.exitCode === null && child.signalCode === null)
			child.kill("SIGKILL");
	}
	await Promise.all(
		[...children].map((child) =>
			child.exitCode !== null || child.signalCode !== null
				? Promise.resolve()
				: new Promise<void>((resolve) => child.once("exit", () => resolve())),
		),
	);
	for (const root of roots.splice(0))
		rmSync(root, { recursive: true, force: true });
	children.clear();
});
function root() {
	const path = mkdtempSync(join(tmpdir(), "celox-sidecar-lock-"));
	roots.push(path);
	return path;
}
const delay = (ms: number) =>
	new Promise<void>((resolve) => setTimeout(resolve, ms));

test("a stale catalogue cannot sweep another generation's new declaration", async () => {
	const path = root();
	writeFileSync(join(path, "Main.veryl"), "module Main {}\n");
	const module = (name: string) => ({
		moduleName: name,
		sourceFile: `${name}.veryl`,
		dtsContent: `export declare const ${name}: {};\n`,
		mdContent: "",
		ports: {},
		events: [],
		isTest: false,
	});
	const old: GenTsJsonOutput = {
		projectPath: path,
		fileModules: { "Main.veryl": ["Main"] },
		modules: [module("Main")],
	};
	let captured!: () => void;
	const snapshot = new Promise<void>((resolve) => {
		captured = resolve;
	});
	let resume!: () => void;
	const paused = new Promise<void>((resolve) => {
		resume = resolve;
	});
	const older = withSidecarLock(path, async () => {
		captured();
		await paused;
		generateSidecars(old, path);
	});
	await snapshot;
	writeFileSync(join(path, "Added.veryl"), "module Added {}\n");
	const current = {
		...old,
		fileModules: { ...old.fileModules, "Added.veryl": ["Added"] },
		modules: [...old.modules, module("Added")],
	};
	const newer = withSidecarLock(path, () => generateSidecars(current, path));
	try {
		// Without serialization the newer writer finishes while the older
		// snapshot is paused, and the older sweep then deletes Added.
		await Promise.race([newer, delay(100)]);
	} finally {
		resume();
	}
	await Promise.all([older, newer]);
	expect(existsSync(join(path, ".celox/Added.d.veryl.ts"))).toBe(true);
});

function child(path: string, body: string) {
	const url = pathToFileURL(
		join(import.meta.dirname, "../../vite-plugin/dist/sidecar-lock.js"),
	).href;
	const code = `import { withSidecarLock } from ${JSON.stringify(url)};
		import { readFileSync, writeFileSync, unlinkSync } from 'node:fs';
		import { join } from 'node:path';
		const path = process.argv[1]; ${body}`;
	const processChild = spawn(process.execPath, [
		"--input-type=module",
		"-e",
		code,
		path,
	]);
	children.add(processChild);
	let output = "";
	processChild.stdout!.on("data", (data) => {
		output += data;
	});
	processChild.stderr!.on("data", (data) => {
		output += data;
	});
	const exited = new Promise<void>((resolve, reject) => {
		processChild.once("error", reject);
		processChild.once("exit", (code, signal) =>
			code === 0 || signal === "SIGKILL"
				? resolve()
				: reject(new Error(output)),
		);
	});
	return { processChild, exited, output: () => output };
}

test("multiple processes keep their generation sections exclusive", async () => {
	const path = root();
	writeFileSync(join(path, "counter"), "0");
	const contenders = Array.from({ length: 4 }, () =>
		child(
			path,
			`
		for (let i = 0; i < 4; i++) await withSidecarLock(path, async () => {
			const active = join(path, 'active');
			writeFileSync(active, String(process.pid), { flag: 'wx' });
			const count = Number(readFileSync(join(path, 'counter'), 'utf8'));
			await new Promise(resolve => setTimeout(resolve, 5));
			writeFileSync(join(path, 'counter'), String(count + 1));
			unlinkSync(active);
		});`,
		),
	);
	await Promise.all(contenders.map((item) => item.exited));
	expect(readFileSync(join(path, "counter"), "utf8")).toBe("16");
});

test("a crashed owner does not prevent the next generation", async () => {
	const path = root();
	const owner = child(
		path,
		`await withSidecarLock(path, async () => {
		writeFileSync(join(path, 'ready'), 'ready');
		await new Promise(resolve => setTimeout(resolve, 30_000));
	});`,
	);
	try {
		const deadline = Date.now() + 3000;
		while (!existsSync(join(path, "ready"))) {
			if (Date.now() > deadline) throw new Error(owner.output());
			await delay(10);
		}
	} finally {
		owner.processChild.kill("SIGKILL");
		await owner.exited;
	}
	expect(await withSidecarLock(path, () => "recovered")).toBe("recovered");
});

test("a failed generation releases its claim", async () => {
	const path = root();
	await expect(
		withSidecarLock(path, () => {
			throw new Error("generation failed");
		}),
	).rejects.toThrow("generation failed");
	expect(await withSidecarLock(path, () => "next")).toBe("next");
});

function abandoned(path: string, local: boolean, legacy = false) {
	const host = local
		? createHash("sha256")
				.update(`${process.platform}:${hostname()}`)
				.digest("hex")
				.slice(0, 16)
		: "0000000000000000";
	const claim = join(
		path,
		".celox/.generation-locks",
		`${host}-${process.pid}-${randomUUID()}`,
	);
	mkdirSync(claim, { recursive: true });
	if (!legacy) {
		writeFileSync(join(claim, "ticket"), "1");
		writeFileSync(join(claim, "lease"), "");
	}
	return { claim, timestamp: legacy ? claim : join(claim, "lease") };
}

test.each([
	["a changed hostname", false, false],
	["a reused live PID", true, false],
	["a legacy claim interrupted before publishing its ticket", false, true],
])("an expired claim from %s is reclaimed", async (_name, local, legacy) => {
	const path = root();
	const { claim, timestamp } = abandoned(path, local, legacy);
	const old = new Date(Date.now() - 120_000);
	utimesSync(timestamp, old, old);
	const contender = child(
		path,
		`await withSidecarLock(path, () => writeFileSync(join(path, 'recovered'), 'yes'));`,
	);
	await Promise.race([
		contender.exited,
		delay(3000).then(() => {
			throw new Error("Expired claim was not reclaimed");
		}),
	]);
	expect(existsSync(join(path, "recovered"))).toBe(true);
	expect(existsSync(claim)).toBe(false);
});

test("a fresh foreign claim remains exclusive until its lease expires", async () => {
	const path = root();
	const { timestamp } = abandoned(path, false);
	let entered = false;
	const waiting = withSidecarLock(path, () => {
		entered = true;
	});
	await delay(100);
	expect(entered).toBe(false);
	const old = new Date(Date.now() - 120_000);
	utimesSync(timestamp, old, old);
	await waiting;
	expect(entered).toBe(true);
});

test("synchronous generation does not prevent lease renewal", async () => {
	const path = root();
	await withSidecarLock(path, () => {
		const directory = join(path, ".celox/.generation-locks");
		const [claim] = readdirSync(directory);
		const lease = join(directory, claim, "lease");
		const before = statSync(lease).mtimeMs;
		Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 1500);
		expect(statSync(lease).mtimeMs).toBeGreaterThan(before);
	});
});

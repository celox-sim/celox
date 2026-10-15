import { createHash, randomUUID } from "node:crypto";
import {
	mkdirSync,
	readdirSync,
	readFileSync,
	renameSync,
	rmSync,
	statSync,
	writeFileSync,
} from "node:fs";
import { hostname } from "node:os";
import { join } from "node:path";
import { Worker } from "node:worker_threads";
import { SIDECAR_DIR } from "./sidecar.js";

const host = createHash("sha256")
	.update(`${process.platform}:${hostname()}`)
	.digest("hex")
	.slice(0, 16);
const claimPattern = /^([a-f0-9]{16})-([1-9][0-9]*)-([a-f0-9-]{36})$/;
const leaseDuration = 60_000;

function alive(name: string, directory: string): boolean {
	const claim = join(directory, name);
	let updated: number;
	try {
		try {
			updated = statSync(join(claim, "lease")).mtimeMs;
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
			// Also recover claims left by versions without a lease file.
			updated = statSync(claim).mtimeMs;
		}
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === "ENOENT") return false;
		// Windows may deny access while another owner removes its claim.
		// Keep waiting until its disappearance is observable.
		if (deletePending(error)) return true;
		throw error;
	}
	if (Date.now() - updated > leaseDuration) return false;
	const match = claimPattern.exec(name)!;
	// PIDs are meaningful only within the same host and operating system.
	if (match[1] !== host) return true;
	try {
		process.kill(Number(match[2]), 0);
		return true;
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === "ESRCH") return false;
		return true;
	}
}

function deletePending(error: unknown): boolean {
	return (
		process.platform === "win32" &&
		(error as NodeJS.ErrnoException).code === "EPERM"
	);
}

async function ticket(dir: string): Promise<number | undefined> {
	const deadline = Date.now() + 120_000;
	for (;;) {
		try {
			const number = Number(readFileSync(join(dir, "ticket"), "utf8"));
			if (!Number.isSafeInteger(number) || number < 1)
				throw new Error(`Invalid declaration-generation ticket: ${dir}`);
			return number;
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
			if (!deletePending(error) || Date.now() >= deadline) throw error;
			// Do not treat an unreadable published ticket as zero while
			// choosing our priority: that could overtake its current owner.
			await new Promise((resolve) => setTimeout(resolve, 25));
		}
	}
}

/** Serialize catalogue reads and declaration writes across local processes. */
export async function withSidecarLock<T>(
	projectRoot: string,
	run: () => T | Promise<T>,
): Promise<T> {
	const directory = join(projectRoot, SIDECAR_DIR, ".generation-locks");
	mkdirSync(directory, { recursive: true });
	const id = `${host}-${process.pid}-${randomUUID()}`;
	const claim = join(directory, id);
	mkdirSync(claim);
	let heartbeat: Worker | undefined;
	let renewalError: unknown;
	try {
		const lease = join(claim, "lease");
		writeFileSync(lease, "");
		// Native catalogue generation is synchronous. Renew from another thread
		// so it cannot expire a live claim by blocking the main event loop.
		heartbeat = new Worker(
			`const { utimesSync } = require('node:fs');
			const { parentPort, workerData } = require('node:worker_threads');
			const renew = () => {
				const now = new Date();
				utimesSync(workerData, now, now);
			};
			renew();
			parentPort.postMessage('ready');
			setInterval(renew, 1000);`,
			{ eval: true, workerData: lease, execArgv: [] },
		);
		heartbeat.on("error", (error) => {
			renewalError = error;
		});
		await new Promise<void>((resolve, reject) => {
			heartbeat!.once("message", () => resolve());
			heartbeat!.once("error", reject);
		});
		const claims = () =>
			readdirSync(directory).filter(
				(name) => claimPattern.test(name) && name !== id,
			);
		let largest = 0;
		for (const name of claims()) {
			if (alive(name, directory))
				largest = Math.max(largest, (await ticket(join(directory, name))) ?? 0);
		}
		const number = largest + 1;
		// Publish the completed ticket atomically. Its absence means that this
		// process is still choosing, so other contenders must wait for it.
		writeFileSync(join(claim, "choosing"), String(number));
		renameSync(join(claim, "choosing"), join(claim, "ticket"));
		const started = Date.now();
		for (;;) {
			if (renewalError) throw renewalError;
			let waiting = false;
			for (const name of claims()) {
				const other = join(directory, name);
				if (!alive(name, directory)) {
					// Each claim has a unique name: removing an abandoned claim
					// cannot remove a subsequent owner's lock.
					rmSync(other, { recursive: true, force: true });
					continue;
				}
				const priority = await ticket(other);
				if (
					priority === undefined ||
					priority < number ||
					(priority === number && name < id)
				)
					waiting = true;
			}
			if (!waiting) {
				const result = await run();
				if (renewalError) throw renewalError;
				return result;
			}
			if (Date.now() - started > 120_000)
				throw new Error(
					`Timed out waiting for declaration generation in ${projectRoot}`,
				);
			await new Promise((resolve) => setTimeout(resolve, 25));
		}
	} finally {
		await heartbeat?.terminate();
		rmSync(claim, { recursive: true, force: true });
	}
}

import { createHash, randomUUID } from "node:crypto";
import {
	mkdirSync,
	readdirSync,
	readFileSync,
	renameSync,
	rmSync,
	writeFileSync,
} from "node:fs";
import { hostname } from "node:os";
import { join } from "node:path";
import { SIDECAR_DIR } from "./sidecar.js";

const host = createHash("sha256")
	.update(`${process.platform}:${hostname()}`)
	.digest("hex")
	.slice(0, 16);
const claimPattern = /^([a-f0-9]{16})-([1-9][0-9]*)-([a-f0-9-]{36})$/;

function alive(name: string): boolean {
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

function ticket(dir: string): number | undefined {
	try {
		const number = Number(readFileSync(join(dir, "ticket"), "utf8"));
		if (!Number.isSafeInteger(number) || number < 1)
			throw new Error(`Invalid declaration-generation ticket: ${dir}`);
		return number;
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
		return undefined;
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
	try {
		const claims = () =>
			readdirSync(directory).filter(
				(name) => claimPattern.test(name) && name !== id,
			);
		let largest = 0;
		for (const name of claims()) {
			if (alive(name))
				largest = Math.max(largest, ticket(join(directory, name)) ?? 0);
		}
		const number = largest + 1;
		// Publish the completed ticket atomically. Its absence means that this
		// process is still choosing, so other contenders must wait for it.
		writeFileSync(join(claim, "choosing"), String(number));
		renameSync(join(claim, "choosing"), join(claim, "ticket"));
		const started = Date.now();
		for (;;) {
			let waiting = false;
			for (const name of claims()) {
				const other = join(directory, name);
				if (!alive(name)) {
					// Each claim has a unique name: removing a dead process's claim
					// cannot remove a subsequent owner's lock.
					rmSync(other, { recursive: true, force: true });
					continue;
				}
				const priority = ticket(other);
				if (
					priority === undefined ||
					priority < number ||
					(priority === number && name < id)
				)
					waiting = true;
			}
			if (!waiting) return await run();
			if (Date.now() - started > 120_000)
				throw new Error(
					`Timed out waiting for declaration generation in ${projectRoot}`,
				);
			await new Promise((resolve) => setTimeout(resolve, 25));
		}
	} finally {
		rmSync(claim, { recursive: true, force: true });
	}
}

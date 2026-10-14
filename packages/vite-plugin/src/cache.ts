import { createHash } from "node:crypto";
import { type Dirent, readdirSync, readFileSync, statSync } from "node:fs";
import { extname, join, relative } from "node:path";
import { runGenTs } from "./generator.js";
import type { GenTsJsonOutput, TestbenchComponentManifests } from "./types.js";

export class GenTsCache {
	private _data: GenTsJsonOutput | undefined;
	private _sourceKey = "";
	private _configKey = "";
	private _lockfileKey = "";
	private _generatedLockfileKey: string | undefined;
	private _sourceFiles: string[] = [];

	constructor(
		private readonly _projectRoot: string,
		private _testbenchComponents?: TestbenchComponentManifests,
	) {}

	setTestbenchComponents(components: TestbenchComponentManifests): void {
		this._testbenchComponents = components;
		this.invalidate();
	}

	/** Refresh when a source or generator configuration file changes. */
	get(): GenTsJsonOutput {
		const key = this.computeSourceKey();
		const configKey = this.computeConfigKey(["Veryl.toml", "celox.toml"]);
		const lockfileKey = this.computeContentKey("Veryl.lock");
		if (
			this._data &&
			this._sourceKey === key &&
			this._configKey === configKey &&
			this._lockfileKey === lockfileKey
		) {
			return this._data;
		}
		this._data = runGenTs(this._projectRoot, this._testbenchComponents);
		const generatedLockfileKey = this.computeContentKey("Veryl.lock");
		if (
			generatedLockfileKey !== lockfileKey &&
			generatedLockfileKey !== "missing"
		)
			this._generatedLockfileKey = generatedLockfileKey;
		else if (generatedLockfileKey !== this._generatedLockfileKey)
			this._generatedLockfileKey = undefined;
		// Module loading may perform the conservative refresh before the watch
		// event arrives. Retain its candidate while lockfile contents match.
		this._sourceKey = key;
		// Preserve every pre-generation fingerprint: an external process can
		// change even Veryl.lock after the generator has read it. A lockfile
		// written by generation also triggers one conservative refresh.
		this._configKey = configKey;
		this._lockfileKey = lockfileKey;
		return this._data;
	}

	/** A possible generator write still needs a conservative refresh in get(). */
	get hasPendingLockfileWrite(): boolean {
		return (
			this._generatedLockfileKey !== undefined &&
			this.computeContentKey("Veryl.lock") === this._generatedLockfileKey
		);
	}

	/** Source paths captured during the most recent cache lookup. */
	get sourceFiles(): readonly string[] {
		return this._sourceFiles;
	}

	/** Force invalidation — next `get()` will re-run the generator. */
	invalidate(): void {
		this._sourceKey = "";
		this._data = undefined;
	}

	private computeConfigKey(names: readonly string[]): string {
		return JSON.stringify(
			names.map((name) => [name, this.computeContentKey(name)]),
		);
	}

	private computeContentKey(name: string): string {
		try {
			// External edits may preserve metadata; generator rewrites may change
			// metadata without changing dependency resolution. Compare contents.
			return createHash("sha256")
				.update(readFileSync(join(this._projectRoot, name)))
				.digest("hex");
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
			return "missing";
		}
	}

	/**
	 * Build a key from every .veryl file's path, mtime, and size.
	 * This stays cheap — only stats, no file reads — while detecting changes to
	 * older files whose mtime does not exceed the newest file in the project.
	 */
	private computeSourceKey(): string {
		const files: Array<[path: string, mtimeMs: number, size: number]> = [];
		const sourceFiles: string[] = [];
		this.walkVerylFiles(this._projectRoot, (path, mtimeMs, size) => {
			files.push([relative(this._projectRoot, path), mtimeMs, size]);
			sourceFiles.push(path);
		});
		this._sourceFiles = sourceFiles;
		files.sort(([a], [b]) => a.localeCompare(b));
		return JSON.stringify(files);
	}

	private walkVerylFiles(
		dir: string,
		cb: (path: string, mtimeMs: number, size: number) => void,
	): void {
		let entries: Dirent<string>[] | undefined;
		try {
			entries = readdirSync(dir, { withFileTypes: true });
		} catch {
			return;
		}
		for (const entry of entries) {
			const full = join(dir, entry.name);
			if (entry.isDirectory()) {
				// Skip common non-source directories
				if (
					entry.name === "node_modules" ||
					entry.name === "target" ||
					entry.name === ".git" ||
					entry.name === "dist"
				) {
					continue;
				}
				this.walkVerylFiles(full, cb);
			} else if (extname(entry.name) === ".veryl") {
				try {
					const stat = statSync(full);
					cb(full, stat.mtimeMs, stat.size);
				} catch {
					// file may have been deleted
				}
			}
		}
	}
}

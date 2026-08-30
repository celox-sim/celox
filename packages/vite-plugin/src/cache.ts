import { type Dirent, readdirSync, statSync } from "node:fs";
import { extname, join, relative } from "node:path";
import { runGenTs } from "./generator.js";
import type { GenTsJsonOutput, TestbenchComponentManifests } from "./types.js";

export class GenTsCache {
	private _data: GenTsJsonOutput | undefined;
	private _sourceKey = "";

	constructor(
		private readonly _projectRoot: string,
		private _testbenchComponents?: TestbenchComponentManifests,
	) {}

	setTestbenchComponents(components: TestbenchComponentManifests): void {
		this._testbenchComponents = components;
		this.invalidate();
	}

	/** Get cached data, refreshing if any .veryl file has changed. */
	get(): GenTsJsonOutput {
		const key = this.computeSourceKey();
		if (this._data && this._sourceKey === key) {
			return this._data;
		}
		this._data = runGenTs(this._projectRoot, this._testbenchComponents);
		this._sourceKey = key;
		return this._data;
	}

	/** Force invalidation — next `get()` will re-run the generator. */
	invalidate(): void {
		this._sourceKey = "";
		this._data = undefined;
	}

	/**
	 * Build a key from every .veryl file's path, mtime, and size.
	 * This stays cheap — only stats, no file reads — while detecting changes to
	 * older files whose mtime does not exceed the newest file in the project.
	 */
	private computeSourceKey(): string {
		const files: Array<[path: string, mtimeMs: number, size: number]> = [];
		this.walkVerylFiles(this._projectRoot, (path, mtimeMs, size) => {
			files.push([relative(this._projectRoot, path), mtimeMs, size]);
		});
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

import { readdirSync, statSync } from "node:fs";
import { join, extname } from "node:path";
import type { GenTsJsonOutput } from "./types.js";
import { runGenTs } from "./generator.js";

export class GenTsCache {
  private _data: GenTsJsonOutput | undefined;
  private _mtimeKey = "";

  constructor(private readonly _projectRoot: string) {}

  /** Get cached data, refreshing if any .veryl file has changed. */
  get(): GenTsJsonOutput {
    const key = this.computeMtimeKey();
    if (this._data && this._mtimeKey === key) {
      return this._data;
    }
    this._data = runGenTs(this._projectRoot);
    this._mtimeKey = key;
    return this._data;
  }

  /** Force invalidation — next `get()` will re-run the generator. */
  invalidate(): void {
    this._mtimeKey = "";
    this._data = undefined;
  }

  /**
   * Build a key from the max mtime of all .veryl files in the project.
   * This is intentionally cheap — only stats, no file reads.
   */
  private computeMtimeKey(): string {
    let maxMtime = 0;
    let count = 0;
    this.walkVerylFiles(this._projectRoot, (mtimeMs) => {
      if (mtimeMs > maxMtime) maxMtime = mtimeMs;
      count++;
    });
    return `${count}:${maxMtime}`;
  }

  private walkVerylFiles(
    dir: string,
    cb: (mtimeMs: number) => void,
  ): void {
    let entries;
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
          cb(statSync(full).mtimeMs);
        } catch {
          // file may have been deleted
        }
      }
    }
  }
}

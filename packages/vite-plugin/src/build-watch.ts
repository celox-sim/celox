import { mkdtempSync, rmSync, watch, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { normalizePath } from "vite";

/** Bridge catalogue additions without giving the bundler a recursive watch root. */
export function watchBuildCatalogue(
	root: string,
	outDir: string | undefined,
	onError: (error: Error) => void,
) {
	const directory = mkdtempSync(join(tmpdir(), "celox-build-watch-"));
	const signal = join(directory, "catalogue");
	writeFileSync(signal, "0");
	const excluded = [".celox", "target", "node_modules", ".git"].map((name) =>
		normalizePath(resolve(root, name)),
	);
	excluded.push(normalizePath(directory));
	if (outDir) excluded.push(normalizePath(outDir));
	let watched = new Set<string>();
	let sequence = 0;
	let timer: ReturnType<typeof setTimeout> | undefined;
	let watcher: ReturnType<typeof watch>;
	try {
		watcher = watch(root, { recursive: true }, (_event, filename) => {
			if (filename) {
				const file = normalizePath(resolve(root, filename.toString()));
				if (
					excluded.some((path) => file === path || file.startsWith(`${path}/`))
				)
					return;
				// Existing inputs are already watched by Rollup/Rolldown. The
				// bridge is for newly added inputs (including directory moves).
				if (watched.has(file)) return;
			}
			if (timer) return;
			timer = setTimeout(() => {
				timer = undefined;
				try {
					writeFileSync(signal, String(++sequence));
				} catch (error) {
					onError(error as Error);
				}
			}, 25);
		});
		watcher.on("error", onError);
	} catch (error) {
		rmSync(directory, { recursive: true, force: true });
		throw error;
	}
	return {
		signal,
		setWatched(files: string[]) {
			watched = new Set(files.map(normalizePath));
		},
		close() {
			watcher.close();
			if (timer) clearTimeout(timer);
			rmSync(directory, { recursive: true, force: true });
		},
	};
}

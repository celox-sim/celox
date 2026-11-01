import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
	copyFileSync,
	existsSync,
	mkdirSync,
	mkdtempSync,
	readFileSync,
	rmSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { checkVerylLane } from "./check-veryl-lane.mjs";
import { useVerylHead, verylDependencyNames } from "./use-veryl-head.mjs";

const revision = "0123456789abcdef0123456789abcdef01234567";
const previousRevision = "fedcba9876543210fedcba9876543210fedcba98";
const workflow = readFileSync(
	new URL("../.github/workflows/veryl-head.yml", import.meta.url),
	"utf8",
);
const step = workflow.split(
	"      - name: Pin Veryl dependencies to upstream HEAD\n",
)[1];
assert.ok(step, "Missing Veryl update step");
const script = step.match(/        run: \|\n((?:          [^\n]*\n|\n)*)/)[1];
const stableManifest =
	"[workspace.dependencies]\n" +
	verylDependencyNames.map((name) => `${name} = "0.22.0"\n`).join("");

function versions(version) {
	return Object.fromEntries(
		verylDependencyNames.map((name) => [name, version]),
	);
}

function lockfile(lockedRevision, version) {
	return (
		"version = 4\n\n" +
		verylDependencyNames
			.map(
				(name) =>
					`[[package]]\nname = "${name}"\nversion = "${version}"\nsource = "git+https://github.com/veryl-lang/veryl.git?rev=${lockedRevision}#${lockedRevision}"\n`,
			)
			.join("\n")
	);
}

// Run the workflow's actual shell and real manifest writer. Only the remote
// revision lookup and Cargo resolver are replaced with offline fixtures.
function runUpdate(manifest, initialLock, resolvedLock) {
	const directory = mkdtempSync(join(tmpdir(), "celox-veryl-roll-"));
	try {
		mkdirSync(join(directory, "scripts"));
		copyFileSync(
			new URL("./use-veryl-head.mjs", import.meta.url),
			join(directory, "scripts/use-veryl-head.mjs"),
		);
		writeFileSync(join(directory, "Cargo.toml"), manifest);
		writeFileSync(join(directory, "original.toml"), manifest);
		writeFileSync(join(directory, "Cargo.lock"), initialLock);
		writeFileSync(join(directory, "resolved.lock"), resolvedLock);
		const result = spawnSync(
			"bash",
			[
				"-e",
				"-o",
				"pipefail",
				"-c",
				`
git() {
  case "$1" in
    ls-remote) printf '%s\\trefs/heads/master\\n' "$ROLL_REVISION" ;;
    diff) cmp -s Cargo.toml original.toml ;;
    *) return 99 ;;
  esac
}
cargo() {
  printf '%s\\n' "$*" > cargo-arguments
  cp resolved.lock Cargo.lock
}
${script}`,
			],
			{
				cwd: directory,
				env: {
					...process.env,
					ROLL_REVISION: revision,
					GITHUB_ENV: join(directory, "env"),
				},
				encoding: "utf8",
			},
		);
		assert.ifError(result.error);
		assert.equal(result.status, 0, result.stderr);
		const updated = readFileSync(join(directory, "Cargo.toml"), "utf8");
		assert.equal(checkVerylLane(updated, "head"), revision);
		assert.equal(
			readFileSync(join(directory, "env"), "utf8"),
			`VERYL_REVISION=${revision}\n`,
		);
		return {
			manifest: updated,
			cargo: existsSync(join(directory, "cargo-arguments"))
				? readFileSync(join(directory, "cargo-arguments"), "utf8").trim()
				: undefined,
		};
	} finally {
		rmSync(directory, { recursive: true, force: true });
	}
}

test("the scheduled roll restores resolved versions after an upstream version bump", () => {
	const manifest = useVerylHead(
		stableManifest,
		previousRevision,
		versions("0.22.0"),
	);
	const result = runUpdate(
		manifest,
		lockfile(previousRevision, "0.22.0"),
		lockfile(revision, "0.23.0"),
	);
	assert.equal(result.cargo, "update -p veryl-analyzer");
	assert.equal(
		result.manifest,
		useVerylHead(stableManifest, revision, versions("0.23.0")),
	);
});

test("an already complete roll does not resolve the dependency graph again", () => {
	const manifest = useVerylHead(stableManifest, revision, versions("0.23.0"));
	const result = runUpdate(
		manifest,
		lockfile(revision, "0.23.0"),
		"must not be used",
	);
	assert.equal(result.cargo, undefined);
	assert.equal(result.manifest, manifest);
});

test("a stable lane can roll directly to a new upstream version", () => {
	const result = runUpdate(
		stableManifest,
		"version = 4\n",
		lockfile(revision, "0.23.0"),
	);
	assert.equal(result.cargo, "update -p veryl-analyzer");
	assert.equal(
		result.manifest,
		useVerylHead(stableManifest, revision, versions("0.23.0")),
	);
});

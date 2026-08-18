import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const guard = fileURLToPath(
  new URL("./check-sync-branch-head.sh", import.meta.url),
);

test("sync branch guard preserves unresolved branch work", (t) => {
  const repository = mkdtempSync(join(tmpdir(), "celox-sync-branch-head-"));
  t.after(() => rmSync(repository, { recursive: true, force: true }));

  const git = (...args) =>
    execFileSync("git", args, { cwd: repository, encoding: "utf8" }).trim();
  const commitFile = (path, contents, message) => {
    writeFileSync(join(repository, path), `${contents}\n`);
    git("add", path);
    git("commit", "--quiet", "-m", message);
  };
  const check = (sync, master, develop) =>
    spawnSync(guard, [sync, master, develop], {
      cwd: repository,
      encoding: "utf8",
    });

  git("init", "--quiet", "--initial-branch=master");
  git("config", "user.name", "Sync Guard Test");
  git("config", "user.email", "sync-guard@example.com");

  commitFile("common.txt", "base", "base");
  git("branch", "develop");
  commitFile("master.txt", "master-1", "master one");
  const oldMaster = git("rev-parse", "HEAD");
  commitFile("master.txt", "master-2", "master two");
  const master = git("rev-parse", "HEAD");

  git("switch", "--quiet", "develop");
  commitFile("develop.txt", "develop", "develop work");
  const develop = git("rev-parse", "HEAD");

  // A stale raw-master fallback is safe to replace with the newer master.
  assert.equal(check(oldMaster, master, develop).status, 0);

  // A merge containing both sides represents an unresolved synchronization
  // head, including a human conflict resolution, so it must be preserved.
  git("switch", "--quiet", "-c", "resolved-sync", develop);
  git("merge", "--quiet", "--no-ff", oldMaster, "-m", "resolve sync conflicts");
  const resolved = git("rev-parse", "HEAD");
  const blockedResolution = check(resolved, master, develop);
  assert.equal(blockedResolution.status, 1);
  assert.match(
    blockedResolution.stderr,
    /Refusing to replace synchronization branch head/,
  );

  // Once the synchronization head has landed in develop, it is disposable.
  git("branch", "--force", "develop", resolved);
  assert.equal(check(resolved, master, resolved).status, 0);

  // Arbitrary branch work based on master must also survive automation.
  git("switch", "--quiet", "-c", "manual-work", oldMaster);
  commitFile("manual.txt", "preserve-me", "manual sync work");
  const manual = git("rev-parse", "HEAD");
  assert.equal(check(manual, master, develop).status, 1);
});

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

test("xezim stays release/content locked with Updatecli as the only update owner", () => {
  const flake = readFileSync(new URL("../flake.nix", import.meta.url), "utf8");
  const version = flake.match(/github:aionhw\/xezim\/(\d+\.\d+\.\d+)"/)[1];
  const lock = JSON.parse(readFileSync(new URL("../flake.lock", import.meta.url))).nodes.xezim;
  assert.equal(lock.original.ref, version);
  assert.equal(lock.original.rev, undefined);
  assert.match(lock.locked.rev, /^[a-f0-9]{40}$/);
  assert.match(lock.locked.narHash, /^sha256-/);
  const cargo = readFileSync(new URL("../nix/xezim/Cargo.lock", import.meta.url), "utf8");
  assert.ok(cargo.includes(`name = "xezim"\nversion = "${version}"`));
  const config = JSON.parse(readFileSync(new URL("../renovate.json", import.meta.url)));
  assert.ok(!config.customManagers.some(manager => manager.depNameTemplate === "xezim"));
  const nixDisableRule = config.packageRules.find(rule => rule.matchManagers?.includes("nix") && rule.enabled === false);
  assert.deepEqual(nixDisableRule.matchDepNames, ["!rust-overlay"]);
});
